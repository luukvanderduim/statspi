use atspi::{
    connection::AccessibilityConnection,
    events::{
        document::DocumentEvents, focus::FocusEvents, keyboard::KeyboardEvents, mouse::MouseEvents,
        object::ObjectEvents, terminal::TerminalEvents, window::WindowEvents, CacheEvents,
        EventListenerEvents,
    },
};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Cell, Row, Sparkline, Table},
    Frame, Terminal,
};
use std::{
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_stream::StreamExt;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Tick duration in milliseconds
const TICK_MS: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub struct AtspiEventCount {
    counter: AtomicU64,
}

impl AtspiEventCount {
    pub fn new() -> AtspiEventCount {
        let counter = AtomicU64::new(0);
        AtspiEventCount { counter }
    }

    /// Get-and-reset of the counter.
    pub fn reset(&self) -> u64 {
        self.counter.swap(0, Ordering::SeqCst)
    }

    /// Increment counter by one.
    pub fn plus_one(&self) {
        let _ = self.counter.fetch_add(1, Ordering::SeqCst);
    }
}

struct App {
    // The counters
    tick_counter: AtspiEventCount,
    secs_counter: AtspiEventCount,

    // The data stores
    tick_data: Mutex<Vec<u64>>,
    secs_data: Mutex<Vec<u64>>,
    total: AtomicU64,
}

impl App {
    fn new() -> App {
        // Init counters
        let tick_counter = AtspiEventCount::new();
        let secs_counter = AtspiEventCount::new();

        // Init data stores
        let tick_data = Mutex::new(vec![0; 200]);
        let secs_data = Mutex::new(Vec::with_capacity(1800)); // 30 minutes
        let total = AtomicU64::new(0);

        App {
            tick_counter,
            secs_counter,
            tick_data,
            secs_data,
            total,
        }
    }

    /// Update the per-tick data store and reset the per-tick counter.
    fn on_tick(&self) {
        // Get current value and reset the per-tick counter.
        let value = self.tick_counter.reset();

        // A circular buffer of tick data:
        let mut tick_data = self.tick_data.lock().unwrap();

        tick_data.pop();
        tick_data.insert(0, value);
        self.total.fetch_add(value, Ordering::SeqCst);
    }

    /// Update the per-second data store and reset the per-second counter.
    /// Also update the total.
    fn on_second(&self) {
        // Get current value and reset the per-second counter.
        let value = self.secs_counter.reset();

        // Per second data:
        let mut secs_data = self.secs_data.lock().unwrap();
        secs_data.push(value);

        // Update total
        self.total.fetch_add(value, Ordering::SeqCst);
    }
}

async fn atspi_setup_connection() -> Result<AccessibilityConnection> {
    // Get a connection to the AT-SPI D-Bus service
    let atspi: AccessibilityConnection = AccessibilityConnection::open().await?;

    // Register for events
    atspi.register_event::<MouseEvents>().await?;
    atspi.register_event::<KeyboardEvents>().await?;
    atspi.register_event::<FocusEvents>().await?;
    atspi.register_event::<WindowEvents>().await?;
    atspi.register_event::<DocumentEvents>().await?;
    atspi.register_event::<ObjectEvents>().await?;
    atspi.register_event::<TerminalEvents>().await?;
    atspi.register_event::<CacheEvents>().await?;
    atspi.register_event::<EventListenerEvents>().await?;

    Ok(atspi)
}

#[tokio::main]
async fn main() -> Result<()> {
    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create the app's state
    let app = Arc::new(App::new());

    // Obtain a stream of AT-SPI events
    let a11y_bus = atspi_setup_connection().await?;
    let mut events = a11y_bus.event_stream();

    // Event -> update counters.
    let app_clone = Arc::clone(&app);
    tokio::spawn(async move {
        while let Some(Ok(_event)) = events.next().await {
            app_clone.tick_counter.plus_one();
            app_clone.secs_counter.plus_one();
        }
    });

    let app_clone = Arc::clone(&app);
    // Each second, update the secs_data store and reset the counter.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            app_clone.on_second();
        }
    });

    let app_clone = Arc::clone(&app);
    let res = run_app(&mut terminal, app_clone, TICK_MS);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: Arc<App>,
    tick_rate: Duration,
) -> io::Result<()> {
    let mut last_tick = Instant::now();

    let app = Arc::clone(&app);
    loop {
        let app_clone = Arc::clone(&app);
        terminal.draw(|f| ui(f, app_clone))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));
        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if let KeyCode::Char('q') = key.code {
                    return Ok(());
                }
            }
        }

        let app_clone = Arc::clone(&app);
        if last_tick.elapsed() >= tick_rate {
            app_clone.on_tick();
            last_tick = Instant::now();
        }
    }
}

fn ui<B: Backend>(f: &mut Frame<B>, app: Arc<App>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Percentage(67), Constraint::Percentage(33)].as_ref())
        .split(f.size());

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(67), Constraint::Percentage(33)].as_ref())
        .split(chunks[1]);

    let tick_data = app.tick_data.lock().unwrap();

    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .title("AT-SPI2 bus activity")
                .border_style(Style::default().fg(Color::LightBlue))
                .borders(Borders::ALL),
        )
        .data(&tick_data)
        .style(Style::default().fg(Color::Yellow));

    let secs_data = app.secs_data.lock().unwrap();

    // Compute the current, max, avg and total values.
    let current: Cell = Cell::from(secs_data.last().unwrap_or(&0).to_string());
    let max: Cell = Cell::from(secs_data.iter().max().unwrap_or(&0).to_string());
    let avg: Cell = Cell::from(
        if secs_data.len() > 0 {
            secs_data.iter().sum::<u64>() / secs_data.len() as u64
        } else {
            0
        }
        .to_string(),
    );
    let total: Cell = Cell::from(app.total.load(Ordering::SeqCst).to_string());

    let column_data = vec![current, max, avg, total];

    let table = Table::new(vec![
        Row::new(vec!["Current", "Maximum", "Average", "Total"])
            .bottom_margin(1)
            .style(Style::default().fg(Color::LightYellow)),
        Row::new(column_data),
    ])
    .style(Style::default().fg(Color::LightYellow))
    .widths(&[
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(12),
    ])
    .column_spacing(2)
    .block(
        Block::default()
            .title("Real-time figures AT-SPI2 bus:")
            .border_style(Style::default().fg(Color::LightYellow))
            .borders(Borders::ALL),
    );

    f.render_widget(sparkline, chunks[0]);
    f.render_widget(table, bottom[0]);
}

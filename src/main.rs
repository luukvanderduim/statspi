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
use std::{
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_stream::StreamExt;
use tui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Sparkline},
    Frame, Terminal,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Tick duration in milliseconds
const TICK_MS: u64 = 100;

#[derive(Clone, Debug, Default)]
pub struct AtspiEventCount {
    counter: Arc<AtomicU64>,
    counter_second: Arc<AtomicU64>,
}

impl AtspiEventCount {
    pub fn new() -> AtspiEventCount {
        let counter = Arc::new(AtomicU64::new(0));
        let counter_second = Arc::new(AtomicU64::new(0));
        AtspiEventCount {
            counter,
            counter_second,
        }
    }
}

impl Iterator for AtspiEventCount {
    type Item = u64;
    fn next(&mut self) -> Option<u64> {
        Some(self.counter.swap(0, Ordering::SeqCst))
    }
}

struct App {
    signal: AtspiEventCount,
    data1: Vec<u64>,
    data_per_second: Arc<Mutex<Vec<u64>>>,
    total: Arc<AtomicU64>,
}

impl App {
    fn new() -> App {
        let signal = AtspiEventCount::new();
        let data1: Vec<u64> = vec![0; 200];
        let data_per_second: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::with_capacity(1800)));

        App {
            signal,
            data1,
            data_per_second,
            total: Arc::new(AtomicU64::new(0)),
        }
    }

    fn on_tick(&mut self) {
        let value = self.signal.next().unwrap();
        self.data1.pop();
        self.data1.insert(0, value);
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

    // Obtain a stream of AT-SPI events
    let conn = atspi_setup_connection().await?;
    let mut events = conn.event_stream();

    // Monitor the stream of events, update the state of App.
    let app = App::new();
    let counter = app.signal.counter.clone();
    let counter_second = app.signal.counter_second.clone();

    tokio::spawn(async move {
        while let Some(Ok(_event)) = events.next().await {
            counter.fetch_add(1, Ordering::SeqCst);
            counter_second.fetch_add(1, Ordering::SeqCst);
        }
    });

    let counter_second = app.signal.counter_second.clone();
    let data_per_second = app.data_per_second.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut data = data_per_second.lock().unwrap();
            data.push(counter_second.swap(0, Ordering::SeqCst));
        }
    });

    let tick_rate = Duration::from_millis(TICK_MS);

    let res = run_app(&mut terminal, app, tick_rate);

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
    mut app: App,
    tick_rate: Duration,
) -> io::Result<()> {
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|f| ui(f, &app))?;

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
        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
}

fn ui<B: Backend>(f: &mut Frame<B>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Length(10), Constraint::Min(0)].as_ref())
        .split(f.size());
    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .title("Last 200 D-Bus Accessibility (AT-SPI2) events")
                .borders(Borders::LEFT | Borders::RIGHT),
        )
        .data(&app.data1)
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(sparkline, chunks[0]);
}

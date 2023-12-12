use atspi::{
    connection::AccessibilityConnection,
    events::{
        document::DocumentEvents, focus::FocusEvents, keyboard::KeyboardEvents, mouse::MouseEvents,
        object::ObjectEvents, terminal::TerminalEvents, window::WindowEvents, AddAccessibleEvent,
        Event as AtspiEvent, EventListenerDeregisteredEvent, EventListenerRegisteredEvent,
        LegacyAddAccessibleEvent, RemoveAccessibleEvent,
    },
};
use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Cell, ListItem, Row, Sparkline, Table},
    Frame, Terminal,
};
use std::{
    collections::HashSet,
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_stream::StreamExt;
use zbus::zvariant::ObjectPath;

mod bus;
use bus::Servers;

mod terminal;
use terminal::{restore_terminal, setup_terminal};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const TICK_MS: Duration = Duration::from_millis(100);

// The path to the root-Accessible object on the AT-SPI2 bus
const ACCESSIBLE_ROOT_PATH: ObjectPath<'static> =
    ObjectPath::from_static_str_unchecked("/org/a11y/atspi/accessible/root");

#[derive(Debug, Default)]
pub struct Counter {
    counter: AtomicU64,
}

impl Counter {
    pub fn new() -> Counter {
        Counter {
            counter: AtomicU64::new(0),
        }
    }

    /// Get-and-reset of the counter.
    pub fn reset(&self) -> u64 {
        self.counter.swap(0, Ordering::AcqRel)
    }

    /// Increment counter by one.
    pub fn incr(&self) {
        let _ = self.counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment counter by `value`.
    /// Returns the previous value.
    pub fn add(&self, value: u64) -> u64 {
        self.counter.fetch_add(value, Ordering::Relaxed)
    }

    /// Read the counter.
    pub fn load(&self) -> u64 {
        self.counter.load(Ordering::Acquire)
    }

    /// Set the counter.
    /// Returns the previous value.
    pub fn set(&self, value: u64) -> u64 {
        self.counter.swap(value, Ordering::AcqRel)
    }
}

#[derive(Debug, Default)]
struct ScoreBoard {
    // Categorized counters
    mouse: Counter,
    keyboard: Counter,
    focus: Counter,
    window: Counter,
    document: Counter,
    object: Counter,
    terminal: Counter,
    cache: Counter,
    listeners: Counter,
    available: Counter,
    other_event: Counter,
    error: Counter,

    // Global counters
    tick_counter: Counter,
    secs_counter: Counter,
    total_seconds: Counter,
    total: Counter,
}

#[derive(Debug, Default)]
struct RtStats {
    pub rate: Counter,
    pub max: Counter,
    pub mean: Counter,
}

struct App {
    // The bus servers
    servers: Servers,

    // Keeping the score
    tally: ScoreBoard,

    // Error set
    error_set: Arc<Mutex<HashSet<String>>>,

    // Tick/secs stats
    rt_stats: RtStats,

    // The counter data stores
    tick_data: Mutex<Vec<u64>>,
    secs_data: Mutex<Vec<u64>>,
}

impl App {
    async fn new() -> Result<App> {
        // Get a connection to the AT-SPI D-Bus service, without registering for events.
        let a11y_conn = atspi::connection::AccessibilityConnection::new().await?;

        // Get the bus servers
        let servers = Servers::new(a11y_conn.connection()).await?;

        // Init counters
        let tally = ScoreBoard::default();

        // error map
        let error_set = Arc::new(Mutex::new(HashSet::new()));

        // Init rate stats
        let rt_stats = RtStats::default();

        // Init counter data stores
        let tick_data = Mutex::new(vec![0; 200]);
        let secs_data = Mutex::new(Vec::with_capacity(1800)); // 30 minutes

        Ok(App {
            servers,
            tally,
            rt_stats,
            tick_data,
            secs_data,
            error_set,
        })
    }

    // Event -> update counters.
    fn on_event(&self, event: Result<AtspiEvent>) {
        match event {
            Ok(AtspiEvent::Mouse(_)) => self.tally.mouse.incr(),
            Ok(AtspiEvent::Keyboard(_)) => self.tally.keyboard.incr(),
            Ok(AtspiEvent::Focus(_)) => self.tally.focus.incr(),
            Ok(AtspiEvent::Window(_)) => self.tally.window.incr(),
            Ok(AtspiEvent::Document(_)) => self.tally.document.incr(),
            Ok(AtspiEvent::Object(_)) => self.tally.object.incr(),
            Ok(AtspiEvent::Terminal(_)) => self.tally.terminal.incr(),
            Ok(AtspiEvent::Cache(_)) => self.tally.cache.incr(),
            Ok(AtspiEvent::Listener(_)) => self.tally.listeners.incr(),
            Ok(AtspiEvent::Available(_)) => self.tally.available.incr(),
            Ok(_) => self.tally.other_event.incr(),
            Err(e) => {
                self.tally.error.incr();
                let msg = format!("{e}");
                let mut set = self.error_set.lock().unwrap();
                if !set.contains(&msg) {
                    set.insert(msg);
                }
            }
        }
        self.tally.tick_counter.incr();
        self.tally.secs_counter.incr();
        self.tally.total.incr();
    }

    /// Update the per-tick data store and reset the per-tick counter.
    fn on_tick(&self) {
        // Get current value and reset the per-tick counter.
        let value = self.tally.tick_counter.reset();

        // A circular buffer of tick data:
        let mut tick_data = self.tick_data.lock().unwrap();
        tick_data.pop();
        tick_data.insert(0, value);
    }

    /// Update the per-second data store and reset the per-second counter.
    fn on_second(&self) {
        // Get current value and reset the per-second counter.
        let value = self.tally.secs_counter.reset();

        if self.rt_stats.max.load() < value {
            self.rt_stats.max.set(value);
        }

        self.rt_stats.rate.set(value);
        self.tally.total_seconds.add(value);

        // Per second data:
        let mut data = self.secs_data.lock().unwrap();
        data.push(value);
        let len = data.len();
        let mean = self.tally.total_seconds.load() / len as u64;
        self.rt_stats.mean.set(mean);
    }
}

async fn setup_atspi() -> Result<AccessibilityConnection> {
    // Get a connection to the AT-SPI D-Bus service
    let atspi: AccessibilityConnection = AccessibilityConnection::new().await?;

    // Register for events
    atspi.register_event::<MouseEvents>().await?;
    atspi.register_event::<KeyboardEvents>().await?;
    atspi.register_event::<FocusEvents>().await?;
    atspi.register_event::<WindowEvents>().await?;
    atspi.register_event::<DocumentEvents>().await?;
    atspi.register_event::<ObjectEvents>().await?;
    atspi.register_event::<TerminalEvents>().await?;

    atspi.register_event::<AddAccessibleEvent>().await?;
    atspi.register_event::<LegacyAddAccessibleEvent>().await?;
    atspi.register_event::<RemoveAccessibleEvent>().await?;
    atspi
        .register_event::<EventListenerDeregisteredEvent>()
        .await?;
    atspi
        .register_event::<EventListenerRegisteredEvent>()
        .await?;

    Ok(atspi)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Create the app's state
    let app = Arc::new(App::new().await.expect("creation of app-state"));

    // Setup tracing
    #[cfg(feature = "tracing")]
    console_subscriber::init();

    // Obtain a connection for events.
    let atspi_conn = setup_atspi().await?;
    let mut events = atspi_conn.event_stream();

    // Trigger counters.
    let app_clone = Arc::clone(&app);
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            app_clone.on_event(event.map_err(Into::into))
        }

        // The event stream has ended.
        tracing::info!("Event stream ended");
    });

    let app_clone = Arc::clone(&app);
    tokio::spawn(async move {
        let mut each_second = tokio::time::interval(Duration::from_secs(1));

        loop {
            each_second.tick().await;
            app_clone.on_second();
        }
    });

    // Ping bus servers 2s. -> acquire response time.
    let app_clone = Arc::clone(&app);
    tokio::spawn(async move {
        let mut in_between = tokio::time::interval(Duration::from_millis(20));
        let mut every_other_second = tokio::time::interval(Duration::from_secs(2));

        loop {
            let app_clone = Arc::clone(&app_clone);
            every_other_second.tick().await;

            for server in app_clone.servers.bus.iter() {
                in_between.tick().await;

                let Ok(mut guard) = server.try_lock() else {
                    continue;
                };

                if let Some(dur) = guard.acquire_rtt().await {
                    guard.update_rtt_stats(dur);
                }
            }
        }
    });

    // setup terminal
    let mut terminal = setup_terminal().expect("setup terminal");

    let app_clone = Arc::clone(&app);
    let res = run_app(&mut terminal, app_clone, TICK_MS);

    // restore terminal
    restore_terminal(&mut terminal)?;

    if let Err(err) = res {
        tracing::error!("Error: {err}");
    }

    Ok(())
}

/// Returns the remaining time until the next tick, or zero if the next tick is overdue.
fn get_remaining_tick_time(tick_dur: Duration, last_tick: Instant) -> Duration {
    tick_dur
        .checked_sub(last_tick.elapsed())
        .unwrap_or_else(|| Duration::from_secs(0))
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: Arc<App>,
    tick_dur: Duration,
) -> io::Result<()> {
    let mut last_tick = Instant::now();

    loop {
        let app_clone = Arc::clone(&app);
        terminal.draw(|f| ui(f, app_clone))?;

        let timeout = get_remaining_tick_time(tick_dur, last_tick);

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if let KeyCode::Char('q') = key.code {
                    return Ok(());
                }
            }
        }

        let app_clone = Arc::clone(&app);
        if last_tick.elapsed() >= tick_dur {
            app_clone.on_tick();
            last_tick = Instant::now();
        }
    }
}

fn ui(f: &mut Frame, app: Arc<App>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(0)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
        .split(f.size());

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(33), Constraint::Percentage(67)].as_ref())
        .split(chunks[1]);

    let bottom_left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)].as_ref())
        .split(bottom[0]);

    let bottom_right = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
        .split(bottom[1]);

    let tick_data = app.tick_data.lock().unwrap();

    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .title("AT-SPI2 signal monitor")
                .border_style(Style::default().fg(Color::LightBlue))
                .border_type(ratatui::widgets::BorderType::Rounded)
                .borders(Borders::ALL),
        )
        .data(tick_data.as_slice())
        .style(Style::default().fg(Color::Yellow));

    // Rates: current, max, mean, total
    let rate = Cell::from(app.rt_stats.rate.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );
    let max = Cell::from(app.rt_stats.max.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );
    let mean = Cell::from(app.rt_stats.mean.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );
    let total = Cell::from(app.tally.total.load().to_string()).style(
        Style::default()
            .fg(Color::LightMagenta)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED),
    );

    let keyboard = Cell::from(app.tally.keyboard.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let mouse = Cell::from(app.tally.mouse.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let focus = Cell::from(app.tally.focus.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let window = Cell::from(app.tally.window.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let object = Cell::from(app.tally.object.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let document = Cell::from(app.tally.document.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let terminal = Cell::from(app.tally.terminal.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let available = Cell::from(app.tally.available.load().to_string()).style(
        Style::default()
            .fg(Color::LightGreen)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let listeners = Cell::from(app.tally.listeners.load().to_string()).style(
        Style::default()
            .fg(Color::LightGreen)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let cache = Cell::from(app.tally.cache.load().to_string()).style(
        Style::default()
            .fg(Color::LightBlue)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let other_event = Cell::from(app.tally.other_event.load().to_string()).style(
        Style::default()
            .fg(Color::LightRed)
            .bg(Color::Black)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let error = Cell::from(app.tally.error.load().to_string()).style(
        Style::default()
            .fg(Color::White)
            .bg(Color::Red)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );

    let column_data = [rate, max, mean, total];
    let event_col1 = [keyboard, mouse, focus, window];
    let event_col2 = [object, document, terminal, cache];
    let event_col3 = [available, listeners, other_event, error];

    let rates = Table::new([
        Row::new(["Last", "Peak", "Mean", "Total"]).style(Style::default().fg(Color::LightYellow)),
        Row::new(column_data).bottom_margin(2),
    ])
    .style(Style::default().fg(Color::LightYellow))
    .widths(&[
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
    ])
    .column_spacing(1)
    .block(
        Block::default()
            .title("AT-SPI2 signal rate dashboard:")
            .border_style(Style::default().fg(Color::LightYellow))
            .border_type(ratatui::widgets::BorderType::Rounded)
            .borders(Borders::ALL),
    );

    let categories = Table::new([
        Row::new(["Keyboard", "Focus", "Mouse", "Window"])
            .style(Style::default().fg(Color::LightYellow)),
        Row::new(event_col1).bottom_margin(1),
        Row::new(["Object", "Document", "Terminal", "Cache"])
            .style(Style::default().fg(Color::LightYellow)),
        Row::new(event_col2).bottom_margin(1),
        Row::new(["Available", "Listeners", "Other", "Error"])
            .style(Style::default().fg(Color::LightYellow)),
        Row::new(event_col3),
    ])
    .style(Style::default().fg(Color::LightYellow))
    .widths(&[
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
    ])
    .column_spacing(1)
    .block(
        Block::default()
            .title("Categorized signals")
            .border_style(Style::default().fg(Color::LightYellow))
            .border_type(ratatui::widgets::BorderType::Rounded)
            .borders(Borders::ALL),
    );

    let binding = app.error_set.lock().unwrap();
    let error_list = ratatui::widgets::List::new(
        binding
            .iter()
            .map(|errs| ListItem::new(errs.as_str()))
            .collect::<Vec<ListItem<'_>>>(),
    )
    .block(
        Block::default()
            .title("Errors")
            .border_style(Style::default().fg(Color::LightRed))
            .border_type(ratatui::widgets::BorderType::Rounded)
            .borders(Borders::ALL),
    )
    .style(Style::default().fg(Color::LightRed))
    .highlight_style(Style::default().fg(Color::Red))
    .highlight_symbol(">> ");

    let server_stats = &app.servers.bus;

    let server_list = ratatui::widgets::List::new(
        server_stats
            .iter()
            .map(|server| {
                if let Ok(guard) = server.try_lock() {
                    ListItem::new(format!("{}:\n\t{}\n", guard.accessible_name, guard.stats))
                } else {
                    ListItem::new(format!("Server contended for lock"))
                }
            })
            .collect::<Vec<ListItem<'_>>>(),
    )
    .block(
        Block::default()
            .title("Server response time stats")
            .border_style(Style::default().fg(Color::LightBlue))
            .border_type(ratatui::widgets::BorderType::Rounded)
            .borders(Borders::ALL),
    )
    .style(Style::default().fg(Color::LightYellow))
    .highlight_style(Style::default().fg(Color::Blue))
    .highlight_symbol(">> ");

    f.render_widget(sparkline, chunks[0]);
    f.render_widget(rates, bottom_left[0]);
    f.render_widget(categories, bottom_left[1]);
    f.render_widget(error_list, bottom_right[0]);
    f.render_widget(server_list, bottom_right[1]);
}

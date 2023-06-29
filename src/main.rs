#![allow(unused_imports)]

use atspi::{
    connection::AccessibilityConnection,
    events::{
        self, document::DocumentEvents, focus::FocusEvents, keyboard::KeyboardEvents,
        mouse::MouseEvents, object::ObjectEvents, terminal::TerminalEvents, window::WindowEvents,
        AvailableEvent, CacheEvents, EventListenerEvents,
    },
};
use core::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_stream::StreamExt;
use zbus::fdo::{DBusProxy, MonitoringProxy};
use zbus::{MatchRule, Proxy};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let atspi: AccessibilityConnection = AccessibilityConnection::open().await?;

    atspi.register_event::<MouseEvents>().await?;
    atspi.register_event::<KeyboardEvents>().await?;
    atspi.register_event::<FocusEvents>().await?;
    atspi.register_event::<WindowEvents>().await?;
    atspi.register_event::<DocumentEvents>().await?;
    atspi.register_event::<ObjectEvents>().await?;
    atspi.register_event::<TerminalEvents>().await?;
    atspi.register_event::<CacheEvents>().await?;
    atspi.register_event::<EventListenerEvents>().await?;

    let events = atspi.event_stream();
    tokio::pin!(events);

    let counter = Arc::new(AtomicUsize::new(0));
    let now = Arc::new(Mutex::new(std::time::Instant::now()));
    let series: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::with_capacity(900)));

    //   let mut eventmap: HashMap<&str, usize> = Arc::new(Mutex::new(std::collections::HashMap::new()));

    // The 'ui'
    print!("{}", termion::clear::All);
    println!("{}{}", termion::cursor::Goto(1, 1), "=".repeat(80));
    println!(
        "{}{:<23}{:<23}{:<24}",
        termion::cursor::Goto(1, 2),
        "Event count",
        "Max event count",
        "Avg event count",
    );
    println!("{}{}", termion::cursor::Goto(1, 5), "=".repeat(80));

    // A scoped task that will print the current event count every second
    let counter_clone = counter.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut now = now.lock().unwrap();
            let mut series = series.lock().unwrap();
            series.push(counter_clone.swap(0, Ordering::SeqCst));
            println!(
                "{}{}{:<23}{:<23}{:<24}",
                termion::clear::CurrentLine,
                termion::cursor::Goto(1, 3),
                series.last().unwrap_or(&0),
                series.iter().max().unwrap_or(&0),
                series.iter().sum::<usize>() / series.len(),
            );
            *now = std::time::Instant::now();
        }
    });

    // Monitor the stream of events
    while let Some(_event) = events.next().await {
        counter.fetch_add(1, Ordering::SeqCst);
    }
    Ok(())
}

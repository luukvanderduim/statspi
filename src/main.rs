use atspi::{
    connection::AccessibilityConnection,
    events::{
        document::DocumentEvents, focus::FocusEvents, keyboard::KeyboardEvents, mouse::MouseEvents,
        object::ObjectEvents, terminal::TerminalEvents, window::WindowEvents, AvailableEvent,
    },
};
use core::panic;
use tokio_stream::StreamExt;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Result<()> {
    let atspi: AccessibilityConnection = AccessibilityConnection::open().await?;

    // atspi.register_event::<DocumentEvents>().await?;
    // atspi.register_event::<FocusEvents>().await?;
    // atspi.register_event::<KeyboardEvents>().await?;
    // atspi.register_event::<MouseEvents>().await?;
    atspi.register_event::<ObjectEvents>().await?;
    // atspi.register_event::<TerminalEvents>().await?;
    // atspi.register_event::<WindowEvents>().await?;
    // atspi.register_event::<AvailableEvent>().await?;

    // atspi.register_event::<CacheEvents>().await?;
    // atspi.register_event::<EventListenerEvents>().await?;

    let events = atspi.event_stream();
    tokio::pin!(events);

    let mut counter = 0;
    while let Some(event) = events.next().await {
        if event.is_ok() {
            counter += 1;
            print!("Event count: {counter}\r");
        } else if let Err(err) = event {
            panic!("Error: {}", err);
        }
    }

    Ok(())
}

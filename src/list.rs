// Shared state

//     let counter = Arc::new(AtomicUsize::new(0));
//     let now = Arc::new(Mutex::new(std::time::Instant::now()));
//     let series: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::with_capacity(900)));

//     let counter_clone = counter.clone();
//     // tokio::spawn(async move {

//     //         tokio::time::sleep(Duration::from_millis(tick_rate)).await;
//     //         let mut now = now.lock().unwrap();
//     //         let mut series = series.lock().unwrap();
//     //         series.push(counter_clone.swap(0, Ordering::SeqCst));
//     //         *now = std::time::Instant::now();

async fn monitor_events(counter: Arc<AtomicUsize>) -> ! {
    // Monitor the stream of events
    while let Some(_event) = events.next().await {
        counter.fetch_(Ordering::SeqCst);
    }
}

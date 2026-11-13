use mux::Mux;
use promise::spawn::Runnable;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use wezterm_mux_server_impl::sessionhandler::{PduSender, SessionHandler};

#[test]
fn coalesces_queued_pane_pushes() {
    // Keep this test in its own binary: promise's scheduler callbacks are global.
    let queued: Arc<Mutex<VecDeque<Runnable>>> = Arc::new(Mutex::new(VecDeque::new()));
    let normal = Arc::clone(&queued);
    let low = Arc::clone(&queued);
    promise::spawn::set_schedulers(
        Box::new(move |runnable| normal.lock().unwrap().push_back(runnable)),
        Box::new(move |runnable| low.lock().unwrap().push_back(runnable)),
    );
    Mux::set_mux(&Arc::new(Mux::new(None)));

    let mut handler = SessionHandler::new(PduSender::new(|_| Ok(())));
    for _ in 0..1000 {
        handler.schedule_pane_push(17);
    }
    assert_eq!(queued.lock().unwrap().len(), 1);

    handler.schedule_pane_push(29);
    assert_eq!(queued.lock().unwrap().len(), 2);

    let mut other_session = SessionHandler::new(PduSender::new(|_| Ok(())));
    other_session.schedule_pane_push(17);
    assert_eq!(queued.lock().unwrap().len(), 3);

    // The absent pane makes the queued task take its early error return.
    let runnable = queued.lock().unwrap().pop_front().unwrap();
    runnable.run();
    assert_eq!(queued.lock().unwrap().len(), 2);
    handler.schedule_pane_push(17);
    handler.schedule_pane_push(17);
    assert_eq!(queued.lock().unwrap().len(), 3);

    loop {
        let runnable = queued.lock().unwrap().pop_front();
        match runnable {
            Some(runnable) => {
                runnable.run();
            }
            None => break,
        }
    }
    Mux::shutdown();
}

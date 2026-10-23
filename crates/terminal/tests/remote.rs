//! Remote sessions (`kubectl exec`): input and size go to the far end,
//! output comes back through `feed_output`.

use std::sync::{Arc, Mutex};

use astrolabe_terminal::TerminalBackend;
use astrolabe_terminal::backend::{ExitStatus, RemoteSink, TerminalEvent, remote};

#[derive(Debug, PartialEq, Eq)]
enum Sent {
    Input(Vec<u8>),
    Resize(u16, u16),
    Close,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Sent>>>);

impl Recorder {
    fn sent(&self) -> Vec<Sent> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl RemoteSink for Recorder {
    fn input(&self, bytes: &[u8]) {
        self.0.lock().unwrap().push(Sent::Input(bytes.to_vec()));
    }
    fn resize(&self, cols: u16, rows: u16) {
        self.0.lock().unwrap().push(Sent::Resize(cols, rows));
    }
    fn close(&self) {
        self.0.lock().unwrap().push(Sent::Close);
    }
}

#[test]
fn the_far_end_learns_the_starting_size() {
    let sink = Recorder::default();
    let (_backend, _events) = remote(20, 4, 100, Box::new(sink.clone()));
    assert_eq!(sink.sent(), [Sent::Resize(20, 4)]);
}

#[test]
fn input_goes_to_the_far_end() {
    let sink = Recorder::default();
    let (backend, _events) = remote(20, 4, 100, Box::new(sink.clone()));
    sink.sent();
    backend.write_input(b"ls\r");
    assert_eq!(sink.sent(), [Sent::Input(b"ls\r".to_vec())]);
}

#[test]
fn output_is_rendered() {
    let (backend, _events) = remote(20, 4, 100, Box::new(Recorder::default()));
    backend.feed_output(b"\x1b[1m/ #\x1b[0m ");
    assert!(backend.snapshot().contains("/ #"));
}

#[test]
fn a_resize_reaches_the_far_end_once_per_size() {
    let sink = Recorder::default();
    let (backend, _events) = remote(20, 4, 100, Box::new(sink.clone()));
    sink.sent();
    backend.resize(80, 24, 8, 16);
    backend.resize(80, 24, 8, 16);
    backend.resize(20, 4, 8, 16);
    assert_eq!(sink.sent(), [Sent::Resize(80, 24), Sent::Resize(20, 4)]);
    assert_eq!(backend.snapshot().cols, 20);
}

#[test]
fn shutdown_closes_the_far_end_once() {
    let sink = Recorder::default();
    let (backend, _events) = remote(20, 4, 100, Box::new(sink.clone()));
    sink.sent();
    backend.shutdown();
    backend.shutdown();
    backend.write_input(b"late");
    assert_eq!(sink.sent(), [Sent::Close]);
}

#[test]
fn dropping_the_backend_closes_the_far_end() {
    let sink = Recorder::default();
    drop(remote(20, 4, 100, Box::new(sink.clone())));
    assert_eq!(sink.sent(), [Sent::Resize(20, 4), Sent::Close]);
}

#[test]
fn the_far_end_exiting_is_a_child_exit() {
    let (backend, events) = remote(20, 4, 100, Box::new(Recorder::default()));
    let status = ExitStatus {
        code: Some(130),
        signal: None,
    };
    backend.remote_exited(status);
    assert!(matches!(
        events.try_recv(),
        Some(TerminalEvent::ChildExit(exited)) if exited == status
    ));
}

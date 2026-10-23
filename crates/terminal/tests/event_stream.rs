//! `EventStream::next`: an async wait that output wakes.

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use astrolabe_terminal::TerminalBackend;
use astrolabe_terminal::backend::{TerminalEvent, display_only};

#[derive(Default)]
struct Flag(AtomicBool);

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn next_waits_until_output_wakes_it() {
    let (backend, events) = display_only(20, 4, 100);
    let flag = Arc::new(Flag::default());
    let waker = Waker::from(flag.clone());
    let mut cx = Context::from_waker(&waker);
    let mut next = pin!(events.next());

    assert!(next.as_mut().poll(&mut cx).is_pending());
    assert!(!flag.0.load(Ordering::SeqCst));

    backend.feed_output(b"hi");
    assert!(
        flag.0.load(Ordering::SeqCst),
        "output wakes the waiting task"
    );
    assert!(matches!(
        next.as_mut().poll(&mut cx),
        Poll::Ready(Some(TerminalEvent::Wakeup))
    ));
}

#[test]
fn queued_events_are_ready_at_once() {
    let (backend, events) = display_only(20, 4, 100);
    backend.feed_output(b"a");
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut next = pin!(events.next());
    assert!(matches!(
        next.as_mut().poll(&mut cx),
        Poll::Ready(Some(TerminalEvent::Wakeup))
    ));
}

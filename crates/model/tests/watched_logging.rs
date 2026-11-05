//! Lives in its own test binary on purpose: a thread-scoped subscriber races
//! other threads over tracing's per-callsite interest cache, so no other test
//! in this binary may reach the callsites asserted here (see #285). The same
//! rule is documented on `Logs` in `crates/kube/tests/support/mod.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use astrolabe_kube::kube::runtime::watcher::Event;

mod support;
use support::{namespace, namespace_watch, relist};

/// Counts the events logged while it is the thread's subscriber.
struct EventCount(Arc<AtomicUsize>);

impl tracing::Subscriber for EventCount {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn events_logged_by(f: impl FnOnce()) -> usize {
    let count = Arc::new(AtomicUsize::new(0));
    tracing::subscriber::with_default(EventCount(Arc::clone(&count)), f);
    count.load(Ordering::SeqCst)
}

#[test]
fn only_a_health_change_is_logged() {
    let mut watched = namespace_watch();
    relist(&mut watched, &["default"]);

    // Live stays Live: nothing to log.
    let same = events_logged_by(|| {
        watched.apply(Ok(Event::Apply(namespace("team-a"))));
    });
    // Live to Connecting is a health change: one log line.
    let changed = events_logged_by(|| {
        watched.apply(Ok(Event::Init));
    });

    assert_eq!((same, changed), (0, 1));
}

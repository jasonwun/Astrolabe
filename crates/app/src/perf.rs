//! Opt-in main-thread cost report (#106): how often the session notifies,
//! and what the panels spend re-projecting in answer. Off unless this
//! module's target is enabled at `trace`
//! (`RUST_LOG=warn,astrolabe::perf=trace`); then [`start`] logs one line
//! a second and resets the counts. Below `trace`, including the
//! `astrolabe=debug` watch-recovery filter, nothing is counted or timed.
//! `docs/agents/notify-cost.md` turns the lines into the #106 numbers.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use gpui_kit::App;

/// How often [`start`]'s report is logged.
const REPORT_PERIOD: Duration = Duration::from_secs(1);

/// The main-thread lag probe's period: a probe that wakes later than this
/// measures how long the main thread was busy.
const PROBE_PERIOD: Duration = Duration::from_millis(5);

/// A lag over this misses a 60 Hz frame.
const FRAME_BUDGET: Duration = Duration::from_millis(16);

/// Timed main-thread work.
#[derive(Clone, Copy)]
pub enum Work {
    /// `ResourceListPanel::refresh`: projects the list's whole watch.
    ListRefresh,
    /// `detail::resolve` in a detail tab's render.
    DetailResolve,
    /// A detail tab's whole render, its resolve included.
    DetailRender,
}

/// Counted events.
#[derive(Clone, Copy)]
pub enum Event {
    /// A watch message applied by a session's drain.
    WatchEvent,
    /// The drain's `cx.notify()` on the whole session after a batch.
    Notify,
    /// The workspace re-rendered: one window draw.
    Draw,
}

#[derive(Default)]
struct Stats {
    watch_events: u64,
    notifies: u64,
    draws: u64,
    /// Calls and total time, indexed by [`Work`].
    work: [(u64, Duration); 3],
    lag_max: Duration,
    lags_over_budget: u64,
}

thread_local! {
    /// The main thread's counts since the last report.
    static STATS: RefCell<Stats> = RefCell::default();
}

/// Whether the report is on.
pub fn enabled() -> bool {
    tracing::enabled!(tracing::Level::TRACE)
}

/// Count one `event`, when the report is on.
pub fn count(event: Event) {
    if !enabled() {
        return;
    }
    STATS.with_borrow_mut(|stats| match event {
        Event::WatchEvent => stats.watch_events += 1,
        Event::Notify => stats.notifies += 1,
        Event::Draw => stats.draws += 1,
    });
}

/// Times `work` until dropped; `None` while the report is off.
pub fn span(work: Work) -> Option<Span> {
    enabled().then(|| Span {
        work,
        start: Instant::now(),
    })
}

/// A running [`span`].
pub struct Span {
    work: Work,
    start: Instant,
}

impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        STATS.with_borrow_mut(|stats| {
            let (calls, total) = &mut stats.work[self.work as usize];
            *calls += 1;
            *total += elapsed;
        });
    }
}

/// Log the report every second, with the main thread's worst lag in it,
/// when the report is on; otherwise do nothing.
pub fn start(cx: &mut App) {
    if !enabled() {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(REPORT_PERIOD).await;
            report();
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        loop {
            let asked = cx.background_executor().now();
            cx.background_executor().timer(PROBE_PERIOD).await;
            let lag = (cx.background_executor().now() - asked).saturating_sub(PROBE_PERIOD);
            STATS.with_borrow_mut(|stats| {
                stats.lag_max = stats.lag_max.max(lag);
                if lag > FRAME_BUDGET {
                    stats.lags_over_budget += 1;
                }
            });
        }
    })
    .detach();
}

fn report() {
    let stats = STATS.take();
    let ms = |duration: Duration| format!("{:.2}", duration.as_secs_f64() * 1000.);
    let [list, resolve, render] = stats.work;
    tracing::trace!(
        watch_events = stats.watch_events,
        notifies = stats.notifies,
        draws = stats.draws,
        list_refresh = list.0,
        list_refresh_ms = %ms(list.1),
        detail_resolve = resolve.0,
        detail_resolve_ms = %ms(resolve.1),
        detail_render = render.0,
        detail_render_ms = %ms(render.1),
        lag_max_ms = %ms(stats.lag_max),
        lags_over_16ms = stats.lags_over_budget,
        "main-thread cost"
    );
}

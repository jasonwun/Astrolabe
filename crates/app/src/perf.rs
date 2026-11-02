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

/// One kind of [`Work`]'s calls and their total time.
#[derive(Default)]
struct Cost {
    calls: u64,
    total: Duration,
}

#[derive(Default)]
struct Stats {
    watch_events: u64,
    notifies: u64,
    draws: u64,
    list_refresh: Cost,
    detail_resolve: Cost,
    detail_render: Cost,
    lag_max: Duration,
    lags_over_budget: u64,
}

impl Stats {
    fn cost(&mut self, work: Work) -> &mut Cost {
        match work {
            Work::ListRefresh => &mut self.list_refresh,
            Work::DetailResolve => &mut self.detail_resolve,
            Work::DetailRender => &mut self.detail_render,
        }
    }
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
pub fn time(work: Work) -> Option<Timing> {
    enabled().then(|| Timing {
        work,
        start: Instant::now(),
    })
}

/// A running [`time`].
pub struct Timing {
    work: Work,
    start: Instant,
}

impl Drop for Timing {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        STATS.with_borrow_mut(|stats| {
            let cost = stats.cost(self.work);
            cost.calls += 1;
            cost.total += elapsed;
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
    tracing::trace!(
        watch_events = stats.watch_events,
        notifies = stats.notifies,
        draws = stats.draws,
        list_refresh = stats.list_refresh.calls,
        list_refresh_ms = %ms(stats.list_refresh.total),
        detail_resolve = stats.detail_resolve.calls,
        detail_resolve_ms = %ms(stats.detail_resolve.total),
        detail_render = stats.detail_render.calls,
        detail_render_ms = %ms(stats.detail_render.total),
        lag_max_ms = %ms(stats.lag_max),
        lags_over_16ms = stats.lags_over_budget,
        "main-thread cost"
    );
}

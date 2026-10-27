//! Measurement code shared by the dependency-upgrade benchmarks in
//! `spikes/*`: gpui frame-trace stats, RSS, load average, and the async
//! helpers the scripted modes drive their views with.

use std::collections::VecDeque;
use std::time::Duration;

use gpui_kit::profiler::{FrameEvent, FrameTimingCollector};
use gpui_kit::*;

/// The overlay's rolling window.
const FRAME_WINDOW: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------- frame stats (gpui profiler trace)

/// Drains gpui's frame trace for one window; keeps ~1s of draw times,
/// present times, and present-to-present gaps (the jank metric) for the
/// overlay, plus totals since the last `reset_totals` (none before it, so
/// an interactive session holds only the 1s window).
///
/// The timestamps come from gpui's trace (`web_time::Instant`).
pub struct FrameStats {
    collector: FrameTimingCollector,
    window_id: WindowId,
    draws: VecDeque<(web_time::Instant, Duration)>,
    presents: VecDeque<web_time::Instant>,
    gaps: VecDeque<(web_time::Instant, Duration)>,
    totals: Option<FrameTotals>,
}

/// Frame totals over a measured span (a scenario, a whole scripted run).
#[derive(Clone, Default)]
pub struct FrameTotals {
    pub presents: usize,
    pub draws: u32,
    pub draw_sum: Duration,
    pub draw_worst: Duration,
    /// Every present-to-present gap inside the span. The gap from the last
    /// present before the span to its first present is left out: it
    /// measures whatever preceded the span.
    pub gaps: Vec<Duration>,
    last_present: Option<web_time::Instant>,
}

impl FrameTotals {
    pub fn draw_avg(&self) -> Duration {
        if self.draws > 0 {
            self.draw_sum / self.draws
        } else {
            Duration::ZERO
        }
    }

    pub fn gap_worst(&self) -> Duration {
        self.gaps.iter().copied().max().unwrap_or_default()
    }

    pub fn gaps_over(&self, limit: Duration) -> usize {
        self.gaps.iter().filter(|g| **g > limit).count()
    }

    /// `gaps>66ms=3 gaps>100ms=1 …`, one count per limit.
    pub fn gap_counts(&self, limits: &[Duration]) -> String {
        limits
            .iter()
            .map(|l| format!("gaps>{l:?}={}", self.gaps_over(*l)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl FrameStats {
    pub fn new(window: &Window) -> Self {
        Self {
            collector: FrameTimingCollector::new(),
            window_id: window.window_handle().window_id(),
            draws: VecDeque::with_capacity(256),
            presents: VecDeque::with_capacity(256),
            gaps: VecDeque::with_capacity(256),
            totals: None,
        }
    }

    fn drain(&mut self) {
        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(t) if t.window_id == self.window_id => {
                    let d = t.draw_duration();
                    self.draws.push_back((t.draw_start, d));
                    if let Some(totals) = &mut self.totals {
                        totals.draws += 1;
                        totals.draw_sum += d;
                        totals.draw_worst = totals.draw_worst.max(d);
                    }
                }
                FrameEvent::Present(t) if t.window_id == self.window_id => {
                    let at = t.present_end;
                    if let Some(&prev) = self.presents.back() {
                        self.gaps.push_back((at, at.duration_since(prev)));
                    }
                    self.presents.push_back(at);
                    if let Some(totals) = &mut self.totals {
                        if let Some(prev) = totals.last_present {
                            totals.gaps.push(at.duration_since(prev));
                        }
                        totals.last_present = Some(at);
                        totals.presents += 1;
                    }
                }
                _ => {}
            }
        }
        while self
            .draws
            .front()
            .is_some_and(|(t, _)| t.elapsed() > FRAME_WINDOW)
        {
            self.draws.pop_front();
        }
        while self
            .presents
            .front()
            .is_some_and(|t| t.elapsed() > FRAME_WINDOW)
        {
            self.presents.pop_front();
        }
        while self
            .gaps
            .front()
            .is_some_and(|(t, _)| t.elapsed() > FRAME_WINDOW)
        {
            self.gaps.pop_front();
        }
    }

    /// Starts a new measured span.
    pub fn reset_totals(&mut self) {
        self.drain();
        self.totals = Some(FrameTotals::default());
    }

    /// Drains, then returns the totals since the last `reset_totals`.
    pub fn totals(&mut self) -> &FrameTotals {
        self.drain();
        self.totals.get_or_insert_default()
    }

    /// Ends the measured span and returns its totals.
    pub fn take_totals(&mut self) -> FrameTotals {
        self.drain();
        self.totals.take().unwrap_or_default()
    }

    /// Drains, then returns (fps over last 1s, mean draw, worst draw, worst
    /// present gap).
    pub fn stats(&mut self) -> (f32, Duration, Duration, Duration) {
        self.drain();
        let fps = self.presents.len() as f32 / FRAME_WINDOW.as_secs_f32();
        let n = self.draws.len() as u32;
        let (sum, worst) = self
            .draws
            .iter()
            .fold((Duration::ZERO, Duration::ZERO), |(s, w), (_, d)| {
                (s + *d, w.max(*d))
            });
        let avg = if n > 0 { sum / n } else { Duration::ZERO };
        let worst_gap = self
            .gaps
            .iter()
            .map(|(_, g)| *g)
            .max()
            .unwrap_or(Duration::ZERO);
        (fps, avg, worst, worst_gap)
    }
}

// ---------------------------------------------------------------- machine

/// Resident set size of this process (macOS `proc_pid_rusage`; 0 elsewhere).
pub fn rss_bytes() -> u64 {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let rc = libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        if rc == 0 {
            return info.ri_resident_size;
        }
        0
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// `1.23/2.34/3.45`: the 1, 5 and 15 minute load averages. Other
/// processes' CPU use skews frame timings; a summary carries this so a
/// reader can tell a loaded run from a regression.
pub fn load_average() -> String {
    let mut avg = [0f64; 3];
    // SAFETY: getloadavg writes at most `nelem` doubles into `avg`.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    if n < 3 {
        return "?".to_string();
    }
    format!("{:.2}/{:.2}/{:.2}", avg[0], avg[1], avg[2])
}

// ---------------------------------------------------------------- async helpers

pub async fn sleep(cx: &mut AsyncWindowContext, d: Duration) {
    cx.background_executor().timer(d).await;
}

/// `view.update_in`, or `None` once the view is gone.
pub fn with_view<T: 'static, R>(
    view: &WeakEntity<T>,
    cx: &mut AsyncWindowContext,
    f: impl FnOnce(&mut T, &mut Window, &mut Context<T>) -> R,
) -> Option<R> {
    view.update_in(cx, f).ok()
}

/// Waits up to 15 s for the window to present frames. An occluded or
/// offscreen window gets no draw callbacks from AppKit, so no frame
/// timings exist. Notifies the view each poll to provoke frames. Prints
/// why and returns false on timeout.
pub async fn wait_for_frames<T: 'static>(
    view: &WeakEntity<T>,
    cx: &mut AsyncWindowContext,
    frames: impl Fn(&mut T) -> &mut FrameStats,
) -> bool {
    for _ in 0..150 {
        with_view(view, cx, |_, _, cx| cx.notify());
        sleep(cx, Duration::from_millis(100)).await;
        let fps = with_view(view, cx, |this, _, _| frames(this).stats().0);
        if fps.is_some_and(|fps| fps > 0.0) {
            return true;
        }
    }
    println!(
        "no presented frames after 15s: the window is occluded or offscreen; frame metrics need a visible window. Bring it on screen and retry."
    );
    false
}

#[cfg(test)]
mod tests {
    use super::{Duration, FrameTotals};

    #[test]
    fn gap_counts_count_the_gaps_strictly_over_each_limit() {
        let ms = Duration::from_millis;
        let totals = FrameTotals {
            gaps: vec![ms(16), ms(66), ms(67), ms(120), ms(300)],
            ..FrameTotals::default()
        };
        assert_eq!(
            totals.gap_counts(&[ms(66), ms(100), ms(250)]),
            "gaps>66ms=3 gaps>100ms=2 gaps>250ms=1"
        );
        assert_eq!(totals.gap_worst(), ms(300));
    }
}

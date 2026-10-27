//! Scripted mode, enabled with `SPIKE_AUTO=1`.
//!
//! Drives the table from inside the app on a fixed schedule: page down and
//! up through the rows, step the selection, and step the sort through every
//! column. The schedule is a pure function of the step index, so every run
//! does the same work whatever the machine speed.

use std::time::{Duration, Instant};

use gpui_kit::component::table::{ColumnSort, TableDelegate as _, TableState};
use gpui_kit::*;

use crate::{PodsDelegate, SpikeView, rss_bytes};

/// One drive step per `STEP`.
pub const STEP: Duration = Duration::from_millis(100);
/// Rows a page-down / page-up moves the selection.
const PAGE_ROWS: usize = 30;
/// FAIL when any present-to-present gap exceeds this; README "Scripted mode"
/// says how it was picked.
pub const GAP_LIMIT: Duration = Duration::from_millis(250);
const DEFAULT_SECS: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DriveStep {
    PageDown,
    PageUp,
    RowDown,
    Sort(usize, ColumnSort),
}

/// The step at index `i` of the schedule, for a table with `cols` sortable
/// columns.
pub fn drive_step(i: usize, cols: usize) -> DriveStep {
    const SORTS: [ColumnSort; 3] = [
        ColumnSort::Ascending,
        ColumnSort::Descending,
        ColumnSort::Default,
    ];
    let (cycle, at) = (i / CYCLE_STEPS, i % CYCLE_STEPS);
    if at < PAGE_DOWNS {
        DriveStep::PageDown
    } else if at == PAGE_DOWNS {
        let k = cycle % (cols * SORTS.len());
        DriveStep::Sort(k / SORTS.len(), SORTS[k % SORTS.len()])
    } else if at <= PAGE_DOWNS + PAGE_UPS {
        DriveStep::PageUp
    } else {
        DriveStep::RowDown
    }
}

const PAGE_DOWNS: usize = 8;
const PAGE_UPS: usize = 4;
const ROW_DOWNS: usize = 6;
/// Page-downs, a sort step, page-ups, row-downs.
pub const CYCLE_STEPS: usize = PAGE_DOWNS + 1 + PAGE_UPS + ROW_DOWNS;

/// Whole drive cycles that fit in `run`.
pub fn cycle_count(run: Duration) -> usize {
    (run.as_millis() / (STEP * CYCLE_STEPS as u32).as_millis()) as usize
}

async fn sleep(cx: &mut AsyncWindowContext, d: Duration) {
    cx.background_executor().timer(d).await;
}

fn with_view<R>(
    view: &WeakEntity<SpikeView>,
    cx: &mut AsyncWindowContext,
    f: impl FnOnce(&mut SpikeView, &mut Window, &mut Context<SpikeView>) -> R,
) -> Option<R> {
    view.update_in(cx, f).ok()
}

fn quit(cx: &mut AsyncWindowContext, pass: bool) {
    if !pass {
        std::process::exit(1);
    }
    let _ = cx.update(|_, app| app.quit());
}

/// Runs the schedule for `SPIKE_SECS` (default 300), prints a summary and a
/// `RESULT:` line, then quits (exit 1 on FAIL).
pub async fn run(view: WeakEntity<SpikeView>, cx: &mut AsyncWindowContext) {
    let secs = std::env::var("SPIKE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SECS);
    let run = Duration::from_secs(secs);

    // An occluded window gets no draw callbacks, so no frame timings exist.
    // The view re-renders every tick; wait until presents flow.
    let mut visible = false;
    for _ in 0..150 {
        sleep(cx, Duration::from_millis(100)).await;
        let fps = with_view(&view, cx, |this, _, _| {
            this.frames.drain();
            this.frames.stats().0
        });
        if fps.is_some_and(|fps| fps > 0.0) {
            visible = true;
            break;
        }
    }
    if !visible {
        println!(
            "[auto] no presented frames after 15s: the window is occluded or offscreen. Bring it on screen and retry."
        );
        println!("[auto] RESULT: FAIL (no frames)");
        quit(cx, false);
        return;
    }

    let cycles = cycle_count(run);
    let Some(rss_start) = with_view(&view, cx, |this, _, _| {
        this.frames.reset_run();
        rss_bytes()
    }) else {
        return;
    };
    println!(
        "[auto] driving {cycles} cycles of {CYCLE_STEPS} steps every {STEP:?} for {secs}s, gap limit {GAP_LIMIT:?}"
    );

    let start = Instant::now();
    let mut late_worst = Duration::ZERO;
    let mut steps_done = 0;
    for i in 0..cycles * CYCLE_STEPS {
        let due = start + STEP * i as u32;
        let now = Instant::now();
        if due > now {
            sleep(cx, due - now).await;
        } else {
            late_worst = late_worst.max(now - due);
        }
        let step = with_view(&view, cx, |this, window, cx| {
            this.table.update(cx, |table, cx| {
                let cols = table.delegate().columns.len();
                apply_step(table, drive_step(i, cols), window, cx);
            });
        });
        if step.is_none() {
            return;
        }
        steps_done += 1;
    }
    let end = start + run;
    let now = Instant::now();
    if end > now {
        sleep(cx, end - now).await;
    }

    let Some((r, flushes, applied)) = with_view(&view, cx, |this, _, _| {
        this.frames.drain();
        let s = &this.stats;
        (this.frames.run, s.flushes, s.adds + s.updates + s.deletes)
    }) else {
        return;
    };
    let (draws, draw_avg, draw_worst, gap_worst, gaps_over) = (
        r.draws,
        r.draw_avg(),
        r.draw_worst,
        r.gap_worst,
        r.gaps_over_limit,
    );
    println!(
        "[auto] summary: cycles={} secs={:.0} flushes={flushes} applied={applied} draws={draws} draw_avg={draw_avg:.2?} draw_worst={draw_worst:.2?} gap_worst={gap_worst:.2?} gaps>{GAP_LIMIT:?}={gaps_over} late_worst={late_worst:.2?} rss={}→{}MB",
        steps_done / CYCLE_STEPS,
        start.elapsed().as_secs_f32(),
        rss_start / 1_048_576,
        rss_bytes() / 1_048_576,
    );
    let pass = draws > 0 && gap_worst <= GAP_LIMIT;
    println!(
        "[auto] RESULT: {} (gap_worst {gap_worst:.2?}, limit {GAP_LIMIT:?})",
        if pass { "PASS" } else { "FAIL" }
    );
    quit(cx, pass);
}

/// What a key press or header click would do, through `TableState`'s public API.
fn apply_step(
    table: &mut TableState<PodsDelegate>,
    step: DriveStep,
    window: &mut Window,
    cx: &mut Context<TableState<PodsDelegate>>,
) {
    let max_row = table.delegate().pods.len().saturating_sub(1);
    let row = table.selected_row().unwrap_or(0).min(max_row);
    match step {
        // Wrap at the ends so the selection keeps moving through the rows.
        DriveStep::PageDown if row == max_row => table.set_selected_row(0, cx),
        DriveStep::PageDown => table.set_selected_row((row + PAGE_ROWS).min(max_row), cx),
        DriveStep::PageUp => table.set_selected_row(row.saturating_sub(PAGE_ROWS), cx),
        DriveStep::RowDown if row == max_row => table.set_selected_row(0, cx),
        DriveStep::RowDown => table.set_selected_row(row + 1, cx),
        // A header click's work, minus its indicator: `TableState::perform_sort`
        // is private, so the header arrows stay unchanged.
        DriveStep::Sort(col, sort) => {
            table.delegate_mut().perform_sort(col, sort, window, cx);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CYCLE_STEPS, ColumnSort, DriveStep, Duration, cycle_count, drive_step};

    #[test]
    fn a_cycle_pages_down_sorts_pages_up_then_steps_rows() {
        let steps: Vec<_> = (0..CYCLE_STEPS).map(|i| drive_step(i, 8)).collect();
        let mut want = vec![DriveStep::PageDown; 8];
        want.push(DriveStep::Sort(0, ColumnSort::Ascending));
        want.extend([DriveStep::PageUp; 4]);
        want.extend([DriveStep::RowDown; 6]);
        assert_eq!(steps, want);
    }

    #[test]
    fn the_sort_steps_through_every_column_asc_desc_off_then_wraps() {
        let sorts: Vec<_> = (0..CYCLE_STEPS * 25)
            .map(|i| drive_step(i, 8))
            .filter_map(|s| match s {
                DriveStep::Sort(col, sort) => Some((col, sort)),
                _ => None,
            })
            .collect();
        assert_eq!(sorts.len(), 25);
        assert_eq!(
            &sorts[..6],
            &[
                (0, ColumnSort::Ascending),
                (0, ColumnSort::Descending),
                (0, ColumnSort::Default),
                (1, ColumnSort::Ascending),
                (1, ColumnSort::Descending),
                (1, ColumnSort::Default),
            ]
        );
        assert_eq!(sorts[23], (7, ColumnSort::Default));
        assert_eq!(sorts[24], (0, ColumnSort::Ascending));
    }

    #[test]
    fn the_default_run_fits_157_cycles() {
        // 19 steps × 100 ms = 1.9 s per cycle; 300 s / 1.9 s = 157.9.
        assert_eq!(cycle_count(Duration::from_secs(300)), 157);
        assert_eq!(cycle_count(Duration::from_secs(1)), 0);
    }
}

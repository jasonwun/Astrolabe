//! Scripted mode, enabled with `SPIKE_AUTO=1`.
//!
//! Drives the table from inside the app on a fixed schedule: page down and
//! up through the rows, step the selection, and step the sort through every
//! column. The schedule is a pure function of the step index, so every run
//! does the same work whatever the machine speed.

use std::time::{Duration, Instant};

use gpui_kit::component::table::{ColumnSort, TableDelegate as _, TableState};
use gpui_kit::*;

use spike_support::{load_average, rss_bytes, sleep, wait_for_frames, with_view};

use crate::{PodsDelegate, SpikeView};

/// One drive step per `STEP`.
pub const STEP: Duration = Duration::from_millis(100);
/// Rows a page-down / page-up moves the selection.
const PAGE_ROWS: usize = 30;
/// A present-to-present gap over this is a jank frame: two missed 50 ms
/// flushes. README "Scripted mode" says how both limits were picked.
const JANK_GAP: Duration = Duration::from_millis(100);
/// Gap limits the summary counts, so a new pass criterion can be read off
/// past runs (docs/agents/spike-regression.md).
const GAP_LIMITS: [Duration; 4] = [
    Duration::from_millis(66),
    JANK_GAP,
    Duration::from_millis(150),
    Duration::from_millis(250),
];
/// FAIL when a run has more than one jank frame per minute.
fn jank_limit(secs: u64) -> usize {
    (secs / 60).max(1) as usize
}
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

    if !wait_for_frames(&view, cx, |this| &mut this.frames).await {
        println!("[auto] RESULT: FAIL (no frames)");
        quit(cx, false);
        return;
    }

    let cycles = cycle_count(run);
    let jank_limit = jank_limit(secs);
    let Some(rss_start) = with_view(&view, cx, |this, _, _| {
        this.frames.reset_totals();
        rss_bytes()
    }) else {
        return;
    };
    let load_start = load_average();
    println!(
        "[auto] driving {cycles} cycles of {CYCLE_STEPS} steps every {STEP:?} for {secs}s, FAIL over {jank_limit} gaps > {JANK_GAP:?}"
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

    let Some((t, flushes, applied)) = with_view(&view, cx, |this, _, _| {
        let s = &this.stats;
        (
            this.frames.totals().clone(),
            s.flushes,
            s.adds + s.updates + s.deletes,
        )
    }) else {
        return;
    };
    let jank_gaps = t.gaps_over(JANK_GAP);
    println!(
        "[auto] summary: cycles={} secs={:.0} flushes={flushes} applied={applied} draws={} draw_avg={:.2?} draw_worst={:.2?} gap_worst={:.2?} {} late_worst={late_worst:.2?} rss={}→{}MB load={load_start}→{}",
        steps_done / CYCLE_STEPS,
        start.elapsed().as_secs_f32(),
        t.draws,
        t.draw_avg(),
        t.draw_worst,
        t.gap_worst(),
        t.gap_counts(&GAP_LIMITS),
        rss_start / 1_048_576,
        rss_bytes() / 1_048_576,
        load_average(),
    );
    let pass = t.draws > 0 && jank_gaps <= jank_limit;
    println!(
        "[auto] RESULT: {} ({jank_gaps} gaps > {JANK_GAP:?}, limit {jank_limit})",
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
    use super::{
        CYCLE_STEPS, ColumnSort, DriveStep, Duration, cycle_count, drive_step, jank_limit,
    };

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

    #[test]
    fn a_run_may_have_one_jank_frame_per_minute() {
        assert_eq!(jank_limit(300), 5);
        assert_eq!(jank_limit(90), 1);
        assert_eq!(jank_limit(20), 1);
    }
}

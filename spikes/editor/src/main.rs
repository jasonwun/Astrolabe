//! PROTOTYPE / THROWAWAY — Editor vs giant YAML spike. Not production code.
//!
//! Question (issue #6): does gpui-kit v0.8.0's code `Editor` stay fluid on a
//! ~2.9MB / ~38k-line Kubernetes CRD YAML — open, scroll, edit, tree-sitter
//! YAML highlighting, undo/redo, and YAML indent continuation?
//!
//! `SPIKE_AUTO=1 cargo run --release` runs the full scripted suite, prints
//! results to stdout, and quits (set `SPIKE_STAY=1` to keep the window).
//! Interactively: ctrl-1 load, ctrl-2 scroll, ctrl-3 edit, ctrl-4 undo/redo,
//! ctrl-5 indent test, ctrl-0 run all.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
use gpui_kit::component::input::language_config::LanguageConfig;
use gpui_kit::component::input::{
    Backspace, Editor, EditorState, Enter, IndentationRules, MovePageDown, MoveToEnd, MoveToStart,
    Position, Redo, Rope, RopeExt as _, TabSize, Undo, set_language_config,
};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::profiler::{FrameEvent, FrameTimingCollector};
use gpui_kit::*;

gpui_kit::actions!(
    spike,
    [RunLoad, RunScroll, RunEdit, RunUndoRedo, RunIndent, RunAll]
);

const KEY_CONTEXT: &str = "spike";
const DATA_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/postgresclusters-crd.yaml"
);
const FRAME_WINDOW: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------- frame stats (gpui profiler trace)

/// Per-scenario frame totals, accumulated between `begin_phase` and `take_phase`.
#[derive(Default, Clone)]
struct PhaseStats {
    presents: usize,
    draws: Vec<Duration>,
    gaps: Vec<Duration>,
}

impl PhaseStats {
    fn report(&self) -> String {
        if self.presents == 0 && self.draws.is_empty() {
            return "NO FRAMES (window occluded? frame timings unavailable)".to_string();
        }
        let n = self.draws.len() as u32;
        let (sum, worst) = self
            .draws
            .iter()
            .fold((Duration::ZERO, Duration::ZERO), |(s, w), d| {
                (s + *d, w.max(*d))
            });
        let avg = if n > 0 { sum / n } else { Duration::ZERO };
        let worst_gap = self.gaps.iter().copied().max().unwrap_or_default();
        // Present gaps above ~2 frames at 60Hz are stutters (or idle periods;
        // scenarios reset the phase after every deliberate settle).
        let stutters = self
            .gaps
            .iter()
            .filter(|g| **g > Duration::from_millis(33))
            .count();
        format!(
            "frames={} draw_avg={:.2?} draw_worst={:.2?} gap_worst={:.2?} gaps>33ms={}",
            self.presents, avg, worst, worst_gap, stutters
        )
    }
}

/// Drains gpui's frame trace for this window; keeps ~1s of draw times,
/// present times, and present-to-present gaps (the jank metric) for the
/// overlay, plus cumulative per-scenario totals.
///
/// The timestamps come from gpui's trace (`web_time::Instant`); pruning uses
/// `elapsed()` so this type never has to name it.
struct FrameStats {
    collector: FrameTimingCollector,
    window_id: WindowId,
    draws: VecDeque<(web_time::Instant, Duration)>,
    presents: VecDeque<web_time::Instant>,
    gaps: VecDeque<(web_time::Instant, Duration)>,
    phase: PhaseStats,
}

impl FrameStats {
    fn new(window: &Window) -> Self {
        Self {
            collector: FrameTimingCollector::new(),
            window_id: window.window_handle().window_id(),
            draws: VecDeque::with_capacity(256),
            presents: VecDeque::with_capacity(256),
            gaps: VecDeque::with_capacity(256),
            phase: PhaseStats::default(),
        }
    }

    fn drain(&mut self) {
        for event in self.collector.collect_unseen() {
            match event {
                FrameEvent::Draw(t) if t.window_id == self.window_id => {
                    let d = t.draw_duration();
                    self.draws.push_back((t.draw_start, d));
                    self.phase.draws.push(d);
                }
                FrameEvent::Present(t) if t.window_id == self.window_id => {
                    let at = t.present_end;
                    if let Some(&prev) = self.presents.back() {
                        let gap = at.duration_since(prev);
                        self.gaps.push_back((at, gap));
                        self.phase.gaps.push(gap);
                    }
                    self.presents.push_back(at);
                    self.phase.presents += 1;
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

    fn begin_phase(&mut self) {
        self.drain();
        self.phase = PhaseStats::default();
    }

    fn take_phase(&mut self) -> PhaseStats {
        self.drain();
        let mut phase = std::mem::take(&mut self.phase);
        // The first present of a phase measures its gap from the last present
        // of the previous (deliberately idle) settle — an artifact, not a
        // stutter. Drop it.
        if !phase.gaps.is_empty() {
            phase.gaps.remove(0);
        }
        phase
    }

    /// (fps over last 1s, mean draw, worst draw, worst present gap)
    fn stats(&mut self) -> (f32, Duration, Duration, Duration) {
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

// ---------------------------------------------------------------- RSS (macOS mach)

fn rss_bytes() -> u64 {
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let rc = libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        if rc == 0 { info.ri_resident_size } else { 0 }
    }
}

// ---------------------------------------------------------------- async helpers

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

// ---------------------------------------------------------------- standalone tree-sitter cost (question 1)

/// Measures the raw cost of tree-sitter YAML on this file, outside the editor:
/// full parse, style extraction, incremental reparse after a 1-char edit, and
/// whether a 2 ms sync budget (the editor's SYNC_PARSE_TIMEOUT) completes.
fn bench_highlighter(content: &str) {
    let rope = Rope::from_str(content);
    let mut hl = SyntaxHighlighter::new("yaml");

    let t = Instant::now();
    let ok = hl.update(None, &rope, None);
    let full = t.elapsed();
    println!(
        "[highlight] full parse (no timeout): {:.2?} completed={} tree={}",
        full,
        ok,
        hl.tree().is_some()
    );

    let theme = HighlightTheme::default_dark();
    let t = Instant::now();
    let styles = hl.styles(&(0..rope.len().min(100_000)), theme.as_ref());
    let mut colors: Vec<String> = styles
        .iter()
        .filter_map(|(_, s)| s.color)
        .map(|c| format!("{c:?}"))
        .collect();
    colors.sort();
    colors.dedup();
    println!(
        "[highlight] styles for first 100KB: {:.2?}, {} runs, {} distinct colors",
        t.elapsed(),
        styles.len(),
        colors.len()
    );

    // Incremental: one inserted char in the middle of the document.
    let mut edited = content.to_string();
    let mut m = edited.len() / 2;
    while !edited.is_char_boundary(m) {
        m += 1;
    }
    edited.insert(m, 'x');
    let row = content[..m].matches('\n').count();
    let col = m - content[..m].rfind('\n').map_or(0, |i| i + 1);
    let edit = tree_sitter::InputEdit {
        start_byte: m,
        old_end_byte: m,
        new_end_byte: m + 1,
        start_position: tree_sitter::Point::new(row, col),
        old_end_position: tree_sitter::Point::new(row, col),
        new_end_position: tree_sitter::Point::new(row, col + 1),
    };
    let rope2 = Rope::from_str(&edited);
    let t = Instant::now();
    let ok = hl.update(Some(edit), &rope2, None);
    println!(
        "[highlight] incremental reparse after 1-char edit: {:.2?} completed={}",
        t.elapsed(),
        ok
    );

    // The editor only attempts a 2 ms synchronous parse for documents up to
    // 256KB (then falls back to a debounced background parse). Both gates fail
    // for this file; measure what a 2 ms attempt would do anyway. A second,
    // distinct edit — the incremental reparse above already covered rope2.
    edited.insert(m + 1, 'y');
    let edit2 = tree_sitter::InputEdit {
        start_byte: m + 1,
        old_end_byte: m + 1,
        new_end_byte: m + 2,
        start_position: tree_sitter::Point::new(row, col + 1),
        old_end_position: tree_sitter::Point::new(row, col + 1),
        new_end_position: tree_sitter::Point::new(row, col + 2),
    };
    let rope3 = Rope::from_str(&edited);
    let t = Instant::now();
    let ok = hl.update(Some(edit2), &rope3, Some(Duration::from_millis(2)));
    println!(
        "[highlight] reparse with 2ms budget (editor's sync timeout): {:.2?} completed={}",
        t.elapsed(),
        ok
    );
}

// ---------------------------------------------------------------- view

struct SpikeView {
    editor: Entity<EditorState>,
    frames: FrameStats,
    /// File content cached for reloads (the load scenario re-reads from disk).
    content: String,
    status: String,
    rss: u64,
    last_rss: Instant,
}

impl SpikeView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let auto = std::env::var("SPIKE_AUTO").is_ok();
        let content = std::fs::read_to_string(DATA_PATH).expect("read data file");
        println!(
            "[spike] data {} bytes, {} lines",
            content.len(),
            content.matches('\n').count()
        );

        let editor = cx.new(|cx| {
            let state = EditorState::new(window, cx)
                .language("yaml")
                .line_number(true)
                .indent_guides(true)
                .tab_size(TabSize {
                    tab_size: 2,
                    hard_tabs: false,
                })
                .soft_wrap(false);
            // Interactive mode loads immediately; the auto suite's load
            // scenario sets the value itself so it can time it.
            if auto {
                state
            } else {
                state.default_value(content.clone())
            }
        });

        let focus_handle = editor.focus_handle(cx).clone();
        window.defer(cx, move |window, cx| {
            focus_handle.focus(window, cx);
        });

        let view = Self {
            editor,
            frames: FrameStats::new(window),
            content,
            status: if auto {
                "auto suite starting".into()
            } else {
                "interactive".into()
            },
            rss: rss_bytes(),
            last_rss: Instant::now(),
        };

        if auto {
            let task = cx.spawn_in(window, async move |view, cx| run_all(view, cx).await);
            task.detach();
        }

        view
    }

    fn set_status(&mut self, status: impl Into<String>, cx: &mut Context<Self>) {
        self.status = status.into();
        cx.notify();
    }

    fn run(&mut self, which: u8, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx.spawn_in(window, async move |view, cx| match which {
            1 => scenario_load(view, cx).await,
            2 => scenario_scroll(view, cx).await,
            3 => scenario_edit(view, cx).await,
            4 => scenario_undo_redo(view, cx).await,
            5 => scenario_indent(view, cx).await,
            _ => run_all(view, cx).await,
        });
        task.detach();
    }
}

impl Render for SpikeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.frames.drain();
        let (fps, avg_draw, worst_draw, worst_gap) = self.frames.stats();

        if self.last_rss.elapsed() >= Duration::from_secs(1) {
            self.rss = rss_bytes();
            self.last_rss = Instant::now();
        }

        let overlay = h_flex()
            .gap_4()
            .px_3()
            .py_1()
            .text_sm()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(format!("fps {fps:>3.0}"))
            .child(format!(
                "draw avg {avg_draw:.2?} worst {worst_draw:.2?} (1s)"
            ))
            .child(format!("gap worst {worst_gap:.2?}"))
            .child(format!("rss {} MB", self.rss / 1_048_576))
            .child(self.status.clone())
            .child("[ctrl-1..5 scenarios, ctrl-0 all]");

        v_flex()
            .size_full()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &RunLoad, w, cx| this.run(1, w, cx)))
            .on_action(cx.listener(|this, _: &RunScroll, w, cx| this.run(2, w, cx)))
            .on_action(cx.listener(|this, _: &RunEdit, w, cx| this.run(3, w, cx)))
            .on_action(cx.listener(|this, _: &RunUndoRedo, w, cx| this.run(4, w, cx)))
            .on_action(cx.listener(|this, _: &RunIndent, w, cx| this.run(5, w, cx)))
            .on_action(cx.listener(|this, _: &RunAll, w, cx| this.run(0, w, cx)))
            .child(overlay)
            .child(
                div().flex_1().min_h_0().child(
                    Editor::new(&self.editor)
                        .bordered(false)
                        .p_0()
                        .h(relative(1.)),
                ),
            )
    }
}

// ---------------------------------------------------------------- scenarios

async fn run_all(view: WeakEntity<SpikeView>, cx: &mut AsyncWindowContext) {
    // Warmup: let the window settle, fonts load, first frames present.
    sleep(cx, Duration::from_millis(800)).await;

    // Visibility gate: an occluded window receives no draw callbacks from
    // AppKit, so no frame timings exist. Wait until presents flow (keep
    // invalidating to provoke them), or give up with a clear message.
    let mut visible = false;
    for _ in 0..150 {
        with_view(&view, cx, |this, _, cx| {
            this.set_status("waiting for a visible window", cx);
            this.frames.drain();
        });
        sleep(cx, Duration::from_millis(100)).await;
        let n = with_view(&view, cx, |this, _, _| {
            this.frames.drain();
            this.frames.stats().0
        })
        .unwrap_or(0.0);
        if n > 0.0 {
            visible = true;
            break;
        }
    }
    if !visible {
        println!(
            "[spike] no presented frames after 15s — the window is occluded or offscreen; frame metrics need a visible window. Bring it on screen and retry."
        );
        let _ = cx.update(|_, app| app.quit());
        return;
    }

    scenario_load(view.clone(), cx).await;
    // Give the debounced background parse time to finish before scrolling.
    sleep(cx, Duration::from_secs(2)).await;
    println!(
        "[load] rss after load + background parse: {} MB",
        rss_bytes() / 1_048_576
    );

    let content = with_view(&view, cx, |this, _, _| this.content.clone()).unwrap_or_default();
    bench_highlighter(&content);

    scenario_scroll(view.clone(), cx).await;
    scenario_edit(view.clone(), cx).await;
    scenario_undo_redo(view.clone(), cx).await;
    scenario_indent(view.clone(), cx).await;

    // Restore the big file so a staying window is usable.
    with_view(&view, cx, |this, window, cx| {
        let content = this.content.clone();
        this.editor
            .update(cx, |s, cx| s.set_value(content, window, cx));
        this.set_status("suite done", cx);
    });

    println!("[spike] suite complete");
    if std::env::var("SPIKE_STAY").is_err() {
        let _ = cx.update(|_, app| app.quit());
    }
}

/// Load: file read + set_value wall time, and time to the first presented frame.
async fn scenario_load(view: WeakEntity<SpikeView>, cx: &mut AsyncWindowContext) {
    let t_read = Instant::now();
    let content = std::fs::read_to_string(DATA_PATH).expect("read data file");
    let read = t_read.elapsed();

    let (set, t0) = with_view(&view, cx, |this, window, cx| {
        this.set_status("load", cx);
        let t_set = Instant::now();
        this.editor
            .update(cx, |s, cx| s.set_value(content.clone(), window, cx));
        let set = t_set.elapsed();
        this.frames.begin_phase();
        (set, Instant::now())
    })
    .expect("view alive");

    // Poll for the first presented frame after set_value.
    let mut first_present = None;
    for _ in 0..2000 {
        sleep(cx, Duration::from_millis(2)).await;
        let presents = with_view(&view, cx, |this, _, _| {
            this.frames.drain();
            this.frames.phase.presents
        })
        .unwrap_or(0);
        if presents > 0 {
            first_present = Some(t0.elapsed());
            break;
        }
    }
    let stats = with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();
    println!(
        "[load] read={:.2?} set_value={:.2?} first_present={:.2?} {}",
        read,
        set,
        first_present.unwrap_or_default(),
        stats.report()
    );
}

/// Scroll: jump to five spread-out positions, then a page-down burst.
async fn scenario_scroll(view: WeakEntity<SpikeView>, cx: &mut AsyncWindowContext) {
    let lines = with_view(&view, cx, |this, _, cx| {
        this.set_status("scroll", cx);
        this.content.matches('\n').count() + 1
    })
    .unwrap_or(0);

    // Jump cut to 0%, 25%, 50%, 75%, 100% — forces layout of distant regions.
    // Draw timings are meaningful here; present gaps are not (deliberate
    // 100 ms idles between jumps), so the phase is reset before the burst.
    with_view(&view, cx, |this, _, _| this.frames.begin_phase());
    for frac in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let row = ((lines as f32 * frac) as usize).min(lines.saturating_sub(1));
        with_view(&view, cx, |this, window, cx| {
            this.editor.update(cx, |s, cx| {
                s.set_cursor_position(Position::new(row as u32, 0), window, cx)
            });
        });
        sleep(cx, Duration::from_millis(100)).await;
    }
    with_view(&view, cx, |_, window, cx| {
        window.dispatch_action(Box::new(MoveToEnd), cx);
    });
    sleep(cx, Duration::from_millis(100)).await;
    let jumps = with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();

    // Page-down burst through the document, one page per frame tick.
    with_view(&view, cx, |_, window, cx| {
        window.dispatch_action(Box::new(MoveToStart), cx);
    });
    sleep(cx, Duration::from_millis(100)).await;
    with_view(&view, cx, |this, _, _| this.frames.begin_phase());
    let mut iter_worst = Duration::ZERO;
    for _ in 0..60 {
        let t = Instant::now();
        with_view(&view, cx, |_, window, cx| {
            window.dispatch_action(Box::new(MovePageDown), cx);
        });
        sleep(cx, Duration::from_millis(16)).await;
        iter_worst = iter_worst.max(t.elapsed());
    }
    sleep(cx, Duration::from_millis(50)).await;
    let burst = with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();

    println!(
        "[scroll] jumps=5+end {} | pagedown-burst=60 {} loop_iter_worst={:.2?}",
        jumps.report(),
        burst.report(),
        iter_worst
    );
}

/// Edit: typing bursts (single chars) and line inserts at top/middle/end.
async fn scenario_edit(view: WeakEntity<SpikeView>, cx: &mut AsyncWindowContext) {
    let lines = with_view(&view, cx, |this, _, cx| {
        this.set_status("edit", cx);
        this.content.matches('\n').count() + 1
    })
    .unwrap_or(0);

    with_view(&view, cx, |this, _, _| this.frames.begin_phase());

    let mut latencies: Vec<Duration> = Vec::new();
    let positions = [0usize, lines / 2, lines.saturating_sub(1)];
    for (spot, row) in positions.into_iter().enumerate() {
        // 20 single-char inserts (typing) ...
        with_view(&view, cx, |this, window, cx| {
            this.editor.update(cx, |s, cx| {
                s.set_cursor_position(Position::new(row as u32, 0), window, cx)
            });
        });
        for i in 0..20 {
            let lat = with_view(&view, cx, |this, window, cx| {
                let t = Instant::now();
                this.editor.update(cx, |s, cx| {
                    s.insert("#", window, cx);
                });
                t.elapsed()
            });
            if let Some(lat) = lat {
                latencies.push(lat);
            }
            let _ = i;
            sleep(cx, Duration::from_millis(5)).await;
        }
        // ... then 10 line inserts.
        for i in 0..10 {
            let text = format!("# spike line {spot}-{i}\n");
            let lat = with_view(&view, cx, |this, window, cx| {
                let t = Instant::now();
                this.editor.update(cx, |s, cx| {
                    s.insert(text, window, cx);
                });
                t.elapsed()
            });
            if let Some(lat) = lat {
                latencies.push(lat);
            }
            sleep(cx, Duration::from_millis(5)).await;
        }
    }

    // Delete burst: 60 backspaces at the end. Separate phase so its frame
    // stats aren't mixed with the insert bursts.
    let insert_stats =
        with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();
    with_view(&view, cx, |this, window, cx| {
        this.editor.update(cx, |s, cx| {
            s.set_cursor_position(Position::new(lines.saturating_sub(1) as u32, 0), window, cx)
        });
    });
    sleep(cx, Duration::from_millis(100)).await;
    with_view(&view, cx, |this, _, _| this.frames.begin_phase());
    let mut del_iter_worst = Duration::ZERO;
    for _ in 0..60 {
        let t = Instant::now();
        with_view(&view, cx, |_, window, cx| {
            window.dispatch_action(Box::new(Backspace), cx);
        });
        sleep(cx, Duration::from_millis(8)).await;
        del_iter_worst = del_iter_worst.max(t.elapsed());
    }
    sleep(cx, Duration::from_millis(50)).await;
    let delete_stats =
        with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();

    latencies.sort();
    let avg = latencies.iter().sum::<Duration>() / latencies.len().max(1) as u32;
    let p50 = latencies
        .get(latencies.len() / 2)
        .copied()
        .unwrap_or_default();
    let worst = latencies.last().copied().unwrap_or_default();
    println!(
        "[edit] inserts={} edit_avg={:.2?} edit_p50={:.2?} edit_worst={:.2?} | insert-burst {} | delete-burst=60 {}",
        latencies.len(),
        avg,
        p50,
        worst,
        insert_stats.report(),
        delete_stats.report()
    );
    println!("[edit] delete_burst_iter_worst={:.2?}", del_iter_worst);
}

/// Undo/redo: 50 atomic line inserts, then 50 undos, then 50 redos.
async fn scenario_undo_redo(view: WeakEntity<SpikeView>, cx: &mut AsyncWindowContext) {
    const N: usize = 50;

    let baseline = with_view(&view, cx, |this, _, cx| {
        this.set_status("undo/redo", cx);
        let len = this.editor.read(cx).text().len();
        let row = this.content.matches('\n').count().div_ceil(2);
        (len, row)
    })
    .unwrap_or((0, 0));

    // N atomic inserts, each its own undo unit.
    with_view(&view, cx, |this, window, cx| {
        this.editor.update(cx, |s, cx| {
            s.set_cursor_position(Position::new(baseline.1 as u32, 0), window, cx)
        });
    });
    with_view(&view, cx, |this, _, _| this.frames.begin_phase());
    for i in 0..N {
        let text = format!("spike-undo-{i}: true\n");
        with_view(&view, cx, |this, window, cx| {
            this.editor.update(cx, |s, cx| {
                s.insert(text, window, cx);
            });
        });
        sleep(cx, Duration::from_millis(5)).await;
    }
    let insert_stats =
        with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();
    let after_inserts =
        with_view(&view, cx, |this, _, cx| this.editor.read(cx).text().len()).unwrap_or(0);

    // N undos.
    sleep(cx, Duration::from_millis(100)).await;
    with_view(&view, cx, |this, _, _| this.frames.begin_phase());
    let t_undo = Instant::now();
    let mut undo_iter_worst = Duration::ZERO;
    for _ in 0..N {
        let t = Instant::now();
        with_view(&view, cx, |_, window, cx| {
            window.dispatch_action(Box::new(Undo), cx);
        });
        sleep(cx, Duration::from_millis(10)).await;
        undo_iter_worst = undo_iter_worst.max(t.elapsed());
    }
    let undo_wall = t_undo.elapsed();
    sleep(cx, Duration::from_millis(50)).await;
    let undo_stats =
        with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();
    let after_undos =
        with_view(&view, cx, |this, _, cx| this.editor.read(cx).text().len()).unwrap_or(0);

    // N redos.
    with_view(&view, cx, |this, _, _| this.frames.begin_phase());
    let t_redo = Instant::now();
    let mut redo_iter_worst = Duration::ZERO;
    for _ in 0..N {
        let t = Instant::now();
        with_view(&view, cx, |_, window, cx| {
            window.dispatch_action(Box::new(Redo), cx);
        });
        sleep(cx, Duration::from_millis(10)).await;
        redo_iter_worst = redo_iter_worst.max(t.elapsed());
    }
    let redo_wall = t_redo.elapsed();
    sleep(cx, Duration::from_millis(50)).await;
    let redo_stats =
        with_view(&view, cx, |this, _, _| this.frames.take_phase()).unwrap_or_default();
    let after_redos =
        with_view(&view, cx, |this, _, cx| this.editor.read(cx).text().len()).unwrap_or(0);

    println!(
        "[undo/redo] n={} undo_wall={:.2?} ({:.2?}/op incl. 10ms pacing) redo_wall={:.2?} ({:.2?}/op) len baseline={} after_inserts={} after_undos={} after_redos={}",
        N,
        undo_wall,
        undo_wall / N as u32,
        redo_wall,
        redo_wall / N as u32,
        baseline.0,
        after_inserts,
        after_undos,
        after_redos,
    );
    println!(
        "[undo/redo] undo_iter_worst={:.2?} redo_iter_worst={:.2?}",
        undo_iter_worst, redo_iter_worst
    );
    println!(
        "[undo/redo] insert-burst {} | undo-burst {} | redo-burst {}",
        insert_stats.report(),
        undo_stats.report(),
        redo_stats.report()
    );
}

/// Indent continuation (question 3): Enter after `key:`, `key:` at column 0,
/// nested keys, and `- ` list items. Runs on a small scratch buffer; the suite
/// reloads the big file afterwards.
async fn scenario_indent(view: WeakEntity<SpikeView>, cx: &mut AsyncWindowContext) {
    with_view(&view, cx, |this, _, cx| this.set_status("indent", cx));

    // (label, scratch text, cursor line, expected leading spaces on the new line)
    let cases: &[(&str, &str, u32, usize)] = &[
        ("plain mapping entry", "key: value\nnext: 1\n", 0, 0),
        ("top-level key:", "spec:\n  replicas: 1\n", 0, 2),
        ("nested key:", "      containers:\n      other: 1\n", 0, 8),
        (
            "list item line",
            "        - name: app\n        - name: db\n",
            0,
            8,
        ),
        ("list item at EOF", "        - name: app\n", 0, 8),
        ("list item ending with ':'", "        - name:\n", 0, 10),
    ];

    for (label, scratch, row, expected) in cases {
        // End column of the cursor line (scratch is ASCII).
        let col = scratch.lines().nth(*row as usize).map_or(0, |l| l.len()) as u32;
        with_view(&view, cx, |this, window, cx| {
            this.editor.update(cx, |s, cx| {
                s.set_value(scratch.to_string(), window, cx);
                s.set_cursor_position(Position::new(*row, col), window, cx);
            });
        });
        sleep(cx, Duration::from_millis(100)).await;
        with_view(&view, cx, |_, window, cx| {
            window.dispatch_action(
                Box::new(Enter {
                    secondary: false,
                    shift: false,
                }),
                cx,
            );
        });
        sleep(cx, Duration::from_millis(100)).await;

        let got = with_view(&view, cx, |this, _, cx| {
            let state = this.editor.read(cx);
            let row = state.cursor_position().line as usize;
            let line: String = state.text().slice_line(row).chars().collect();
            let indent = line.len() - line.trim_start().len();
            (row, indent, line.trim_end().to_string())
        })
        .unwrap_or((0, 0, String::new()));

        let verdict = if got.1 == *expected { "OK" } else { "MISMATCH" };
        println!(
            "[indent] {label}: Enter after line {row} -> new line {} indent={} (expected {expected}) text={:?} {verdict}",
            got.0, got.1, got.2
        );
    }
}

// ---------------------------------------------------------------- main

fn main() {
    #[expect(
        clippy::disallowed_methods,
        reason = "a spike measures gpui-kit alone, without the app's icon assets"
    )]
    gpui_kit::application().run(|cx| {
        // Turn on gpui's frame trace so the overlay's FrameTimingCollector
        // actually receives draw/present events (off by default).
        gpui_kit::profiler::set_trace_enabled(true);
        gpui_kit::init(cx);
        // Foreground the app: an occluded window gets no draw callbacks from
        // AppKit, and without draws there are no frame timings to collect.
        cx.activate(true);

        // YAML editing defaults: indent one level after lines ending with a
        // block opener, same increase-indent pattern as gpui-kit's built-in
        // Python config. Without this, YAML falls back to bracket-only rules.
        set_language_config(
            "yaml",
            LanguageConfig::default().indentation_rules(IndentationRules::new(
                regex::Regex::new(r"[\{\(\[:]\s*$").unwrap(),
                regex::Regex::new(r"^\s*[\}\)\]]").unwrap(),
            )),
            cx,
        );

        cx.bind_keys([
            KeyBinding::new("ctrl-1", RunLoad, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-2", RunScroll, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-3", RunEdit, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-4", RunUndoRedo, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-5", RunIndent, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-0", RunAll, Some(KEY_CONTEXT)),
        ]);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| SpikeView::new(window, cx))
        })
        .expect("failed to open window");
    });
}

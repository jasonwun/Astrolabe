//! Scripted smoke suite, enabled with `SPIKE_AUTO=1`.
//!
//! Drives the real PTY-backed session end to end: injects input bytes,
//! waits for the grid to reflect them, and asserts colors/flags/modes/
//! resize/scrollback/flood behavior. Prints `[auto]` lines and a final
//! PASS/FAIL, then exits the child and (unless `SPIKE_STAY=1`) quits.
//!
//! `SPIKE_LATENCY_PROBE=1` adds keystroke(echo-roundtrip)→paint samples.
//! `SPIKE_DUMP_GRID=<path>` writes the final visible grid as text.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::profiler::{FrameEvent, FrameTimingCollector};
use gpui_kit::*;
use spike_support::{load_average, rss_bytes};

use crate::backend::{DEFAULT_BG, DEFAULT_FG, RenderSnapshot, Rgba, StyleFlags, TerminalBackend};
use crate::view::{Probe, TerminalView};

const STEP_TIMEOUT: Duration = Duration::from_secs(8);

pub fn spawn_driver(cx: &mut Context<TerminalView>) {
    cx.spawn(async move |this, cx| {
        let Ok(backend) = this.update(cx, |v, _| v.backend().clone()) else {
            return;
        };
        if std::env::var("SPIKE_AUTO").ok().as_deref() == Some("log-flood") {
            run_log_flood(this, backend, cx).await;
        } else {
            run_suite(this, backend, cx).await;
        }
    })
    .detach();
}

// ---------------------------------------------------------------- log-flood benchmark
//
// Simulates `kubectl logs -f` at ~10k lines/s against the display-only
// backend (no PTY): a pacer thread feeds colored ANSI log lines in 4 KiB
// chunks (arbitrary split points — escape sequences cross chunk boundaries
// constantly, as with real pipes). The scrollback is bounded
// (crate::LOG_SCROLLBACK lines), so a passing run must show bounded RSS.

const FLOOD_DURATION: Duration = Duration::from_secs(60);
const TARGET_LINES_PER_SEC: u64 = 10_000;
const FEED_CHUNK: usize = 4096;

struct FeedStats {
    lines: u64,
    bytes: u64,
    chunks: u64,
    wall: Duration,
    /// Total/max time inside `feed_output` (vte parse + grid write under
    /// the FairMutex — the number that can starve the render side).
    advance_total: Duration,
    advance_max: Duration,
}

/// A realistic-ish colored log line, ~110–140 bytes.
fn log_line(seq: u64) -> Vec<u8> {
    let (level, status_color) = match seq % 20 {
        0 => ("\x1b[31mERROR\x1b[0m", "\x1b[31m500\x1b[0m"),
        1..=3 => ("\x1b[33mWARN \x1b[0m", "\x1b[33m404\x1b[0m"),
        _ => ("\x1b[32mINFO \x1b[0m", "\x1b[36m200\x1b[0m"),
    };
    format!(
        "2026-10-20T01:{:02}:{:02}.{:03}Z {} pod/nginx-7d8f9c6d5-xk2lp[nginx] \
         10.244.3.17 - GET /api/v1/namespaces/kube-system/pods?limit=500 \
         {} {}µs seq={seq}\n",
        (seq / 1000 / 60) % 60,
        (seq / 1000) % 60,
        seq % 1000,
        level,
        status_color,
        40 + seq % 900,
    )
    .into_bytes()
}

async fn run_log_flood(
    this: WeakEntity<TerminalView>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut AsyncApp,
) {
    println!(
        "[log-flood] {TARGET_LINES_PER_SEC} lines/s for {FLOOD_DURATION:?}, \
         {FEED_CHUNK}-byte chunks, scrollback bound = {} lines",
        crate::LOG_SCROLLBACK
    );
    // Let the first render/resize settle before measuring.
    settle(cx, 500).await;
    let rss_before = rss_bytes();
    let load_start = load_average();

    // ---- feeder thread (the "PTY reader" of the log use case) ----
    let (tx, rx) = std::sync::mpsc::channel::<FeedStats>();
    let feeder_backend = backend.clone();
    std::thread::Builder::new()
        .name("log-feeder".into())
        .spawn(move || {
            let start = Instant::now();
            let mut seq = 0u64;
            let mut bytes = 0u64;
            let mut chunks = 0u64;
            let mut advance_total = Duration::ZERO;
            let mut advance_max = Duration::ZERO;
            let mut buf: Vec<u8> = Vec::with_capacity(FEED_CHUNK + 256);
            while start.elapsed() < FLOOD_DURATION {
                while buf.len() < FEED_CHUNK {
                    seq += 1;
                    buf.extend_from_slice(&log_line(seq));
                }
                let t0 = Instant::now();
                feeder_backend.feed_output(&buf);
                let d = t0.elapsed();
                advance_total += d;
                advance_max = advance_max.max(d);
                bytes += buf.len() as u64;
                chunks += 1;
                buf.clear();
                // Pace to the target average rate.
                let target = Duration::from_secs_f64(seq as f64 / TARGET_LINES_PER_SEC as f64);
                let elapsed = start.elapsed();
                if target > elapsed {
                    std::thread::sleep(target - elapsed);
                }
            }
            let _ = tx.send(FeedStats {
                lines: seq,
                bytes,
                chunks,
                wall: start.elapsed(),
                advance_total,
                advance_max,
            });
        })
        .expect("spawn feeder");

    // ---- UI-responsiveness probe during the flood ----
    // A timed snapshot is exactly what the paint path does once per frame;
    // its latency under lock contention is the "does the UI stay smooth"
    // proxy that works without a visible window.
    let stats0 = backend.stats();
    let (w0, n0) = this
        .update(cx, |v, _| (v.wakeup_events, v.notifies))
        .unwrap_or((0, 0));
    let mut snap_latencies: Vec<Duration> = Vec::new();
    let feed = loop {
        match rx.try_recv() {
            Ok(f) => break f,
            Err(_) => {
                let t0 = Instant::now();
                std::hint::black_box(backend.snapshot());
                snap_latencies.push(t0.elapsed());
                settle(cx, 2000).await;
            }
        }
    };

    // Let the drain loop consume the tail of the stream.
    settle(cx, 500).await;
    let stats1 = backend.stats();
    let (w1, n1) = this
        .update(cx, |v, _| (v.wakeup_events, v.notifies))
        .unwrap_or((0, 0));
    let rss_after = rss_bytes();
    let snap = backend.snapshot();

    // Post-flood responsiveness burst.
    let mut post: Vec<Duration> = Vec::new();
    for _ in 0..20 {
        let t0 = Instant::now();
        std::hint::black_box(backend.snapshot());
        post.push(t0.elapsed());
    }
    let post_max = post.iter().copied().max().unwrap_or_default();

    let snap_max = snap_latencies.iter().copied().max().unwrap_or_default();
    let snap_avg = snap_latencies.iter().sum::<Duration>() / (snap_latencies.len().max(1) as u32);
    let wakeups = stats1.wakeups - stats0.wakeups;
    let notifies = n1 - n0;
    let _ = (w0, w1);

    println!(
        "[log-flood] fed: {} lines / {:.1} MiB in {:.1?} = {:.0} lines/s ({:.1} MiB/s), {} chunks",
        feed.lines,
        feed.bytes as f64 / 1_048_576.0,
        feed.wall,
        feed.lines as f64 / feed.wall.as_secs_f64(),
        feed.bytes as f64 / 1_048_576.0 / feed.wall.as_secs_f64(),
        feed.chunks,
    );
    println!(
        "[log-flood] parse: feed_output total={:.2?} avg={:.2?}/chunk max={:.2?} \
         -> engine parse throughput {:.1} MiB/s",
        feed.advance_total,
        feed.advance_total / (feed.chunks.max(1) as u32),
        feed.advance_max,
        feed.bytes as f64 / 1_048_576.0 / feed.advance_total.as_secs_f64(),
    );
    println!(
        "[log-flood] coalescing: {} wakeups -> {} notifies ({:.1}x)",
        wakeups,
        notifies,
        wakeups as f64 / (notifies.max(1)) as f64,
    );
    println!(
        "[log-flood] snapshot-under-load: n={} avg={:.2?} max={:.2?} (grid {}x{}); \
         post-flood burst max={:.2?}",
        snap_latencies.len(),
        snap_avg,
        snap_max,
        snap.cols,
        snap.rows,
        post_max,
    );
    println!(
        "[log-flood] rss: before={} MB after={} MB (delta={} MB), history={} lines (bound={})",
        rss_before / 1_048_576,
        rss_after / 1_048_576,
        (rss_after as i64 - rss_before as i64) / 1_048_576,
        snap.history_size,
        crate::LOG_SCROLLBACK,
    );

    // Verdicts.
    let mut failures = 0;
    let mut check = |name: &str, ok: bool| {
        println!("[log-flood] {} {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };
    check(
        "rate-on-target",
        feed.lines as f64 / feed.wall.as_secs_f64() > TARGET_LINES_PER_SEC as f64 * 0.95,
    );
    check(
        "parse-much-faster-than-feed",
        feed.advance_total < feed.wall / 10,
    );
    // No feed stall: a single 4 KiB feed must never block >50ms.
    check(
        "no-feed-stall",
        feed.advance_max < Duration::from_millis(50),
    );
    // No UI stall proxy: snapshot under load stays well under a frame.
    check(
        "no-snapshot-stall",
        snap_max < Duration::from_millis(16) && post_max < Duration::from_millis(16),
    );
    check(
        "memory-bounded",
        snap.history_size <= crate::LOG_SCROLLBACK && rss_after < rss_before + 256 * 1_048_576,
    );
    println!("[log-flood] load={load_start}→{}", load_average());
    println!(
        "[log-flood] RESULT: {} ({} failure{})",
        if failures == 0 { "PASS" } else { "FAIL" },
        failures,
        if failures == 1 { "" } else { "s" },
    );

    if let Ok(path) = std::env::var("SPIKE_DUMP_GRID") {
        let _ = std::fs::write(&path, backend.snapshot().to_plain_text());
        println!("[log-flood] grid dumped to {path}");
    }
    quit(cx, &this).await;
}

struct Ctx {
    failures: usize,
}

impl Ctx {
    fn check(&mut self, name: &str, ok: bool) {
        if ok {
            println!("[auto] PASS {name}");
        } else {
            println!("[auto] FAIL {name}");
            self.failures += 1;
        }
    }
}

async fn wait_for(
    cx: &mut AsyncApp,
    backend: &Arc<dyn TerminalBackend>,
    pred: impl Fn(&RenderSnapshot) -> bool,
) -> bool {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        if pred(&backend.snapshot()) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
}

async fn settle(cx: &mut AsyncApp, ms: u64) {
    cx.background_executor()
        .timer(Duration::from_millis(ms))
        .await;
}

/// Most recent occurrence of an ASCII needle → (col, row). Searches
/// bottom-up so output lines win over the echoed command line above them.
fn find_text(snap: &RenderSnapshot, needle: &str) -> Option<(usize, usize)> {
    for row in (0..snap.rows).rev() {
        let line: String = (0..snap.cols).map(|c| snap.cell(c, row).c).collect();
        if let Some(idx) = line.rfind(needle) {
            return Some((idx, row));
        }
    }
    None
}

async fn run_suite(
    this: WeakEntity<TerminalView>,
    backend: Arc<dyn TerminalBackend>,
    cx: &mut AsyncApp,
) {
    let mut t = Ctx { failures: 0 };
    println!("[auto] scripted suite starting, load={}", load_average());

    // 1. Shell comes up with a prompt.
    let up = wait_for(cx, &backend, |s| s.contains("spike$")).await;
    t.check("shell-prompt", up);
    if !up {
        println!("[auto] aborting: no prompt");
        quit(cx, &this).await;
        return;
    }
    let snap = backend.snapshot();
    println!("[auto] grid {}x{}", snap.cols, snap.rows);

    // 2. Input round-trip: typed command executes, output lands on the grid.
    backend.write_input(b"echo SPIKE_$((40+2))_OK\r");
    let ok = wait_for(cx, &backend, |s| s.contains("SPIKE_42_OK")).await;
    t.check("echo-roundtrip", ok);

    // 3. Colors: 256-color and truecolor.
    backend.write_input(
        b"printf '\\033[38;5;196mC256OK\\033[0m \\033[38;2;12;34;56mTRUEOK\\033[0m\\n'\r",
    );
    let ok = wait_for(cx, &backend, |s| s.contains("TRUEOK")).await;
    let snap = backend.snapshot();
    let c256 = find_text(&snap, "C256OK").map(|(c, r)| snap.cell(c, r).fg);
    let truec = find_text(&snap, "TRUEOK").map(|(c, r)| snap.cell(c, r).fg);
    t.check("color-256", ok && c256 == Some(Rgba::rgb(255, 0, 0)));
    t.check(
        "color-truecolor",
        ok && truec == Some(Rgba::rgb(12, 34, 56)),
    );

    // 4. Style flags: bold/italic/underline/dim/inverse.
    backend.write_input(
        b"printf '\\033[1mBOLD\\033[0m \\033[3mITAL\\033[0m \\033[4mUNDL\\033[0m \\033[2mDIMM\\033[0m \\033[7mINVR\\033[0m\\n'\r",
    );
    let ok = wait_for(cx, &backend, |s| s.contains("INVR")).await;
    let snap = backend.snapshot();
    let flag_at = |needle: &str| find_text(&snap, needle).map(|(c, r)| snap.cell(c, r).flags);
    let cell_at = |needle: &str| find_text(&snap, needle).map(|(c, r)| snap.cell(c, r).clone());
    t.check(
        "flag-bold",
        ok && flag_at("BOLD").is_some_and(|f| f.contains(StyleFlags::BOLD)),
    );
    t.check(
        "flag-italic",
        ok && flag_at("ITAL").is_some_and(|f| f.contains(StyleFlags::ITALIC)),
    );
    t.check(
        "flag-underline",
        ok && flag_at("UNDL").is_some_and(|f| f.contains(StyleFlags::UNDERLINE)),
    );
    t.check(
        "flag-dim",
        ok && cell_at("DIMM").is_some_and(|c| c.fg.r < DEFAULT_FG.r),
    );
    t.check(
        "flag-inverse",
        ok && cell_at("INVR").is_some_and(|c| c.fg == DEFAULT_BG && c.bg == DEFAULT_FG),
    );

    // 5. Wide chars.
    backend.write_input("echo WIDE你SPIKE\n".as_bytes());
    let ok = wait_for(cx, &backend, |s| s.contains("WIDE你SPIKE")).await;
    t.check("wide-chars", ok);

    // 6. Full-screen app (alt screen) via vim, scripted.
    if std::path::Path::new("/usr/bin/vim").exists() {
        backend.write_input(b"vim -u NONE -n\r");
        let alt = wait_for(cx, &backend, |_| backend.modes().alt_screen).await;
        t.check("vim-alt-screen-enter", alt);
        backend.write_input(b"iSPIKE_VIM_TEXT\x1b");
        let text = wait_for(cx, &backend, |s| s.contains("SPIKE_VIM_TEXT")).await;
        t.check("vim-insert-visible", alt && text);
        backend.write_input(b":wq spike_vim_out.txt\r");
        let back = wait_for(cx, &backend, |_| !backend.modes().alt_screen).await;
        t.check("vim-alt-screen-exit", back);
        backend.write_input(b"cat spike_vim_out.txt && rm spike_vim_out.txt\r");
        let saved = wait_for(cx, &backend, |s| s.contains("SPIKE_VIM_TEXT")).await;
        t.check("vim-file-roundtrip", saved);
    } else {
        println!("[auto] SKIP vim steps: /usr/bin/vim not found");
    }

    // 7. htop if available (SGR mouse mode + dense truecolor frames).
    let htop = [
        "/opt/homebrew/bin/htop",
        "/usr/local/bin/htop",
        "/usr/bin/htop",
    ]
    .iter()
    .find(|p| std::path::Path::new(p).exists())
    .map(|s| s.to_string());
    if let Some(htop) = htop {
        backend.write_input(format!("{htop}\r").as_bytes());
        let alt = wait_for(cx, &backend, |_| backend.modes().alt_screen).await;
        settle(cx, 800).await;
        let snap = backend.snapshot();
        let colored = (0..snap.rows * snap.cols)
            .filter(|&i| snap.cells[i].bg != DEFAULT_BG || snap.cells[i].fg != DEFAULT_FG)
            .count();
        t.check(
            "htop-fullscreen-frame",
            alt && colored > snap.cells.len() / 4,
        );
        backend.write_input(b"q");
        let back = wait_for(cx, &backend, |_| !backend.modes().alt_screen).await;
        t.check("htop-exit", back);
    } else {
        println!("[auto] SKIP htop steps: not installed");
    }

    // 8. Resize: engine + PTY (SIGWINCH), verified through `stty size`.
    let before = backend.snapshot();
    let (cols, rows) = (before.cols, before.rows);
    let (new_cols, new_rows) = (cols + 7, rows + 3);
    backend.resize(new_cols, new_rows, 8, 17);
    settle(cx, 200).await;
    backend.write_input(b"stty size\r");
    let want = format!("{new_rows} {new_cols}");
    let ok = wait_for(cx, &backend, |s| s.contains(&want)).await;
    t.check("resize-sigwinch", ok);
    backend.resize(cols, rows, 8, 17);
    settle(cx, 200).await;

    // 9. Flood: `yes` for 600ms. Proves wakeup coalescing holds and the
    //    UI stays alive; records draw timings through gpui's profiler.
    let mut frames = FrameTimingCollector::new();
    let (w0, n0) = this
        .update(cx, |v, _| (v.wakeup_events, v.notifies))
        .unwrap_or((0, 0));
    let stats0 = backend.stats();
    backend.write_input(b"yes SPIKE_FLOOD_LINE\r");
    settle(cx, 600).await;
    backend.write_input(b"\x03");
    settle(cx, 600).await;
    let (w1, n1) = this
        .update(cx, |v, _| (v.wakeup_events, v.notifies))
        .unwrap_or((0, 0));
    let stats1 = backend.stats();
    let mut draws = Vec::new();
    for event in frames.collect_unseen() {
        if let FrameEvent::Draw(d) = event {
            draws.push(d.draw_duration());
        }
    }
    let draws_total: Duration = draws.iter().sum();
    let draw_avg = if draws.is_empty() {
        Duration::ZERO
    } else {
        draws_total / draws.len() as u32
    };
    let draw_worst = draws.iter().copied().max().unwrap_or_default();
    println!(
        "[auto] flood: engine-wakeups={} batched-to-notifies={} draws={} draw_avg={:.2?} draw_worst={:.2?}",
        stats1.wakeups - stats0.wakeups,
        n1 - n0,
        draws.len(),
        draw_avg,
        draw_worst,
    );
    let _ = (w0, w1);
    t.check("flood-survived", n1 > n0);
    t.check(
        "flood-coalesced",
        stats1.wakeups - stats0.wakeups > (n1 - n0), // strictly fewer notifies than wakeups
    );

    // 10. Scrollback present after the flood; scroll_display works.
    let snap = backend.snapshot();
    t.check("scrollback-grew", snap.history_size > 0);
    backend.scroll_display(3);
    let off = backend.snapshot().display_offset;
    t.check("scroll-display", off >= 3);
    backend.scroll_to_bottom();

    // 11. Damage experiment data point (v2 option).
    let stats = backend.stats();
    println!(
        "[auto] damage: snapshots={} full={} partial-lines={}",
        stats.snapshots, stats.damage_full, stats.damage_partial_lines
    );

    // 11b. Full-snapshot copy cost (the per-frame engine-side cost of
    // the v1 "full renderable_content" strategy).
    let snap = backend.snapshot();
    let iters = 100u32;
    let t0 = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(backend.snapshot());
    }
    println!(
        "[auto] snapshot-copy: {}x{} grid, avg over {iters} iters = {:.2?}",
        snap.cols,
        snap.rows,
        t0.elapsed() / iters,
    );

    // 12. Latency probe.
    if std::env::var("SPIKE_LATENCY_PROBE").ok().as_deref() == Some("1") {
        for i in 0..5 {
            let marker = format!("SPIKE_PROBE_{i}_XYZZY");
            let start = Instant::now();
            let _ = this.update(cx, |v, _| {
                *v.probe.lock().unwrap() = Some(Probe {
                    marker: marker.clone(),
                    start,
                });
            });
            backend.write_input(format!("echo {marker}\r").as_bytes());
            // Wait until a paint observes the marker (probe slot cleared).
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let done = this
                    .update(cx, |v, _| v.probe.lock().unwrap().is_none())
                    .unwrap_or(true);
                if done || Instant::now() > deadline {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(2))
                    .await;
            }
        }
    }

    // 13. Grid dump.
    if let Ok(path) = std::env::var("SPIKE_DUMP_GRID") {
        let text = backend.snapshot().to_plain_text();
        match std::fs::write(&path, &text) {
            Ok(()) => println!("[auto] grid dumped to {path}"),
            Err(err) => println!("[auto] grid dump failed: {err}"),
        }
    }

    println!("[auto] load at end={}", load_average());
    println!(
        "[auto] RESULT: {} ({} failure{})",
        if t.failures == 0 { "PASS" } else { "FAIL" },
        t.failures,
        if t.failures == 1 { "" } else { "s" },
    );

    // 14. Clean shutdown: exit the child, observe ChildExit, then quit.
    backend.write_input(b"exit\r");
    let deadline = Instant::now() + Duration::from_secs(3);
    let exited = loop {
        let done = this
            .update(cx, |v, _| v.child_exited().is_some())
            .unwrap_or(false);
        if done {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    };
    println!(
        "[auto] child-exit-event: {}",
        if exited { "received, clean" } else { "TIMEOUT" }
    );

    quit(cx, &this).await;
}

async fn quit(cx: &mut AsyncApp, _this: &WeakEntity<TerminalView>) {
    if std::env::var("SPIKE_STAY").ok().as_deref() == Some("1") {
        println!("[auto] SPIKE_STAY=1: window stays open");
        return;
    }
    cx.update(|cx| cx.quit());
}

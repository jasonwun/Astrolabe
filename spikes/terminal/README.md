# terminal-spike

A feasibility and performance benchmark, rerun on every dependency upgrade
(`docs/agents/spike-regression.md`). A workspace member, so it tracks the
pinned dependencies; RSS and load average come from `spikes/support`.

## Question it answers

GitHub issue jasonwun/Astrolabe#7 (research: `docs/research/alacritty-gpui-path.md`
on branch `research/alacritty-gpui-path`): can a terminal emulator be built on
`alacritty_terminal` (VT engine, grid state) + GPUI (self-rendered grid)?

## Run

```sh
cd spikes/terminal
cargo run --release            # interactive: a real terminal running $SHELL
SPIKE_AUTO=1 cargo run --release   # scripted suite vs /bin/sh, prints results, quits
SPIKE_AUTO=log-flood cargo run --release  # 10k lines/s display-only log benchmark (60s)
SPIKE_AUTO=1 SPIKE_STAY=1 cargo run --release  # suite, then keep window
SPIKE_DUMP_GRID=/tmp/grid.txt SPIKE_AUTO=1 cargo run --release  # dump final grid
SPIKE_LATENCY_PROBE=1 SPIKE_AUTO=1 cargo run --release  # + keystroke->paint probe
SPIKE_DEBUG_PAINT=1 cargo run --release  # print when the canvas first paints
```

First build of GPUI is heavy (several minutes). `cargo test` runs 23 unit
tests (key encoding + ANSI-stream → grid assertions through the backend,
no window needed).

**Caveat for paint-side numbers:** macOS delivers no frame callbacks to
occluded windows (same as the editor spike). The scripted suite's grid
assertions are engine-side and always valid; draw timings and the latency
probe need a *visible* window.

## What was verified, and how (maps to research doc §5)

Scripted suite (`SPIKE_AUTO=1`), release build, Apple Silicon macOS,
2026-10-20 — **RESULT: PASS (0 failures)**, plus 23/23 unit tests:

```
[auto] PASS shell-prompt            # tty::new spawns /bin/sh, TERM/COLORTERM set, prompt on grid
[auto] grid 122x58                  # font-metrics-derived cols/rows, initial resize applied
[auto] PASS echo-roundtrip          # input bytes -> child -> output on grid ("SPIKE_42_OK")
[auto] PASS color-256               # 38;5;196 -> rgb(255,0,0) in snapshot
[auto] PASS color-truecolor         # 38;2;12;34;56 -> rgb(12,34,56) in snapshot
[auto] PASS flag-bold/italic/underline/dim/inverse   # StyleFlags + INVERSE fg/bg swap + DIM darkening
[auto] PASS wide-chars              # CJK: WIDE_CHAR + spacer, cursor advances by 2
[auto] PASS vim-alt-screen-enter    # vim -u NONE: TermMode::ALT_SCREEN set
[auto] PASS vim-insert-visible      # text inserted in vim appears on the grid
[auto] PASS vim-alt-screen-exit     # :wq restores main screen, mode cleared
[auto] PASS vim-file-roundtrip      # file written by vim, cat output verified
[auto] SKIP htop                    # not installed on this machine
[auto] PASS resize-sigwinch         # backend.resize -> Term::resize + Msg::Resize; `stty size` confirms
[auto] flood: engine-wakeups=1186 batched-to-notifies=118   # yes(1) 600ms: 10x coalescing, UI alive
[auto] PASS scrollback-grew / scroll-display   # scroll_display(3) -> display_offset=3
[auto] damage: snapshots=26 full=6 partial-lines=44         # Term::damage() works; v2 option viable
[auto] snapshot-copy: 122x58 grid, avg over 100 iters = 141.33µs
[auto] child-exit-event: received, clean   # `exit` -> ChildExit(code) -> shutdown, no thread leak
```

Unit tests (`cargo test`, display-only `Term` fed known ANSI byte streams):
plain text + cursor position, 16/256/truecolor resolution, style flags,
INVERSE swap, wide-char spacer, alt-screen + APP_CURSOR + bracketed-paste
modes, scrollback + `scroll_display`, resize reflow (content preserved via
scrollback), cursor shapes/hide, escape sequences split across reads,
combining chars (`zerowidth()`); plus 12 key-encoding tests (arrows w/ and
w/o APP_CURSOR, Ctrl/Alt/Shift CSI params, Home/End, editing keys, F1–F12,
backtab, bracketed paste wrapping).

Research-doc §5 checklist status:

- [x] `alacritty_terminal = "=0.26.0"` + GPUI compiles on macOS, crates.io
  (no fork, no patch); Cargo.lock committed. (Linux build untested — no
  Linux box here.)
- [x] PTY lifecycle: spawn, env, initial size, `ChildExit` → clean
  shutdown (Msg::Shutdown + join; PTY master drop SIGHUPs child).
- [x] Wakeup → `cx.notify` full path; coalescing 4 ms / 100 events
  measured working (1186 wakeups → 118 notifies under `yes` flood).
- [x] DEC 2026 sync output: **route A already handles it** — alacritty's
  own `EventLoop` suppresses wakeups inside synchronized-output windows
  (`sync_bytes_count() < processed` gate in `event_loop.rs`), so no extra
  work needed unless we move to route B for other reasons (OSC scan).
- [x] Damage experiment: `Term::damage()`/`reset_damage()` work as
  documented (6 full / 44 partial-lines over 26 snapshots in the suite);
  full-snapshot v1 costs 141 µs per 122×58 frame, so damage-based v2 is
  optional, not required.
- [x] Rendering correctness (engine side): colors, flags, wide chars,
  combining chars, cursor shapes/visibility. (Paint-side verified only as
  "canvas paint executes, no panic" — see human pass below.)
- [x] Resize: rows/cols recompute from GPUI font metrics + viewport;
  `stty size` proves SIGWINCH reaches the child; vim reflows on real
  window resize (engine reflow preserves content into scrollback —
  unit-tested).
- [x] Scrollback: `scroll_display`/`display_offset`, mouse wheel, alternate
  scroll (wheel → arrow keys in alt screen without mouse reporting).
- [x] Input: full key encoder written (the research doc's "most
  underestimated chunk") — unit-tested incl. APP_CURSOR and CSI modifier
  params; bracketed paste per TermMode; application PtyWrite replies
  forwarded.
- [x] display-only Term (log-stream reuse, §4): implemented as
  `AlacrittyBackend::display_only` + `feed_output()` behind the same
  boundary; it *is* the unit-test harness, and it renders through the
  same GPUI canvas pipeline as PTY sessions.
- [x] High-throughput follow (simulated `kubectl logs -f` at 10k lines/s,
  60 s sustained): UI responsive, memory bounded — see the throughput
  section below.
- [x] Split/truncated sequences: unit-tested, plus 60 s of 4 KiB chunks
  splitting escape sequences at arbitrary byte offsets without a panic.

Remaining for the human interactive pass:

- [ ] Visible-window run: draw timings, `SPIKE_LATENCY_PROBE=1`
  keystroke→paint samples (this machine's display was occluded/asleep
  during the agent run — GPUI delivered no frames after startup).
- [ ] IME input (GPUI InputHandler path — not wired in the spike).
- [ ] OSC 52 clipboard store/load (code path exists, untested).
- [ ] Mouse: SGR 1006 encoding, click-to-position (htop), text selection +
  copy (`Term.selection`/`selection_to_string` not exercised).
- [ ] OSC 8 hyperlinks (exposed by engine; not rendered/clickable).
- [ ] Block-character (U+2580–259F) seamless rendering — the Paneflow
  pitfall; needs visual check with a fallback-font coverage table.
- [ ] Multi-session (≥4 terminals) CPU/memory; title/bell UI wiring
  (events arrive, printed only); regex search (`term::search` not wired).
- [ ] htop/btop interactive run (skipped: not installed).
- [ ] Linux build.

## Measured numbers (this machine, release build)

- Full `renderable_content()` snapshot copy, 122×58 grid: **125–141 µs**
  avg (100 iters, across runs) — v1 "full snapshot per frame" is cheap;
  shaping/paint cost needs the visible-window pass.
- Wakeup coalescing under `yes` flood (600 ms): 1186 engine wakeups → 118
  GPUI notifies (~10×), UI responsive, clean ctrl-c afterwards.
- Scripted suite wall time (shell → vim → resize → flood → exit): ~13 s.

### Log-stream throughput (`SPIKE_AUTO=log-flood`)

Simulated `kubectl logs -f`: colored ANSI log lines (~110–140 B each) at a
paced 10k lines/s for **60 s sustained**, fed to the **display-only
backend (no PTY)** in 4 KiB chunks (arbitrary split points, so escape
sequences cross chunk boundaries exactly like real pipes). Scrollback
bound: **10,000 lines**. RESULT: PASS (0 failures).

```
[log-flood] fed: 599528 lines / 95.9 MiB in 60.0s = 9984 lines/s (1.6 MiB/s), 23981 chunks
[log-flood] parse: feed_output total=4.13s avg=172.37µs/chunk max=2.51ms
            -> engine parse throughput 23.2 MiB/s
[log-flood] coalescing: 23981 wakeups -> 6411 notifies (3.7x)
[log-flood] snapshot-under-load: n=29 avg=399.17µs max=2.24ms (grid 122x58);
            post-flood burst max=617.00µs
[log-flood] rss: before=66 MB after=99 MB (delta=33 MB), history=10000 lines (bound=10000)
```

Reading the numbers:

- **Feed/parse**: the engine parses colored ANSI at **23 MiB/s** — 14×
  headroom over the 1.6 MiB/s stream; parse consumed 6.9% of wall time on
  the feeder thread. Slowest single 4 KiB feed: 2.51 ms (no feed stall).
- **UI responsiveness** (window occluded here, so measured by proxy): a
  timed `snapshot()` is exactly what the paint path does per frame; under
  full load it averaged **399 µs** and peaked at **2.24 ms** — FairMutex
  contention between feeder and render side never approached a 16 ms
  frame. Post-flood burst max 617 µs.
- **Coalescing**: 3.7× wakeup reduction at ~400 wakeups/s (the 4 ms
  window batches less than during a burst flood; notifies ≈ 107/s, each
  ~0.4 ms of work — idle-safe and busy-safe).
- **Memory**: +33 MB over 60 s of 600k lines, plateaued exactly at the
  10,000-line scrollback bound (history=10000 at the end). The retained
  grid is ~10k × 122 cells; RSS stays bounded regardless of stream
  duration. (No per-line heap leaks visible in the delta.)
- The dumped grid tail shows clean, correctly soft-wrapped colored log
  lines — no mangling from 60 s of mid-sequence chunk splits.

## Interface sketch (the deliverable for the real terminal issue)

`src/backend/mod.rs` is the boundary; `src/backend/alacritty.rs` is the
only module allowed to name alacritty/vte types. Route A (alacritty's
`EventLoop`) is used; route B (hand-written reader/writer threads à la
Paneflow) can replace it behind the same trait — the PTY threads are
plain sync threads either way, no tokio.

```rust
pub trait TerminalBackend: Send + Sync + 'static {
    fn write_input(&self, bytes: &[u8]);                       // user -> child
    fn resize(&self, cols: usize, rows: usize, cw: u16, ch: u16); // grid + PTY (SIGWINCH)
    fn scroll_display(&self, delta_lines: i32);                // viewport only
    fn scroll_to_bottom(&self);
    fn snapshot(&self) -> RenderSnapshot;                      // render-ready copy
    fn feed_output(&self, _bytes: &[u8]) {}                    // display-only: log stream in
    fn modes(&self) -> TerminalModes;                          // input-encoding modes
    fn respond_clipboard_load(&self, _text: &str) {}           // OSC 52 load reply
    fn stats(&self) -> BackendStats { .. }                     // wakeup/damage counters
    fn shutdown(&self);                                        // idempotent
}

pub enum TerminalEvent {        // engine -> UI (own types only)
    Wakeup, Title(String), ResetTitle, Bell,
    PtyWrite(Vec<u8>),          // application replies (DA etc.) -> forward to PTY
    ClipboardStore { clipboard: ClipboardKind, text: String },
    ClipboardLoad { clipboard: ClipboardKind },
    ColorRequest { index: usize },
    ChildExit(ExitStatus), Exit, CursorBlinkingChange, MouseCursorDirty,
}

pub struct RenderSnapshot {     // one frame of paint data
    cols, rows: usize,
    cells: Vec<SnapshotCell>,   // row-major; char + combining chars + resolved
                                // Rgba fg/bg (palette/inverse/DIM already applied)
    cursor: Option<SnapshotCursor>,  // col/row + Block/Underline/Beam/Hollow/Hidden
    display_offset, history_size: usize, alt_screen: bool,
}

pub struct TerminalModes {      // what input encoding needs
    alt_screen, app_cursor, app_keypad, bracketed_paste,
    show_cursor, mouse_report, sgr_mouse, line_wrap: bool,
}
```

Data flow:

```
PTY reader (alacritty EventLoop thread, sync, polling)
  -> vte advance under FairMutex<Term>  (wakeups suppressed in DEC 2026 windows)
  -> SharedListener -> mpsc<TerminalEvent>
GPUI main thread: background-recv -> drain <=4ms / <=100 events
  -> ONE cx.notify() per batch
  -> canvas prepaint: backend.snapshot() (141µs) -> shape_line per row
  -> canvas paint: bg quads (merged runs) -> block cursor -> text -> beam/underline/hollow
Input: on_key_down -> keys::encode_key(Key, Mods, &TerminalModes) -> write_input
Resize: render() sees viewport change -> backend.resize -> Term::resize + Msg::Resize
```

Key encoding lives in `src/backend/keys.rs` as pure functions over own
types (`Key`, `Mods`) — alacritty_terminal deliberately does not include
it; this file is directly reusable.

## Verdict: FEASIBLE

- The full closed loop works end-to-end with production-grade margins:
  PTY → vte → grid → snapshot → GPUI canvas, plus input, resize
  (SIGWINCH-verified), scrollback, alt screen (scripted vim), flood
  coalescing, and clean shutdown — all green in one scripted run.
- 141 µs full-snapshot copies make Zed's "full snapshot per frame" v1
  strategy obviously viable; `Term::damage()` is confirmed available for
  a v2 optimization but is not needed.
- Route A suffices to ship: DEC 2026 sync-output wakeup suppression is
  already inside alacritty's `EventLoop`, so Paneflow's route B is only
  needed if we want OSC scanning (OSC 7 CWD / OSC 133 prompt boundaries)
  — and it slots in behind the same trait.
- The engine boundary (`TerminalBackend` + own types) cost nothing extra
  and is the interface the real terminal work should start from.
- Known gaps are all enumerated above and none are architectural: IME,
  mouse, selection, OSC 52/8, multi-session, and a visible-window paint
  pass remain for the human.

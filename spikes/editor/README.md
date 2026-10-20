# editor-spike — PROTOTYPE / THROWAWAY

**This is a one-shot performance spike, not production code. Do not build on
it, do not merge it into anything real.** Self-contained crate (own
`Cargo.toml`/`Cargo.lock`, no workspace at the repo root).

## Question it answers

GitHub issue jasonwun/Astrolabe#6: does gpui-kit v0.8.0's code `Editor`
(`EditorState` + `Editor`, tree-sitter highlighting) stay fluid on a giant
real-world YAML file — open, scroll, edit, YAML syntax highlighting,
undo/redo, and YAML indent continuation?

## Test data

`data/postgresclusters-crd.yaml` — the real published CrunchyData
postgres-operator `PostgresCluster` CRD:

- Source: <https://raw.githubusercontent.com/crunchydata/postgres-operator/main/config/crd/bases/postgres-operator.crunchydata.com_postgresclusters.yaml>
- 2,872,552 bytes (2.87 MB), 38,382 lines — exceeds the 2 MB / ~20k-line
  target from the roadmap. (Istio's `crd-all.gen.yaml`, the usual candidate,
  is only ~975 KB / ~20k lines; no single published CRD ≥ 2 MB was found in
  Istio, Crossplane/upjet, Strimzi, or ACK — this one is the biggest real one
  located.)

## Run

```sh
cd spikes/editor
cargo run --release          # interactive: editor + metrics overlay
SPIKE_AUTO=1 cargo run --release   # scripted suite: prints results, quits
SPIKE_AUTO=1 SPIKE_STAY=1 cargo run --release  # suite, then keep window
```

First build of GPUI is heavy (several minutes). **The window must be visible
on screen** — macOS delivers no draw callbacks to occluded windows or while
the display is asleep, so there are no frame timings to collect. The suite
waits up to 15 s for presented frames at startup and bails with a clear
message if the window never becomes visible. (`caffeinate -u` wakes a sleeping
display.) Interactive keys: ctrl-1 load, ctrl-2 scroll, ctrl-3 edit,
ctrl-4 undo/redo, ctrl-5 indent test, ctrl-0 run all.

## What it measures

- **load** — file read, `set_value` wall time, and time from `set_value` to
  the first presented frame (gpui frame trace via
  `gpui_kit::profiler::FrameTimingCollector`, enabled by the `profiler`
  feature + `set_trace_enabled(true)`).
- **highlight** — standalone `SyntaxHighlighter::new("yaml")` on the full
  file: full parse, style extraction for the first 100 KB, incremental
  reparse after a 1-char mid-document edit, and the same under the editor's
  2 ms sync-parse budget.
- **scroll** — five jump cuts across the document + jump-to-end (draw
  timings only; the deliberate 100 ms idles between jumps pollute present
  gaps), then a 60× page-down burst at 16 ms pacing (draw + present gaps).
- **edit** — 90 timed inserts (single chars and whole lines at top/middle/
  end) plus a 60× backspace burst; per-insert wall latency and frame stats.
- **undo/redo** — 50 atomic inserts, then 50 undos, then 50 redos (dispatched
  as real actions); byte length is checked to prove the document returns to
  baseline and back.
- **indent** — on a small scratch buffer, Enter after `key: value`,
  `key:` at two depths, and `- ` list items; the resulting line's indentation
  is read back from the editor state.

The first present gap of each phase is dropped before reporting: it measures
from the last present of the deliberate idle before the phase, not from a
stutter.

## Measured results (Apple M3 Max, macOS, release build, 2026-10-19)

```
[spike] data 2872552 bytes, 38382 lines
[load] read=599µs set_value=15.81ms first_present=10.61ms frames=1 draw=10.08ms
[load] rss after load + background parse: 163 MB
[highlight] full parse (no timeout): 77.86ms completed=true tree=true
[highlight] styles for first 100KB: 1.77ms, 3422 runs, 3 distinct colors
[highlight] incremental reparse after 1-char edit: 118.88µs completed=true
[highlight] reparse with 2ms budget (editor's sync timeout): 94.83µs completed=true
[scroll] jumps=5+end draw_avg=1.90ms draw_worst=5.11ms
        | pagedown-burst=60 frames=60 draw_avg=3.20ms draw_worst=5.20ms gap_worst=18.69ms gaps>33ms=0
[edit] inserts=90 edit_avg=78µs edit_p50=69µs edit_worst=199µs
      | insert-burst frames=68 draw_avg=1.66ms draw_worst=4.41ms gap_worst=11.23ms gaps>33ms=0
      | delete-burst=60 frames=60 draw_avg=1.17ms draw_worst=1.48ms gap_worst=16.78ms gaps>33ms=0
[undo/redo] n=50 undo_wall=550ms (11ms/op incl. 10ms pacing) redo_wall=552ms
            len baseline=2873062 after_inserts=2874052 after_undos=2873062 after_redos=2874052
           | undo-burst frames=50 draw_avg=1.53ms draw_worst=3.74ms gap_worst=18.89ms gaps>33ms=0
           | redo-burst frames=50 draw_avg=1.42ms draw_worst=2.49ms gap_worst=17.78ms gaps>33ms=0
[indent] all 6 cases OK (see below)
```

## Conclusion: PASS

Opening a 2.87 MB / 38,382-line CRD costs ~16 ms of main-thread work
(`set_value`) plus one 10 ms frame; scrolling, typing, deleting, and 50-deep
undo/redo all draw in 1–6 ms per frame with zero present gaps over 33 ms —
comfortably fluid, well inside a 16.6 ms frame budget.

### Q1 — tree-sitter YAML highlighting cost

Cheap, and never on the critical path:

- Full parse of the whole 2.87 MB file: **~78 ms**, run once. In the editor
  this never blocks the UI: documents over 256 KB skip the synchronous parse
  entirely (`SYNC_PARSE_MAX_BYTES` in
  `gpui-component/src/highlighter/input_adapter.rs`) and go straight to a
  **debounced (150 ms) background-thread parse**; styles are served from the
  stale tree until it lands. So highlighting simply fades in ~250 ms after
  load.
- Incremental reparse after an edit: **~120 µs** — fast enough that it would
  fit even the 2 ms sync budget, but the >256 KB gate still routes this file
  through the background path on every edit. Per-keystroke cost is
  effectively invisible either way.
- Style extraction is per-visible-range at render time (~1.8 ms per 100 KB
  worst case measured; visible ranges are far smaller).

There is no user-visible "degraded" mode beyond this: stale-tree highlighting
during the debounce window is the degradation strategy, and it works.

### Q2 — fallbacks

Not needed. No performance problem showed up at 2.9 MB / 38k lines, so
read-only viewer splits, segmented loading, or forking are unnecessary. (Note:
`EditorState::new`'s own doc comment says the editor targets "large text up
to about 50K lines" — this file at 38k lines is near that stated ceiling;
a multi-MB file with far more lines might behave differently.)

### Q3 — YAML indent continuation

Works, with one caveat. Registering a YAML `LanguageConfig` is required —
gpui-kit's built-in defaults cover only text/json/python. The spike registers:

```rust
set_language_config(
    "yaml",
    LanguageConfig::default().indentation_rules(IndentationRules::new(
        regex::Regex::new(r"[\{\(\[:]\s*$").unwrap(),
        regex::Regex::new(r"^\s*[\}\)\]]").unwrap(),
    )),
    cx,
);
```

Measured behavior of Enter (cursor at end of the line):

| Line under cursor                  | New line indent | Verdict |
| ---------------------------------- | --------------- | ------- |
| `key: value`                       | same level      | OK      |
| `spec:` (ends with `:`)            | +1 level        | OK      |
| nested `containers:` (indent 6)    | 8 (+1 level)    | OK      |
| `- name: app` (list item)          | same indent (8) | OK      |
| `- name: app` at EOF               | same indent (8) | OK      |
| `- name:` (list item, ends `:`)    | 10 (+1 level)   | OK      |

Caveat: there is **no `- ` list-item continuation** — Enter after a list item
yields a blank line at the item's indent; the user types the next `- `
manually. That is how `indent_of_next_line_at` in gpui-base works (regex-based
indent rules only); whether Astrolabe wants bullet continuation is a product
decision, not a performance one. Also note the quirk that Enter prefers the
*next* line's indent when it is deeper than the current one.

## API findings for the implementation work

- `EditorState::new(window, cx)` (code-editor mode: line numbers, indent
  guides, search on; tab size 2 default) + `.language("yaml")` builder, then
  `Editor::new(&state)`. The component `Editor` renders through `Input`, which
  installs the tree-sitter highlighter factory and theme — no extra wiring.
- `set_highlighter(name, cx)` / `set_value(text, window, cx)` for open-file
  flows; `set_value` clears undo history, `replace_all` preserves it.
- `insert` / `replace` are synchronous and measurable (microseconds at this
  file size); each `insert` is its own undo unit (`EditIntent::Atomic`).
- Undo/redo/Enter/movement are actions (`Undo`, `Redo`,
  `Enter { secondary, shift }`, `MovePageDown`, …) dispatched on the focused
  editor — `window.dispatch_action(Box::new(Undo), cx)`.
- YAML editing rules: `gpui_kit::component::input::set_language_config`
  (re-export of `gpui_base::input::set_language_config`) +
  `LanguageConfig::default().indentation_rules(...)`; `LanguageConfig` for
  editing lives at `component::input::language_config::LanguageConfig` — a
  different type from `component::highlighter::LanguageConfig` (grammar
  registration). Don't confuse them.
- The `tree-sitter-yaml` feature flag links the grammar; `LanguageRegistry`
  maps `yml` → `yaml` automatically.
- Frame metrics: `profiler` feature + `profiler::set_trace_enabled(true)` +
  `FrameTimingCollector`. Samples are dropped while the window is hidden or
  the system is suspended (by design), so benchmark harnesses need a visible
  window.

# datatable-spike — PROTOTYPE / THROWAWAY

**This is a one-shot performance spike, not production code. Do not build on
it, do not merge it into anything real.** Self-contained crate (own
`Cargo.toml`/`Cargo.lock`, no workspace at the repo root).

## Question it answers

Does gpui-kit v0.8.0's `DataTable` (uniform-list row body, uniform row height)
stay smooth with ~10,000 Pod rows while a synthetic Kubernetes-style watch
stream pushes incremental updates at 20 Hz for minutes, while the user
scrolls, sorts, and selects? And does coalescing the mutation stream into
wider application windows matter?

## Run

```sh
cd spikes/datatable
cargo run --release
```

First build of GPUI is heavy (several minutes). Quit with the window's close
button or Ctrl-C the terminal.

## What it does

- Generates 10,000 fake Pod rows (name, namespace, status, restarts, cpu,
  memory, age, node).
- A 50 ms timer (20 Hz) generates a batch of 1–4 mutations per tick:
  ~65% row updates (status flips, cpu/memory jitter, restarts+1),
  ~15% adds, ~15% deletes (biased so the row count oscillates around 10k
  instead of drifting). Runs forever.
- Mutations are generated every tick but **applied** to the table delegate
  (`delegate_mut()` + `cx.notify()`) only when the current batching window
  elapses.

## Keys

| Key | Batching window                                  |
| --- | ------------------------------------------------ |
| `1` | 50 ms — apply every tick as-is (default)         |
| `2` | 100 ms — coalesce 2 ticks per table update       |
| `3` | 250 ms — coalesce ~5 ticks                       |
| `4` | 500 ms — coalesce ~10 ticks                      |

Column headers click-sort (asc/desc/off; comparators in
`PodsDelegate::perform_sort`). Sorting is applied once per click — streamed
mutations do not re-sort (fine for the spike). Rows/columns are selectable and
columns are resizable/movable (TableState defaults).

## Metrics overlay (top bar)

- `fps` — window presents in the last 1 s, from gpui's frame trace
  (`FrameTimingCollector`, enabled via gpui-kit's `profiler` feature plus
  `profiler::set_trace_enabled(true)` at startup). This is the real
  presented-frame rate, not a timer approximation. Note: gpui only draws when
  invalidated, so with no pointer interaction this number equals the flush
  rate (~19–20 at 50 ms mode); while scrolling it should jump to the display
  refresh rate.
- `draw avg / worst (1s)` — time gpui spent in `Window::draw`, mean and max
  over the last 1 s of frames.
- `gap worst` — largest present-to-present gap in the last 1 s; the jank
  metric (a stutter is a big gap, even when draws are fast).
- `rows` — current row count.
- `applied` — cumulative applied mutations (adds+updates+deletes), plus the
  size of the last flushed batch.
- `batch Nms` — current batching mode.
- `rss` — process resident memory, sampled ~1/s via mach `task_info()`.

Every 30 s a stats line is printed to stdout (rows, generated/applied counts,
flush count, max batch size, frame stats, RSS), so a 5-minute run leaves a
record in the terminal.

## Pass criterion

No visible jank while scrolling and sorting during the update storm; watch
`gap worst` and `draw worst` for the objective version. The 1–4 keys answer
whether coalescing window size moves those numbers.

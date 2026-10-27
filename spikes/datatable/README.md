# datatable-spike

A performance spike, not production code: build nothing on it. A workspace member
so it tracks the pinned dependencies; rerun it on every dependency
upgrade (`docs/agents/spike-regression.md`).

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

### Scripted mode

```sh
SPIKE_AUTO=1 cargo run --release            # 300 s
SPIKE_AUTO=1 SPIKE_SECS=60 cargo run --release
```

Runs the same watch storm at the 50 ms window and drives the table from
inside the app, one step every 100 ms. A cycle is 8 page-downs (30 rows
each), one sort step, 4 page-ups and 6 single-row downs; the selection wraps
at the last row. The sort step walks every column through asc, desc and off,
one state per cycle (24 cycles per sweep); it sorts the rows as a header
click does, but the header arrows do not change. Steps run at fixed offsets from the
start, so a run does `SPIKE_SECS / 1.9 s` cycles (157 for 300 s) on any
machine. The window must be visible: an occluded window presents no frames.

It keeps the `[spike t=…]` lines, then prints an `[auto] summary:` line
(cycles, flushes, applied mutations, whole-run `draw_avg` / `draw_worst` /
`gap_worst`, the count of gaps over the limit, `late_worst` for how far the
schedule fell behind, RSS start→end) and `[auto] RESULT: PASS|FAIL`, then
quits. FAIL (exit 1) when the whole-run `gap_worst` exceeds 250 ms. #76's
1 s samples sat at 58–65 ms on 0.8.0 and 0.8.1 at the 50 ms flush cadence.
The whole-run worst is noisier: four 300 s runs on 0.8.1 gave 85, 92 and
151 ms, and on a loaded machine one 218 ms gap (`late_worst` 49 ms: the
whole main thread stalled). 250 ms is five missed flushes. Keep other heavy work off the
machine during an A/B.

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
flush count, max batch size, frame stats over the last 1 s, RSS), so a
5-minute run leaves a record in the terminal.

## Pass criterion

No visible jank while scrolling and sorting during the update storm; watch
`gap worst` and `draw worst` for the objective version. The scripted mode's
`RESULT` line is that check. The 1–4 keys answer
whether coalescing window size moves those numbers.

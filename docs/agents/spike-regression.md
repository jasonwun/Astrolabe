# Spike regression (dependency upgrades)

A dependency-upgrade PR attaches an A/B of the three spikes in `spikes/`
(README "Dependency upgrades"). Done when every row of the table below
has a baseline and a candidate number from the same machine and session,
and the PR body carries that table with a verdict per spike.

## 1. Two trees

Baseline is `main` before the bump, candidate is your branch. Spikes are
workspace members, so the bump reaches them through `[workspace.dependencies]`.

Build both trees into this tree's `target/`, so the baseline reuses every
dependency the bump leaves alone: an unchanged tree rebuilds in seconds,
against minutes for a first GPUI build.
Both trees then write the same `target/release/<name>-spike`, so copy
each tree's binaries out right after its build:

```sh
git worktree add "$SCRATCH/base" origin/main   # baseline
target="$PWD/target"                            # from the candidate's root
(cd "$SCRATCH/base" && CARGO_TARGET_DIR="$target" cargo build --release \
  -p editor-spike -p terminal-spike -p datatable-spike)
mkdir -p "$SCRATCH/bin/base" && cp target/release/{editor,terminal,datatable}-spike "$SCRATCH/bin/base/"
cargo build --release -p editor-spike -p terminal-spike -p datatable-spike
mkdir -p "$SCRATCH/bin/cand" && cp target/release/{editor,terminal,datatable}-spike "$SCRATCH/bin/cand/"
```

Time the copies, not `cargo run`: a queued `cargo run` rebuilds when a
source changes mid-session, so a later run measures a different tree. A
copy keeps measuring its own build while you edit. It finds its data by
absolute path into its tree, so it runs from any directory while the
worktree exists.

Run baseline and candidate back to back: numbers from an
earlier day (the spike READMEs' own) differ from today's on both versions,
so they are no baseline.

## 2. Run

With `BIN=$SCRATCH/bin/base`, then `BIN=$SCRATCH/bin/cand`:

| Spike | Command | Record |
|---|---|---|
| editor | `SPIKE_AUTO=1 $BIN/editor-spike`, twice | `[load] set_value`, `[scroll] pagedown-burst draw_avg`, `[edit] edit_avg`, `gaps>33ms` per burst |
| terminal suite | `SPIKE_AUTO=1 $BIN/terminal-spike` | `RESULT: PASS (N failures)`, `[auto] flood draw_avg`, `snapshot-copy` |
| terminal log-flood | `SPIKE_AUTO=log-flood $BIN/terminal-spike` (60 s) | the five PASS criteria, parse MiB/s, snapshot under load, RSS delta |
| datatable | `SPIKE_AUTO=1 $BIN/datatable-spike` (300 s) | `RESULT`, and from `[auto] summary:` `cycles`, `draw_avg`, `draw_worst`, `gap_worst`, `gaps>100ms`, `rss`, `load` |

Keep every spike window on screen while it runs: an occluded window
presents no frames. The datatable run does the same cycle count on both
trees, so its numbers compare directly.

Check machine load before each run (`uptime`) and in the run's own
`load=` figures (start→end in the datatable summary, editor
`suite complete` and terminal log-flood lines; the terminal suite, a few
seconds long, prints its start only). Other agent sessions share the machine and skew frame timings: on
this 16-core Mac quiet runs start near load 3, and a run at load 13 stalled
the main thread for 218 ms. Run baseline and candidate at similar, quiet
load; rerun a pair whose loads differ widely.

## 3. Read

- terminal and datatable print PASS/FAIL; a FAIL is a regression.
- datatable: FAIL means more jank frames (gaps over 100 ms) than the
  README's limit. Sampled over 1 s at the default 50 ms batch window,
  `gap_worst` sits near 60 ms (the flush cadence) and `draw_worst` stays well under 16 ms apart from rare outliers;
  RSS stays flat around 10k rows.
- A slower metric that still costs no frames (`gaps>33ms` unchanged) is
  noted in the PR, not a blocker. #76 is the reference write-up.
- terminal log-flood: parse MiB/s and snapshot under load follow the CPU
  clock, not just the engine. The paced feed is light enough that the
  clock tracks the whole process's load, so a candidate that does less
  work elsewhere parses slower (gpui-fast: ~30 vs ~47 MiB/s). Before
  calling a drop a regression, rerun both trees with a busy neighbour
  (`yes > /dev/null &`, killed after the run): if the gap closes, it is
  clock, not engine (#92).

## Adding a pass criterion

A PASS/FAIL criterion over a whole run (a count of jank frames, a worst
gap) needs a whole-run baseline: three runs on a quiet machine, recorded in
the spike's README with the limit and why it was picked. 1 s overlay
samples say nothing about a run's worst case. The datatable summary prints
gap counts at several limits (`gaps>66ms` … `gaps>250ms`) so a new limit
can be read off runs already made; datatable README "Scripted mode" is the
reference (#78).

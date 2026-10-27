# Spike regression (dependency upgrades)

A dependency-upgrade PR attaches an A/B of the three spikes in `spikes/`
(README "Dependency upgrades"). Done when every row of the table below
has a baseline and a candidate number from the same machine and session,
and the PR body carries that table with a verdict per spike.

## 1. Two trees

Baseline is `main` before the bump, candidate is your branch. Spikes are
workspace members, so the bump reaches them through `[workspace.dependencies]`.

```sh
git worktree add "$SCRATCH/base" origin/main   # baseline
```

Build both with `--release` before timing anything; the first GPUI build
takes minutes. Run baseline and candidate back to back: numbers from an
earlier day (the spike READMEs' own) differ from today's on both versions,
so they are no baseline.

## 2. Run

From `spikes/<name>` in each tree:

| Spike | Command | Record |
|---|---|---|
| editor | `SPIKE_AUTO=1 cargo run --release`, twice | `[load] set_value`, `[scroll] pagedown-burst draw_avg`, `[edit] edit_avg`, `gaps>33ms` per burst |
| terminal suite | `SPIKE_AUTO=1 cargo run --release` | `RESULT: PASS (N failures)`, `[auto] flood draw_avg`, `snapshot-copy` |
| terminal log-flood | `SPIKE_AUTO=log-flood cargo run --release` (60 s) | the five PASS criteria, parse MiB/s, snapshot under load, RSS delta |
| datatable | `SPIKE_AUTO=1 cargo run --release` (300 s) | `RESULT`, and from `[auto] summary:` `cycles`, `draw_avg`, `draw_worst`, `gap_worst`, `gaps>100ms`, `rss`, `load` |

Keep every spike window on screen while it runs: an occluded window
presents no frames. The datatable run does the same cycle count on both
trees, so its numbers compare directly.

Check machine load before each run (`uptime`) and in the run's own
`load=` figures at start and end (datatable summary, editor
`suite complete`, terminal suite and log-flood lines). Other agent sessions share the machine and skew frame timings: on
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

## Adding a pass criterion

A PASS/FAIL criterion over a whole run (a count of jank frames, a worst
gap) needs a whole-run baseline: three runs on a quiet machine, recorded in
the spike's README with the limit and why it was picked. 1 s overlay
samples say nothing about a run's worst case. The datatable summary prints
gap counts at several limits (`gaps>66ms` … `gaps>250ms`) so a new limit
can be read off runs already made; datatable README "Scripted mode" is the
reference (#78).

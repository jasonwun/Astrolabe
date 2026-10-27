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
| datatable | `cargo run --release`, then `./drive.sh 300` | the `[spike t=…]` lines: fps, `draw_avg`, `draw_worst`, `gap_worst`, `rss` |

The datatable spike has no scripted mode: `drive.sh` pages and sorts it
through `orca computer` (`docs/agents/manual-check.md`). Its cycle count
varies run to run, so compare ranges, not single samples.

## 3. Read

- editor and terminal print PASS/FAIL; a FAIL is a regression.
- datatable: at the default 50 ms batch window `gap_worst` sits near 60 ms
  (the flush cadence) and `draw_worst` stays well under 16 ms apart from
  rare outliers; RSS stays flat around 10k rows.
- A slower metric that still costs no frames (`gaps>33ms` unchanged) is
  noted in the PR, not a blocker. #76 is the reference write-up.

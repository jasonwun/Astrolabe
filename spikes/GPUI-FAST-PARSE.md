# RESEARCH — terminal parse throughput on GPUI Fast (#92)

Throwaway branch `research/gpui-fast-parse-throughput` (off `prototype/gpui-fast`); never merged.
It adds `spikes/terminal/src/probe.rs`, wired into `feed_output`, `snapshot` and the log-flood report:

- `Term` lock **wait** vs **hold** per feed, and the feeder thread's CPU time over the hold;
- a fixed register-only ALU loop (`calibrate`, 20k mul-adds) after every feed: its time tracks the core's clock;
- `SPIKE_FEEDER_QOS=ui` pins the feeder to user-interactive QoS; `SPIKE_FEED_UNPACED=1` drops the 10k lines/s pacing.

The same patch was applied to `main` 0ba80ff (gpui-pre 0.3.10) for the baseline.
All runs `SPIKE_AUTO=log-flood`, 60 s, release, same session 2026-10-26, M3 Max, load 2–3.

## Results

| Run | gpui-pre parse MiB/s | gpui-fast parse MiB/s | calib µs (pre / fast) |
|---|---|---|---|
| default (×3 / ×3) | 46.5 / 46.2 / 47.7 | 30.0 / 29.1 / 30.7 | 74.2 / 91.8 |
| `GPUI_VIEW_RETENTION=0` | — | 29.2 | — |
| feeder QoS user-interactive | 49.1 | 33.0 | 71.7 / 85.2 |
| unpaced (feeder 100% busy) | 181.8 | 168.3 | 31.0 / 32.3 |
| paced + one `yes > /dev/null` neighbour | 160.7 | 155.6 | 35.3 / 35.7 |

Lock split, default runs (of ~2.0 s / ~3.1 s total `feed_output`):

| | gpui-pre | gpui-fast |
|---|---|---|
| lock wait | 25–38 ms | 50–75 ms |
| lock hold (parse) | 1888–1948 ms | 2898–3049 ms |
| feeder CPU time over hold | 1812–1855 ms | 2638–2699 ms |
| snapshot hold avg (all, incl. render) | 114–117 µs | 154–160 µs |

With the busy neighbour, snapshot under load is 51.1 vs 51.4 µs.

## Reading

- **Not lock contention**: wait is ~2% of feed time on both; the gap is in the hold, and it is CPU time, not preemption.
- **Not Retained Mode**: `GPUI_VIEW_RETENTION=0` leaves gpui-fast at 29 MiB/s.
- **Clock (DVFS)**: the same parse code takes 1.45× the CPU time, and a loop with no memory traffic is 1.2× slower too, so the core the feeder lands on runs at a lower clock.
  Whenever something keeps the clocks up (the feeder itself unpaced, or any busy neighbour), both backends parse at ~160–180 MiB/s and the gap closes to 3–8%.
  The paced flood (~85 µs of work every ~3 ms) is light enough that the clock follows whole-machine demand; gpui-fast retires 41% fewer instructions (#74), so the clock settles lower.
- QoS user-interactive on the feeder does not change it (33 vs 49 MiB/s).
- Core placement could not be read from user space: TPIDR_EL0 / TPIDRRO_EL0 do not carry a usable CPU id on this macOS; `powermetrics` needs root.
- Not explained: the higher sys time in #74 (11.9 vs 7.1 s).

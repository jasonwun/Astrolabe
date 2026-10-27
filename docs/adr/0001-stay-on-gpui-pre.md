# Stay on gpui-pre; revisit GPUI Fast when it leaves experimental

gpui-kit can run on GPUI Fast (Longbridge's fork of Zed's GPUI, with Retained Mode) through its `gpui-fast` Cargo feature instead of the default `gpui-pre` snapshot. We stay on `gpui-pre`. We revisit when GPUI Fast is no longer marked experimental upstream.

The prototype ([Spike: GPUI Fast backend via gpui-kit's gpui-fast feature](https://github.com/jasonwun/Astrolabe/issues/74), A/B in `spikes/GPUI-FAST.md` on `prototype/gpui-fast`) found no blocker. All three spikes pass on both backends, `cargo test --workspace` passes, and the manual check showed no stale frames. DataTable draw_avg went from 11.5 to 8.0 ms and frame gaps over 66 ms from 663 to 262. The lower log-flood parse throughput comes from the CPU clock, not from GPUI Fast ([Why terminal parse throughput drops on GPUI Fast](https://github.com/jasonwun/Astrolabe/issues/92)).

The gain is headroom we don't need: `gpui-pre` already meets every pass criterion. The supply side counts against switching now. Upstream calls GPUI Fast experimental. It shipped 12 releases in 20 days (0.1.0 → 0.1.12). Its GitHub issues and discussions are disabled. gpui-kit 0.8.1 depends on it with a caret requirement, so only `Cargo.lock` would pin it.

## Considered options

- **Adopt now**, pinning `gpui-fast = "=0.1.12"` directly in the workspace. Rejected for the supply-side reasons above.
- **Optional feature, off by default, both backends in CI.** Rejected: a doubled CI matrix for a backend we don't ship.

## Consequences

- Switching later stays a one-line feature change, so this is a two-way door.
- Views written meanwhile are not checked against Retained Mode. Retained Mode requires a `cx.notify()` whenever render state changes outside tracked entities, globals, or list/scroll state. A later switch must repeat the prototype's A/B, test run, manual stale-frame check, and static review. `GPUI_VIEW_RETENTION=0` is the escape hatch while doing so.
- When comparing backends, the paced log-flood "parse MiB/s" depends on the CPU clock. Compare the unpaced or busy-neighbour figure instead.

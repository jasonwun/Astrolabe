# PROTOTYPE — GPUI Fast backend (#74)

Throwaway branch `prototype/gpui-fast`; never merged. The only code change is
`gpui-kit = { version = "=0.8.1", features = ["gpui-fast"] }` in the root
`Cargo.toml`, which resolves `gpui-fast 0.1.12` (released 2026-10-26).

## A/B (same machine, same session, release builds, 2026-10-26)

Baseline = `origin/main` 0ba80ff (gpui-pre 0.3.10); candidate = this branch.
Method: `docs/agents/spike-regression.md`. Loads 2.3–3.9 for every run.

| Spike | Metric | gpui-pre | gpui-fast |
|---|---|---|---|
| datatable (300 s, 157 cycles) | RESULT | PASS (0 gaps>100ms) | PASS (0 gaps>100ms) |
| | draw_avg / draw_worst | 11.49 / 30.96 ms | **8.01 / 22.80 ms** |
| | gap_worst / gaps>66ms | 85.7 ms / 663 | 85.0 ms / **262** |
| | rss start→end | 86→96 MB | 87→115 MB (same slope, +18 MB offset) |
| editor (×2) | set_value | 38.3 / 18.0 ms | 35.2 / 37.6 ms |
| | pagedown draw_avg | 6.73 / 5.75 ms | 5.91 / 5.68 ms |
| | edit_avg | 354 / 277 µs | 323 / 340 µs |
| | gaps>33ms (all bursts) | identical | identical |
| terminal suite | RESULT | PASS (0) | PASS (0) |
| | flood draw_avg / worst | 1.15 / 2.85 ms | 0.91 / 4.24 ms |
| | snapshot-copy | 166 µs | 147 µs |
| terminal log-flood (60 s, ×3) | RESULT | PASS 5/5 | PASS 5/5 |
| | parse throughput | 47.7 / 48.4 / 46.9 MiB/s | **31.1 / 30.7 / 30.3 MiB/s** |
| | snapshot under load avg | 139 / 134 µs | 176 / 190 µs |
| | process instructions / sys time | 198 G / 7.1 s | 116 G / 11.9 s |

Log-flood: the candidate retires 41% fewer instructions overall but the
feeder thread's `feed_output` (vte parse under the `Term` lock) is ~1.5×
slower, reproducible in either run order, with more kernel time — lock
contention or core placement, not investigated. Still 19× headroom over the
1.6 MiB/s feed.

## Tests

`cargo test --workspace` passes on gpui-fast (all `gpui_kit::test` UI tests).

## Manual check (kind-astrolabe-dev, debug build)

No stale frames: external `kubectl scale` 2→4 showed in Deployment details
without input (pods, replicas, ages ticking); a pod created by kubectl
appeared in the Pods list without input; Logs kept streaming; Exec echoed
typed input; namespace selector popover, inline Scale form (typing + Apply),
dock tab switching all behave as on gpui-pre. Terminal rows look tightly
packed — identical on gpui-pre, so not a backend effect.

## Static review of Retained Mode hazards

Render state outside entities would go stale. Ages read the wall clock but
both refresh paths go through the entity (`resource_list` timer →
`refresh` → `table.update`; `resource_detail` timer → `cx.notify()`).
`Rc<RefCell>` only appears in tests. Escape hatch: `GPUI_VIEW_RETENTION=0`.

## Supply side

- Same publisher as gpui-kit (longbridge); 12 releases in 20 days
  (v0.1.0 2026-10-06 → v0.1.12 2026-10-26); README: "experimental".
- GitHub issues and discussions are disabled on longbridge/gpui-fast.
- gpui-kit 0.8.1 requires `gpui-fast = "0.1.12"` (caret): only
  `Cargo.lock` holds the exact version; `cargo update` can float it.

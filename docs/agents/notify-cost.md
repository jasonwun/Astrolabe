# Notify-cost measurement

Session notifies as one entity: every batch its watch drain applies, from
any watch, wakes every observer (#106). This measures what that costs the
main thread under watch churn, so a fix (per-watch notification, say) is
chosen on numbers. #106's results comment holds the first run.

## Run it

```sh
dev/notify-cost.sh [out-dir]
```

It needs `kind-astrolabe-dev` reachable and a display: the app opens a
real window, and only the front tab draws, so leave the window visible
and do not use the machine meanwhile. The script:

1. builds `astrolabe` with `--release` and debug assertions, which keeps
   the debug-only `ASTROLABE_*` launch preselection, and the
   `astrolabe-kube` `churn` example;
2. creates `astrolabe-notify-<worktree>` and seeds `COUNT` (300) each of
   Secrets, Services, zero-replica Deployments and ConfigMaps
   (`churn ... seed`);
3. for each `PANELS` (`1 5 20`) x `RATES` (`0 10 100`): launches the app on
   that namespace with `ASTROLABE_OPEN` opening the ConfigMaps list, then
   the Secrets list in its place, then `PANELS - 1` detail tabs over the
   seeded Deployments, Secrets and Services. After `WARMUP` seconds (10)
   it runs `churn ... run RATE`, cycling 300 `churn-N` ConfigMaps
   through create, patch and delete, one watch event each;
4. prints one line per run, the mean of the perf reports logged over
   `RUN_SECONDS` (30) once churn has had 5 s to reach its rate, and
   finally deletes the namespace.

A panel is a dock tab: the one list plus each detail tab. Watches stay
resident once started, so the ConfigMaps watch runs while the list shows
Secrets: every ConfigMaps event re-projects a list it does not touch.

Run the pieces by hand when you need another mix:

```sh
kubectl --context kind-astrolabe-dev create namespace NAMESPACE
cargo run --release -p astrolabe-kube --example churn -- kind-astrolabe-dev NAMESPACE seed 300
cargo run --release -p astrolabe-kube --example churn -- kind-astrolabe-dev NAMESPACE run 100 30
NO_COLOR=1 RUST_LOG=warn,astrolabe::perf=trace ASTROLABE_CLUSTER=kind-astrolabe-dev \
    ASTROLABE_NAMESPACE=NAMESPACE ASTROLABE_OPEN=ConfigMap,Secret,Deployment/d-1 \
    CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true cargo run --release -p astrolabe
```

`ASTROLABE_OPEN` is a comma-separated list: `Kind` shows that built-in
kind's list, `Kind/name` opens that object's detail in the namespace.

## The report

`crates/app/src/perf.rs` is off unless its target is at `trace`
(`RUST_LOG=warn,astrolabe::perf=trace`); `astrolabe=debug` leaves it off.
Then it logs one `main-thread cost` line a second, each count since the
last line:

| Field | Meaning |
|---|---|
| `watch_events` | watch messages the session drains applied |
| `notifies` | whole-session `cx.notify()`s after a drained batch |
| `draws` | workspace renders, one per window frame drawn |
| `list_refresh`, `_ms` | `ResourceListPanel::refresh` calls, total time |
| `detail_resolve`, `_ms` | `detail::resolve` calls in detail renders, total time |
| `detail_render`, `_ms` | detail tab renders, resolve included |
| `lag_max_ms` | worst lateness of a 5 ms main-thread probe timer |
| `lags_over_16ms` | probe wakes later than a 60 Hz frame |

Main-thread ms/s is the `_ms` fields summed (render includes resolve, so
add `list_refresh_ms + detail_render_ms`); the probe's lag catches the
rest, such as layout and paint. Frame stalls are `lags_over_16ms`: the
probe measures how long the main thread was blocked, a proxy for frame
time rather than frame time itself.

Read the numbers with these in mind:

- Debug assertions are on in the measured build, so times may run a
  little high against a shipped release.
- The time per call falls as the rate rises (a list refresh took about
  6 ms idle, 1.2 ms at 100/s in #106's run): a busy main thread keeps the
  CPU clocked up and its caches warm. Compare `_ms` per second across
  runs at one rate, not per call across rates.
- An idle app also logs a few `lags_over_16ms`, since macOS coalesces an
  idle app's timers. Compare against the `RATES=0` row.

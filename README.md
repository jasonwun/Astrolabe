# Astrolabe

A desktop application built with [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui), the GPU-accelerated UI framework from Zed.

## Layout

- `crates/app` — `astrolabe` (binary): GPUI entry point; store and session entities live here
- `crates/kube` — `astrolabe-kube` (library): kube-rs communication layer; must not depend on gpui
- `crates/terminal` — `astrolabe-terminal` (library): `TerminalBackend` trait + alacritty implementation + GPUI view; alacritty types never leave the crate

## Requirements

- Rust (pinned, see `rust-toolchain.toml`)
- On Linux: gpui's system dependencies (see the gpui docs); on macOS, Xcode command line tools

## Development

```sh
cargo run
```

### Watch recovery logs

The app installs a `tracing` subscriber driven by `RUST_LOG` (default `warn`: watch failures surface, but there is no per-event flood). To follow Namespace watch failures, kube-rs backoff/relist transitions, and session health/snapshot changes during local development:

```sh
RUST_LOG=astrolabe=debug,astrolabe_kube=debug,kube_runtime=debug cargo run -p astrolabe
```

Only metadata is logged — cluster name, error summaries, object counts, health states — never kubeconfig contents, credentials, request headers, or full resource payloads.

## Dependency upgrades

Third-party versions are declared once in the root `[workspace.dependencies]` (exact `=` pins for `gpui-kit` / `alacritty_terminal` / `k8s-openapi`; `kube` follows the kube-rs survey's `"4.0"` + `ws`/`rustls-tls`), members only write `workspace = true`, and `Cargo.lock` is committed. Upgrades are explicit actions:

- one dependency per PR, in its own PR — no batch bumps
- the PR must attach regression numbers for the three spike benchmarks (DataTable watch, giant-YAML editor, terminal log-flood); the spikes live in `spikes/` and are run manually, not in CI (`docs/agents/spike-regression.md`)

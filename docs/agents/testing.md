# Testing & quality commands

## Unit / integration

```sh
cargo test --workspace        # hermetic default suite
```

## Cluster e2e

`crates/kube/tests/e2e_cluster.rs` is `#[ignore]`d by default; it needs a
reachable cluster from the ambient kubeconfig (dev loop: kind
`kind-astrolabe-dev`). CI runs it in the `e2e` job on ubuntu-latest —
GitHub's macOS runners have no Docker, so kind cannot run there.

```sh
cargo test -p astrolabe-kube --test e2e_cluster -- --ignored --test-threads=1
```

## Coverage

```sh
cargo llvm-cov --workspace --summary-only
```

## Mutation testing

`astrolabe-kube` keeps a zero-missed bar — after touching it, rerun and
kill or justify every missed mutant:

```sh
cargo mutants -p astrolabe-kube -j 4        # full run, ~90s
cargo mutants -p astrolabe-kube -f <file>   # scoped to one file
```

## Scripted apiserver tests (gpui + real I/O)

Tests that drive real network I/O (e.g. `Session::new` against a scripted
localhost apiserver) inside a `#[gpui_kit::test]` — reference implementation:
`session_watch_error_forces_complete_namespace_relist` in
`crates/app/tests/session.rs`. Two gotchas:

- Call `cx.executor().allow_parking()` first. Real I/O wakes the foreground
  executor from tokio worker threads; without it the test scheduler fails the
  test at teardown with "Your test is not deterministic".
- `shutdown(Shutdown::Both)` every connection the server abandons. Returning
  from a connection handler without shutdown leaves the socket open, and
  hyper's client pool hands the dead connection to the next request, which
  then hangs unread.

Origin: #28 session, both hit mid-implementation.

## Cargo.lock sticky-pin conflicts

When a pinned transitive dependency deadlocks resolution (e.g.
gpui-fast-macos pinning `cocoa =0.26.0` while the lockfile holds 0.26.1),
delete the stale `[[package]]` blocks from Cargo.lock and let cargo
re-resolve. Verify with `git diff Cargo.lock` that only the expected
packages moved.

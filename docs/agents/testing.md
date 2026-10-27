# Testing & quality commands

## Unit / integration

```sh
cargo test --workspace        # hermetic default suite
```

Run it last on the final commit, after review fixes; the PR's Evidence
cites that run: its output pasted verbatim, with any notes outside the
code block.

## Cluster e2e

`crates/kube/tests/e2e_cluster.rs` is `#[ignore]`d by default; it needs a
reachable cluster from the ambient kubeconfig (dev loop: kind
`kind-astrolabe-dev`). CI runs it in the `e2e` job on ubuntu-latest —
GitHub's macOS runners have no Docker, so kind cannot run there.

```sh
cargo test -p astrolabe-kube --test e2e_cluster -- --ignored --test-threads=1
```

CI also runs the `crd_` e2e tests on a v1.25 kind node
(`e2e-legacy-discovery`), the real-server check of discovery's per-group
fallback.

A kube-rs `watcher` emits `Event::Init` before its LIST, so a test waiting
for a LIST failure (e.g. a 403) skips `Init` until the `Err` arrives (#16).

### Manual Namespace recovery check (kind-astrolabe-dev)

The automated e2e cannot prove timely full-relist recovery versus silent
watch resumption; that needs a real GUI stop/start against the dev cluster.

1. Identify the kind node (the kind CLI has no `stop` subcommand — use
   Docker for the power cycle):

   ```sh
   kind get nodes --name astrolabe-dev    # prints astrolabe-dev-control-plane
   ```

2. Open Astrolabe on the `kind-astrolabe-dev` context, then stop and start
   the control plane:

   ```sh
   docker stop astrolabe-dev-control-plane
   docker start astrolabe-dev-control-plane
   ```

3. While the node is down, the cluster health indicator turns red
   ("Stale") and a banner shows "connection lost, data may be stale"; the
   existing Namespace rows stay visible.
4. Once the node is Ready again, the banner clears and the indicator
   returns to green ("Healthy") **without any additional Namespace
   create/update/delete event** — recovery must come from the watch relist
   alone, not from you touching resources.

Cleanup: close Astrolabe and leave the kind cluster running unless you
intend to tear it down.

## Coverage

```sh
cargo llvm-cov --workspace --summary-only
```

To compare against a baseline (e.g. a refactor that must not regress),
build the baseline in a throwaway worktree, not via `git stash` (the stash
stack is shared across worktrees), and filter the summary rows by file
basename:

```sh
git worktree add ../cov-base origin/main
(cd ../cov-base && cargo llvm-cov --workspace --summary-only) | grep -E 'session\.rs|store\.rs'
cargo llvm-cov --workspace --summary-only | grep -E 'session\.rs|store\.rs'
git worktree remove ../cov-base
```

## Mutation testing

`astrolabe-kube` keeps a zero-missed bar — after touching it, rerun and
kill or justify every missed mutant:

```sh
dev/mutants.sh -p astrolabe-kube -j 4                       # full run, ~90s
dev/mutants.sh -p astrolabe-kube -f crates/kube/src/resource.rs -j 4
```

Use repository-relative paths with `-f`. `Found 0 mutants to test` means the
filter missed its target, not that the bar passed; the wrapper turns that
result into a failure.

A `TIMEOUT` fails the run too (cargo-mutants 27.1 exits `Timeout` ahead of
`FoundProblems`): a mutant that silences a callback leaves an unbounded
`recv().await` hanging. Bound every wait on a channel or stream in kube
tests with `tokio::time::timeout`.

CI's `mutants` job enforces the bar on every PR, scoped to the PR's diff.

## Scripted apiserver tests (gpui + real I/O)

Tests that drive real network I/O (e.g. `Session::new` against a scripted
localhost apiserver) inside a `#[gpui_kit::test]` — reference implementation:
`session_watch_error_forces_complete_namespace_relist` in
`crates/app/tests/session.rs`. Three gotchas:

- Call `cx.executor().allow_parking()` first. Real I/O wakes the foreground
  executor from tokio worker threads; without it the test scheduler fails the
  test at teardown with "Your test is not deterministic".
- `shutdown(Shutdown::Both)` every connection the server abandons. Returning
  from a connection handler without shutdown leaves the socket open, and
  hyper's client pool hands the dead connection to the next request, which
  then hangs unread.
- Route requests by path and answer every request `Session::new` makes on
  connect (namespace list/watch, CRD list/watch, `/api` + `/apis`
  discovery). A server that treats every non-watch request as one resource's
  LIST miscounts its queries and serves the wrong body once the session adds
  a connect-time request; the test then times out far from the cause.

Origin: #28 session (first two), #16 session (third).

## Merge gate

`main` is gated by the repository ruleset "main" alone (no classic branch
protection). Its required checks are the `ci.yml` job names `lint`,
`build-test`, `e2e`, `e2e-legacy-discovery`, `mutants`; squash is the only
merge method. Renaming or adding a CI job means updating the ruleset in the
same PR, or the gate waits on a check that never reports:

```sh
gh api repos/jasonwun/Astrolabe/rulesets/25450659 \
  --jq '.rules[] | select(.type=="required_status_checks").parameters.required_status_checks[].context'
```

Origin: #42 session (a ruleset with an emptied required list sat beside
stale classic protection unnoticed).

## Cargo.lock sticky-pin conflicts

When a pinned transitive dependency deadlocks resolution (e.g.
gpui-fast-macos pinning `cocoa =0.26.0` while the lockfile holds 0.26.1),
delete the stale `[[package]]` blocks from Cargo.lock and let cargo
re-resolve. Verify with `git diff Cargo.lock` that only the expected
packages moved.

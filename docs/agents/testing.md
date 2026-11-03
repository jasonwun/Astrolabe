# Testing & quality commands

## Unit / integration

```sh
cargo test --workspace        # hermetic default suite
```

Run it last on the final commit, after review fixes, through the wrapper,
naming the test binaries your change affects: `<name>` for
`tests/<name>.rs`, or the crate name for its unit tests (`astrolabe_kube`
or `astrolabe-kube`; `astrolabe` prints both its lib and main unit tests):

```sh
dev/test-evidence.sh watched session
```

It runs the suite once with `--no-fail-fast`, writes the full log to a
temp file (path on stderr), and exits with cargo's status. Its stdout is
the PR's Evidence block, pasted as is:

- each named binary's `Running` and `test result` lines;
- each failing test's name and captured output, panic message included;
- each binary that crashed (abort, signal) before its `test result` line;
- one tally line: test binaries and doc-test runs, and passed / failed /
  ignored summed over every `test result` line.

Notes go outside the code block. Do not paste the full log. A named binary
missing from the log fails the script; check the spelling.

## Lint job, before each push

Before pushing, run every step of `.github/workflows/ci.yml`'s `lint`
job locally, not only fmt and clippy. That means `dev/comment-width.sh`,
`dev/registration-path.sh`, `dev/log-capture.sh`, `dev/layer-check.sh`
and the gpui tree check. Run them again after review fixes: a reworded
doc comment can push a line past 80 columns.

Origin: #149's review fix rewrapped `client_for_context`'s `# Errors`
to an 82-column line. Only fmt and clippy ran locally, so CI's
`comment-width.sh` caught it and cost a CI round (#149 retro).

## Move-only refactors

```sh
dev/move-check.sh             # lines the diff does not just move
```

Run it before review on a ticket that only moves code; the residue
should be `mod`/`use` lines, new modules' `//!` docs and rustfmt
re-wraps. What else counts is in `CODING_STANDARDS.md`.

A test left in the lower crate that uses a moved item keeps its
assertions: the lower crate takes the upper one as a dev-dependency, and
the test changes only its `use` path. Integration tests (`tests/*.rs`)
link the same library the upper crate builds on, so the types match; the
library itself still does not depend on the upper crate. Do not rewrite
the assertions to avoid the import, and do not move a cluster e2e into
`astrolabe-model`: `dev/layer-check.sh` forbids `async fn` and
`astrolabe_kube::Cluster` in its tests.

Origin: #154 moved the Catalog to `astrolabe-model`. The first version
rewrote `crd_kind_is_discovered_watched_and_deleted`'s Catalog
assertions as `ServedResources::resolve` checks, one of which only read
back the test's own manifest. Both reviewers flagged it; the Spec
reviewer's fix (an e2e in the model crate) would have failed the layer
check (#154 retro).

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

A poll that waits for the cluster to converge (a CRD becoming served, a
rollout finishing) treats an error from the polled call as "not yet":
keep the last error, sleep, poll again, and name that error in the
timeout's panic. Only the timeout fails the test. While the apiserver is
still registering what the test just applied, a call that succeeds a
moment later can fail outright.

Origin: `crd_kind_is_discovered_watched_and_deleted` unwrapped
`discover()` inside its 30 s poll. On `e2e-legacy-discovery` the new
group was already listed under `/apis` while its group version still
answered 404, so per-group discovery failed and the unwrap panicked on a
docs-only PR (#186, fixed in #187).

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

### Notify-cost measurement (kind-astrolabe-dev)

`dev/notify-cost.sh` measures what Session's whole-entity notify costs the
main thread under watch churn, for a given number of open panels (#106).
Setup, columns and how to read them: `docs/agents/notify-cost.md`.

## Coverage

```sh
cargo llvm-cov --workspace --summary-only
```

To compare against a baseline (e.g. a refactor that must not regress),
build the baseline in a throwaway worktree, not via `git stash` (the stash
stack is shared across worktrees), and filter the summary rows by path
from the line start (a bare basename also matches cargo's `Running
tests/crd_session.rs` lines). Name the worktree after the issue: other
worktrees may run the same recipe at once.

```sh
git worktree add ../cov-base-<issue> origin/main
(cd ../cov-base-<issue> && cargo llvm-cov --workspace --summary-only) | grep -E '^crates/app/src/(session|detail)\.rs '
cargo llvm-cov --workspace --summary-only | grep -E '^crates/app/src/(session|detail)\.rs '
git worktree remove ../cov-base-<issue>
```

A bar over several files (e.g. "the merged module vs `session.rs` +
`detail.rs` on `main`") compares summed counts: add the files' regions and
missed regions, then take the percentage.

## Mutation testing

`astrolabe-kube` and `astrolabe-model` keep a zero-missed bar — after
touching either, rerun it and kill or justify every missed mutant:

```sh
dev/mutants.sh -p astrolabe-kube -j 4                       # full run, ~90s
dev/mutants.sh -p astrolabe-kube -f crates/kube/src/resource.rs -j 4
dev/mutants.sh -p astrolabe-model -j 4
```

Use repository-relative paths with `-f`. `Found 0 mutants to test` means the
filter missed its target, not that the bar passed; the wrapper turns that
result into a failure.

A `TIMEOUT` fails the run too (cargo-mutants 27.1 exits `Timeout` ahead of
`FoundProblems`): a mutant that silences a callback leaves an unbounded
`recv().await` hanging. Bound every wait on a channel or stream in kube
tests with `tokio::time::timeout`. A loop that runs until nothing changes
hangs the same way under a mutant: cap its passes (`column_widths` runs
at most one pass per column, plus one).

A `TIMEOUT` can also come from load alone: cargo-mutants' automatic test
timeout (about 20s for the model crate) is too short while `-j 3` builds
run alongside. Retest only the missed and timed-out mutants with a longer
timeout before chasing a hang:

```sh
dev/mutants.sh -p astrolabe-model --iterate --timeout 180 -j 3
```

Before each push, review fixes included, run CI's own command; scoped
to the diff, it takes about a minute:

```sh
git diff origin/main...HEAD > "$SCRATCH/pr.diff"
cargo mutants -p astrolabe-kube -p astrolabe-model --in-diff "$SCRATCH/pr.diff" -j 2
```

Origin: #156 ran an unscoped `dev/mutants.sh` after its review fixes,
which also mutated the app crate. It ran past ten minutes, so the PR was
pushed without its result. CI then missed `delete !` in a pruning line
the fixes had added (#156 retro).

CI's `mutants` job enforces the bar on every PR, scoped to the PR's diff,
and fails on every missed mutant there: a justification in the PR does not
pass it. When no input can tell a mutant from the original (an equivalent
mutant), change the code so the mutated branch no longer exists: drop a
`< 0` branch that `max(0)` already covers, or a match arm or branch that
prints what another one does.

Origin: #153 moved the projections into `astrolabe-model`. A full run
reported TIMEOUTs that `--timeout 180` cleared, two real hangs in
`column_widths`, and equivalent mutants in `format_age`,
`format_duration`, `condition_priority` and `quantity_value` (#153 retro).

CI leaves the app crate out, so its `mutants` check passes on any
app-only PR. After changing app logic (state, selection, branching; not
layout), run the changed files and kill or justify, in the PR, every
missed mutant:

```sh
dev/mutants.sh -p astrolabe -f crates/app/src/workspace.rs -j 4   # ~4 min
```

When the change touches a few lines of a large file, scope the run to the
diff instead; `-f` mutates the whole file:

```sh
git diff origin/main -- crates/app > app.diff
dev/mutants.sh -p astrolabe --in-diff app.diff -j 4
```

Origin: #152 changed `ClusterIo::operate` in `cluster_io.rs`. `-f` found
107 mutants at about two minutes each, so the run was stopped; the diff
held 4 (#152 retro).

Paths a scripted test cannot reach stay missed: kubeconfig discovery
(`discover_contexts`) and styling such as the selected row's colours.

## Watch tests on a mock client (paused clock)

Relist and the 403 backoff are tested in the cluster layer, through
`Cluster::watch_*` over a mock `kube::Client` — reference implementation:
`crates/kube/tests/support/mod.rs` and `crates/kube/tests/watch.rs`. No TCP server and no GPUI. Three gotchas:

- Use `#[tokio::test(start_paused = true)]` and build the handle with
  `Cluster::from_client` inside the test: it runs the watch on the caller's
  runtime, so the paused clock skips the 1 s and 30 s retries. A handle from
  `Cluster::connect` runs on the cluster runtime, whose clock is real.
- Answer the last scripted watch with a request that never completes
  (`future::pending()`). An empty `200` ends the watch at once and the
  watcher re-watches in a busy loop; awaiting a message that never comes,
  with no timer pending, hangs the test instead of failing it.
- Capture logs with a process-wide subscriber, in a binary whose only test
  reaches the asserted callsites (`crates/kube/tests/watch_relist.rs`). A
  thread-scoped one (`set_default`) races other tests' threads over
  tracing's per-callsite cache and loses events.

Origin: #156 (replaced the scripted TCP apiserver test from #28/#16).

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

The ruleset also requires the branch to be up to date with `main`, so a
PR behind `main` is refused at merge time even with every check green.
Update the branch and queue the merge in one go; auto-merge is enabled and
merges once the rerun checks pass:

```sh
gh pr update-branch <n>
gh pr merge <n> --squash --auto
```

Origin: #118 session (merge refused as behind `main`; a second CI round was
watched by hand).

## Cargo.lock sticky-pin conflicts

When a pinned transitive dependency deadlocks resolution (e.g.
gpui-fast-macos pinning `cocoa =0.26.0` while the lockfile holds 0.26.1),
delete the stale `[[package]]` blocks from Cargo.lock and let cargo
re-resolve. Verify with `git diff Cargo.lock` that only the expected
packages moved.

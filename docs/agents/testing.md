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

Before pushing, run `dev/pre-push.sh`: every step of
`.github/workflows/ci.yml`'s `lint` job, not only fmt and clippy, then
the diff-scoped mutants. Run it again after review fixes: a reworded doc
comment can push a line past 80 columns.

Origin: #149's review fix rewrapped `client_for_context`'s `# Errors`
to an 82-column line. Only fmt and clippy ran locally, so CI's
`comment-width.sh` caught it and cost a CI round (#149 retro).

#344 ran fmt, clippy and the tests, then pushed: `dev/layer-check.sh`
failed `lint` on `ClusterId::named` in `launch.rs`'s `#[cfg(test)]` module,
because it forbids that call anywhere under `crates/app/src`. A test that
names a Cluster from a string goes in `crates/app/tests/` (#344 retro).

`dev/pre-push.sh`'s mutants step reads the committed diff: an edit left
uncommitted fails it with `Diff content doesn't match source file`.
Commit the fix, then re-run. A hand-written `PartialEq`/`PartialOrd` for a
type used as a `BTreeMap` probe (#244's `dyn KeyView`) is never called by
`BTreeMap::get`, which uses only `Ord::cmp`: its mutants go MISSED. Write
`eq` and `partial_cmp` through `cmp`, and cover them with a unit test next
to the type. A test that binds `a`, `b`, `x`, `y` trips clippy's
`many_single_char_names`: name the bindings (#244 retro).

Code moved from the app crate into `astrolabe-model` or `astrolabe-kube`
enters the mutants scope, tests and all: #302 moved `OpenRowError` and its
hand-written `Error::source`, and `dev/pre-push.sh` reported `source -> None`
MISSED, because the app's window tests had never asserted it. Test each moved
trait impl in the model's own tests, before the first push (#302 retro).

Run the lint job's steps and `dev/test-evidence.sh` on the rebased tree,
right before the push that opens the PR and again after every later
rebase, and fetch `origin/main` first. CI lints the merge with main:
#258's tests called `watch_builtin(kind, "default")` after another PR
made its namespace an `Option`, and `lint` failed on a base the local
run had never seen. The PR's tally line is pasted from the last run:
#258 merged saying 552 passed after a rebase made it 555 (#234 retro).

Chain the fix, the re-run and the push with `&&`, never `;`. #243's
`sed -i` (BSD `sed` on macOS needs `sed -i ''`) failed on a line that
`comment-width.sh` had flagged, and the `;` let the commit and push
carry on with the 83-column line. Use Edit for a one-line change, then
re-run the failing script before committing (#243 retro).

A `cd crates/kube && python3 ...` carries to the next Bash call: the one
after ran in `crates/kube`, and its repo-relative paths were wrong. Use
absolute paths or a subshell `(cd dir && ...)` (#278 retro).

A new rule in `dev/layer-check.sh` gets a probe file for every way to
write the path, not only the full path: a one-line `use a::{b::X}`, the
`astrolabe_kube::kube::` route and a long form such as `kube::api::Api`.
Put each probe in `crates/model/src` and `crates/app/src`, run the script,
delete the probe, and list the runs in the PR's Evidence.

Origin: #224's first draft caught the full paths but let
`use kube::{config::Kubeconfig}` through; the Spec reviewer found it and
the rules were tightened before merge (#224 retro).

CI's `mutants` job takes about 15 minutes on a diff of 57 mutants, where
`dev/pre-push.sh` took 4: a PR on a new model module waits that long for
auto-merge after the other four jobs pass in 3 (#299).

A CI failure in a test the diff cannot reach: rerun the failed job once,
and when it passes file a `needs-triage` issue with both run links instead
of merging silently. #225's `build-test` failed twice on
`dropping_a_port_forward_stops_its_listener_and_connections` before it
passed (#226).

A CI job that fails before any test runs (#343's `e2e`: `kind-action`
printed `sha256sum: ... no properly formatted checksum lines found` while
installing kind) is the same case: rerun with `gh run rerun <id> --failed`,
and file the issue with both run links (#354) (#343 retro). #353 hit it
twice in a row (`e2e`, then `e2e-legacy-discovery`, as `kind-linux-amd64:
FAILED`): the failed job's log has the line, and one more `--failed`
rerun each passed. Comment the run links on #354 instead of a new issue
(#341 retro). #344 hit it again on
`e2e-legacy-discovery`, twice in a row on two pushes: one `gh run rerun
<id> --failed` each passed, and the three failed runs are on #354 (#344
retro). #354 fixed it: both e2e jobs install a
pinned kind through `.github/actions/setup-kind` with a bounded retry, so a
recurrence is a bug in that action. Bump its `KIND_VERSION` and
`KIND_SHA256` together (#354).

A CI-only PR (#354) has no local test: the dev machine is macOS without
Docker, so the new action's happy path is proven only by the PR's own `e2e`
jobs (a PR runs its own workflow and local actions), and a transient
failure cannot be forced there. Check the pieces that run anywhere (a wrong
sha256 rejected, the pinned sha256 against the release's `.sha256sum`),
and say in the PR that the retry loop itself is unexercised (#354 retro).
When scripting a doc edit, assert the anchor string is unique
(`s.count(old) == 1`): an unanchored `replace(..., 1)` put #354's note in
the wrong paragraph until the diff showed it (#354 retro).

`astrolabe-model`'s tests are linted too (`--all-targets`, pedantic): a
test helper that always returns `Ok(..)` trips `unnecessary_wraps`, and
an unbackticked type name in a doc comment trips `doc_markdown`. Return
the bare value and wrap at the call sites (#164). `assert!(x.is_empty())`
trips `assert_is_empty`: write `assert_eq!(x, [])`, or
`assert_eq!(x, Vec::<T>::new())` where the element type is not inferred;
for `!x.is_empty()` assert on `x.first()` (#245). A test helper that
picks one `Command` variant with `_ => None` hides the next variant a
ticket adds: list the other variants so the compiler asks for them
(#165; #245's `stopped()` helper broke the rule again and neither
review caught it). Adding variants to `Command` can leave its
`#[expect(clippy::large_enum_variant)]` unfulfilled, which `-D warnings`
rejects: delete the `expect` when clippy says so (#166). Code moved into
`astrolabe-model` meets the same pedantic lints: a `pub fn` returning
`Result` needs a `# Errors` section, and `|w| w.method()` becomes
`Type::method` (#167).

## Move-only refactors

```sh
dev/move-check.sh             # lines the diff does not just move
```

Run it before review on a ticket that only moves code; the residue
should be `mod`/`use` lines, new modules' `//!` docs and rustfmt
re-wraps. What else counts is in `CODING_STANDARDS.md`.

Run `git add -N .` first: the script diffs the working tree, and a new
file it cannot see leaves every moved line "removed nowhere". Before the
move, compare what the moved code imports with the target crate's
dependencies and `dev/layer-check.sh`'s allowlist: `Watched`'s
`tracing::info!` needed a new dependency and allowlist entry, which then
shows in the residue. A visibility change the move forces (a private
method the old crate called) is invisible to the script; name it, and its
real caller, in the PR body.

Origin: #163 moved Session's value types. The first `move-check` run
listed the whole new module as removed lines because the file was
untracked, and the implementer first gave a test as the reason
`Watched::mark_stale` went `pub`, where production code calls it; both
reviewers corrected it (#163 retro).

A test left in the lower crate that uses a moved item keeps its
assertions: the lower crate takes the upper one as a dev-dependency, and
the test changes only its `use` path. Integration tests (`tests/*.rs`)
link the same library the upper crate builds on, so the types match; the
library itself still does not depend on the upper crate. Do not rewrite
the assertions to avoid the import, and do not move a cluster e2e into
`astrolabe-model`: `dev/layer-check.sh` forbids `async fn`,
`astrolabe_kube::Cluster` and the kube-rs I/O entry points in its tests.

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

A test that waits for a listener to close polls `TcpListener::bind` on its
port, not `connect`. Dropping a handle aborts its task asynchronously, so
the listener can outlive the drop signal; a `connect` probe cannot tell a
closed listener from a leaked one, because connects queue in the backlog
until it fills and then fail too. Prove the test still fails by leaking
the listener (`ManuallyDrop`) and reading the timeout panic.

Origin: `dropping_a_port_forward_stops_its_listener_and_connections`
failed twice on #225's CI by connecting right after the drop. The issue's
brief said to poll `connect`; with a leaked listener that version passed
after about 1.6 s, so #230 polls `bind` (#226 retro).

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
(cd ../cov-base-<issue> && cargo llvm-cov --workspace --summary-only) | grep -E '^crates/app/src/session\.rs '
cargo llvm-cov --workspace --summary-only | grep -E '^crates/app/src/session\.rs '
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
tests with `tokio::time::timeout`. A test that awaits on a plain executor
(`futures::executor::block_on`), to show the work runs on the cluster
runtime, has no tokio timer: run the `block_on` on a `std::thread` and
take its answer with `std::sync::mpsc`'s `recv_timeout` (#159 retro). A
loop that runs until nothing changes
hangs the same way under a mutant: cap its passes (`column_widths` runs
at most one pass per column, plus one).

A `TIMEOUT` can also come from load alone: cargo-mutants' automatic test
timeout (about 20s for the model crate) is too short while `-j 3` builds
run alongside. Retest only the missed and timed-out mutants with a longer
timeout before chasing a hang:

```sh
dev/mutants.sh -p astrolabe-model --iterate --timeout 180 -j 3
```

Before each push, review fixes included, after rebasing onto `origin/main`,
run:

```sh
dev/pre-push.sh
```

It runs the steps of CI's `lint` job and CI's own `mutants` command (the
diff against the base, `-j 2`, about a minute), and fails like CI does.
Keep it in step with those two jobs in `.github/workflows/ci.yml`.

Origin: #156 ran an unscoped `dev/mutants.sh` after its review fixes,
which also mutated the app crate. It ran past ten minutes, so the PR was
pushed without its result. CI then missed `delete !` in a pruning line
the fixes had added (#156 retro).

#249 pushed without that run. Its `SessionState::served_resources`, a
pass-through of `.cloned().unwrap_or_default()` that every Session test
reaches only through the scripted cluster, survived `replace ... with
Default::default()`. A new accessor in `astrolabe-model` needs a test that
reads it back from a state that has the value and one that has not (#249
retro).

On 2026-11-04 seven CI runs failed, and `mutants` failed in every one. In
the three of four runs whose logs were read (`gh run view <id> --log-failed`)
it reported a real missed mutant, not a flake; the fourth was a test that
did not compile. The diff-scoped run below finds those mutants locally,
and this section already told each PR to run it (the Origins above). #242
added `custom_watch` and `custom_object_watch`, covered only by an app test
that CI's scope leaves out, and was pushed without the run: it found two
missed mutants afterwards. A prose rule had not closed the gap, so
`dev/pre-push.sh` runs it (#242 retro).

A `tracing` line guarded by a condition (`if old != new { info!(..) }`)
is a mutant `astrolabe-model`'s tests cannot see unless one counts log
events: the crate has no `tracing-subscriber` dev-dependency and
`dev/layer-check.sh` limits its dependencies. Implement `tracing::Subscriber`
in the test with an `AtomicUsize` and run the call under
`tracing::subscriber::with_default`; assert zero events when the
condition is false and one when it is true (`events_logged_by` in
`crates/model/tests/watched_logging.rs`). Put that test in a binary of its
own: `with_default` is thread-scoped, so a sibling reaching the same callsite
races it ("Tests that assert log lines" in `CODING_STANDARDS.md`).

Origin: this section sent #232's test to `watched.rs`, next to tests that log
the same line. It failed about 1 run in 6 under parallel tests until #285
moved it out. The proof was the binary run directly 1000 times with 0
failures, as the brief asked, not a handful of `cargo test` reruns (#285
retro).

Origin: #232 pushed without the command above. CI's `mutants` job missed
`!=` to `==` in `Watched::set_status`'s log guard, which only the app's
`LogCapture` test saw, and the PR needed a second CI round (#232 retro).

CI's `mutants` job enforces the bar on every PR, scoped to the PR's diff,
and fails on every missed mutant there: a justification in the PR does not
pass it. When no input can tell a mutant from the original (an equivalent
mutant), change the code so the mutated branch no longer exists: drop a
`< 0` branch that `max(0)` already covers, or a match arm or branch that
prints what another one does.

A `SessionState` query that only forwards to a free function (`custom_kind`
over the Catalog) is a mutant of its own: `-> None` survived until a test
called the method, not the free function (#165).

A `Display` impl on a model or kube error is a mutant too (`fmt` ->
`Ok(())`). The test that kills it asserts `to_string()` in the same crate:
the app test that shows the notice does not count, because CI scopes the
run to `astrolabe-kube` and `astrolabe-model`. #216 pushed without the
diff-scoped run and CI's `mutants` job failed on `ReferenceError`'s
`Display`, costing a CI round and a rebase (#216 retro).

For the diff-scoped run, write `pr.diff` under the session's scratchpad
directory: `$TMPDIR` is empty on this machine, so `> $TMPDIR/pr.diff`
failed and `--in-diff` reported "Failed to open diff file" (#165).

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

Paths a scripted test cannot reach stay missed: Cluster listing
(`list_clusters`) and styling such as the selected row's colours.

### Assertions that something is dropped

A test that asserts an answer is dropped, or a note is absent, also shows
the same answer appearing on the path that keeps it; without that control
it passes whether or not the guard works. Kill the guard's mutant with the
test, not only the full suite.

A counter that supersedes a pending timer (`generation += 1`) needs a test
where two timers from successive bumps exist and the older one must fire
nothing: assert the generations strictly increase, and that the old one's
`debounce_elapsed` is empty after the newer bump. A test that only checks
the old timer is dropped passes under `+=` -> `-=` and `+=` -> `*=` (#277:
both went MISSED until it asserted that).

Origin: #161's stale-Precheck test passed under the `+=` → `*=` mutant of
`session_version`: the scripted report noted a field the buffer did not
hold, so no note ever appeared. Typing `spec.bogus` and asserting the note
while the first Cluster is still selected made it fail (#161 retro).

## Watch tests on a mock client (paused clock)

A test of connect retries built on a connector that answers at once is
order-dependent: the poller that `Connection::attempt` spawns may finish an
attempt before a watch asks, so the watch starts a new one and the attempt
count shifts by one (#277's first drafts failed on it). To show that two
watches share one attempt, give the connector a delay on the paused clock
(`slow_flaky` in `crates/kube/src/cluster.rs`) so both wait on the same
in-flight attempt, and count attempts after both answer.

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

A test that a watch stops asserts the apiserver side, not the reader's next
message. kube-rs sends `Event::Init` before it issues the LIST, so a watch
task not yet polled when its `Cluster` drops can queue one `Init` before it
is cancelled: asserting the next message is `None` passed alone and failed
about one run in two (#274's `dropping_the_cluster_stops_its_watches`). Read
the stream to its end under a timeout, and count the dropped requests with
`Apiserver::closed_requests()`. Run a new test of this kind 15 times
(`for i in $(seq 15); do cargo test ...; done`) before the push: a one-off
pass hid the race.

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

A kube-layer test of a value read from the kubeconfig goes through
`Cluster::connect`, not only the helper that parses it: #236's Spec
reviewer found that the helper's tests left the handle's wiring unproven.
Put the extra context in `tests/client.rs`'s existing `KUBECONFIG` test,
since tests in that binary share the environment (#236 retro).

A `match` arm over a third-party error that a test cannot build has no
test, and its mutant goes MISSED. #235's `kube::Error::HyperError(_) =>
Unreachable` was one: `hyper::Error` has no public constructor. Drop the
arm and let the type fall to `Other`, and say so in the doc comment.
To build a transport failure in a kube-layer test, give `Client::new` a
`tower::service_fn` that returns `Err(std::io::Error::from(
ErrorKind::ConnectionRefused))`: kube-client turns it into
`kube::Error::Service`, which is what a real connect failure is
(#235 retro).

A `Client::new(service, ns)` mock skips every layer `ClientBuilder` adds:
kube-client's retry (429/503/504, 15 attempts by default) and its auth
layer. A kube test over such a mock shows what the code does with the
first answer, never how long a real node takes to give it. To test a retry
bound, wrap the mock in the layer itself (`client.rs`'s `retry_tests`, with
`#[tokio::test(start_paused = true)]`), and see the real node for the
duration (#279 retro; `docs/adr/0003-bounded-client-retry.md`).

`grep -rn pattern crates --include=*.rs` fails in zsh with `no matches
found`: quote the glob (`--include='*.rs'`) (#275 retro).

`astrolabe-kube`'s `test-support` cargo feature gates test-only API such as
`ApplyReport::failed`. A dependent enables it under its own
`[dev-dependencies]` only (resolver 3 keeps it out of a plain build), as
`astrolabe` does (#278).

A ticket's "reproduce first" step starts at the field that decides the
state, not at the app. #307 assumed a crash-looping Pod has an empty
`Containers.running`; `running_containers` reads the Pod spec, never its
status, so a real Pod never has it empty. The window test written for the
ticket could not show the reason, which is how it came out. Read the
projection that builds the list, and say in the ticket what drives it,
before writing a rule or an AC that depends on the list being empty
(#307 retro; the status-driven version is #313).

A state that a late answer can write needs a test for the answer arriving
after the state was cleared. #308 split the Apply and Precheck notices and
cleared the Precheck's when an Apply starts; the review found that a manual
Precheck already in flight would write its failure after that. A `Ticket`
checked only the Cluster and the buffer. When a rule says "X voids Y",
write the `begin_x`, `finish_y` order as a test, not only the clear
(#308 retro).

A projection that starts reading `status` breaks every fixture that built
its Pod with the spec alone: #313's change made `running` follow
`containerStatuses`, and four view fixtures (`workload_views.rs`,
`dock_tabs.rs`) and three model tests went red because their Pods had no
status. Run the affected test binaries right after the projection change
and give each fixture the statuses its test relies on, rather than
loosening the rule (#313 retro).

A review fix that rewords text a screenshot shows leaves the PR's
Evidence stale. #313's Spec review changed Forward's empty reason after
the kind captures; the PR said so, but the `crashy` Forward capture still
showed the old text. Redo the capture for any view whose text the review
fix touched (#313 retro).

When a rule moves into the model layer, the window test that checked it
usually stays as a wiring check, and finds the element by its id too.
#304's `workload_views` asserted `Stream failed: stream reset` by label
alone, so nothing in the app tests found `logs-status` or `exec-status`;
the id was covered only by the model's `Notice` equality, which cannot show
it renders. Keep the text assert, say in a comment that the text is the
model's, and add `try_find("<id>")` (#304 retro).
#347 moved eight more texts to `Notice` and added the model-side tests
first; the window tests still found `list-notice`, `detail-notice`,
`delete-cascade`, `picker-empty` and `nav-crd-denied` by label alone until
the Standards review pointed at this rule. Grep `crates/app/tests` for each
moved text's label and add `try_find("<id>")` beside it before the first
push (#347 retro).

A new `assert!(!x.is_empty())` fails `dev/pre-push.sh`'s clippy step
(`assert_is_empty`); write `assert_ne!(x, "")` (#347 retro).

Two more clippy lints that `dev/pre-push.sh` catches: building a string
with `text.push_str(&format!(..))` or `text += &format!(..)` trips
`format_push_string`, so write `let _ = write!(text, ..)` with `use
std::fmt::Write as _` inside the function. An emptiness assert on a
`Vec` is `assert_eq!(v, Vec::<String>::new())`, since `assert_ne!(x, "")`
is for strings (#345 retro).

A PR opened while main keeps merging goes `BEHIND` with no checks reported
(`gh pr checks` says "no checks reported", `mergeStateStatus` `BEHIND`) and
auto-merge never starts. Rebase onto `origin/main`, re-run
`dev/pre-push.sh` and `dev/test-evidence.sh` on the rebased tree, then
`git push --force-with-lease`: CI started and the PR merged within about
ten minutes (#345 retro).

Changing a workspace `version` makes `cargo ... --locked` fail with "cannot
update the lock file" until `Cargo.lock` follows: run `cargo update -p
<crate> --offline` once and commit the one-line lock change (#371 retro).

Code that runs a subprocess or reads the process environment (#379's login
shell capture) needs its seams before the first `dev/pre-push.sh`: the
program and the timeout as parameters, `$SHELL`'s value as an argument,
and a fake shell script in the temp dir for the success, fish, csh, hang and
failure paths. The first push had 14 MISSED mutants on `login_shell`, the
`fish`/`csh` match arms and empty-value guards. A lib that skips the work in
tests needs an opt-out the test run sets for itself, here
`ASTROLABE_NO_LOGIN_SHELL_ENV` in `.cargo/config.toml`: a `cargo test` with
piped stdout is not a TTY and would run the developer's shell. A
`#[cfg(not(unix))]` twin of a function shows up as its own MISSED mutants on
macOS: put the `cfg` on the call instead (#379 retro).

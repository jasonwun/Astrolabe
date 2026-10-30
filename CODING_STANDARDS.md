# Coding Standards

Read during review. Rules here are judgement calls; mechanical rules belong
in CI (`.github/workflows/ci.yml`) instead. Each rule cites the incident
that motivated it — challenge the rule, not the citation.

## Golden fixtures that claim real server responses

A golden test fixture presented as a real apiserver (or other external
service) response is captured from that server, and the fixture comment
records the server version and capture method (e.g. `kubectl -v=8` output).
A fixture written from assumption is labeled synthetic, so future readers
know the shape was never verified.

Origin: two #10/#15 goldens (409 messages always carrying a `using <g/v>`
clause; strict-decoding 422s under SSA) were plausible fabrications that the
kind e2e later disproved (#21 retro).

## Third-party API capabilities in design resolutions

A resolution that names a specific third-party API, type, or feature is
verified against the pinned version's source or docs before being written
down, and carries a "verified against <crate> <version>" note. Anything
unverified is phrased as an open question, not a mechanism. An upstream
with no release to pin (Freelens `main`, kubectl `master`) is cited by
commit (`Freelens 088d085`).

Origin: #10 named saphyr's `MarkedYamlOwned` (records no collection spans
in 0.0.6) and assumed a gpui-component gutter API (absent in 0.8.0); both
surfaced only mid-implementation, forcing deviations from the resolution
(#21 retro).

## Recovery claims in findings and design resolutions

A review finding or recommendation that invokes a third-party recovery
mechanism is verified against the pinned version's source or docs, as a
design resolution would be (the "verified against <crate> <version>"
note is required of resolutions, not findings). The claim names the
consumer-visible event that demonstrates recovery completed, because
reconnect, silent resume, and complete relist are distinct outcomes and
are not interchangeable: a silent resume may emit no signal at all, so
"the stream reconnected" is not evidence the consumer recovered. In the
Namespace watch incident, the valid recovery signal was kube-rs's
`InitDone` after a full relist; a silent watch resumption emits
nothing, leaving the UI stale until a later event.

Origin: the #26 review treated kube-rs stream recovery as equivalent to
UI recovery; kind verification showed a silent resumption need not emit
`InitDone` (#26/#27 retro).

## Spec mandates and resource scope in review findings

Before reporting a structure as Speculative Generality, the reviewer
checks whether the spec or design resolution explicitly mandates it: a
required abstraction is not speculative, however general its shape
looks. Before reporting a watch-scope violation, the reviewer checks
whether the Kubernetes resource is cluster-scoped or namespaced (e.g.
via `kubectl api-resources`): watching a cluster-scoped resource such
as Namespace across the cluster is not the same as aggregating
namespaced resources across all namespaces, and the two are not
interchangeable findings. Both checks are positive verification against
the spec and the resource's actual scope, not pattern-matching on the
shape of the code.

Origin: the #26 review reported `WatchKey`/`ResourceKind` as
Speculative Generality although the spec mandated the `(cluster, kind,
namespace)` watch architecture, and reported `Api::<Namespace>::all` as
prohibited cluster-wide watching although Namespace itself is
cluster-scoped and the acceptance criteria require a live Namespaces
list; both findings were withdrawn (#26/#27 retro).

## Test assertions in no-behaviour-change refactors

In a refactor ticket that forbids behaviour change, updating an
assertion's literals for a changed type (e.g. `"ns/name"` becoming
`ObjectKey::new(..)`) is mechanical and allowed. Changing what an
assertion checks, or deleting one, is a behaviour change and is a
finding.

Origin: #42 asked for unchanged test assertions while replacing string
row keys with `ObjectKey`; the implementer had to guess the line (#42
retro).

## Move-only refactors

In a ticket that only moves code between files, review the residue of
`dev/move-check.sh`, not the whole diff: it pairs removed and added
lines, visibility changes included. A `//!` doc on a new module file, the
`mod`/`use` lines the split needs, and rustfmt's re-wrap of a signature
that gained `pub(super)` are part of the move, not findings. Anything else
in the residue is a behaviour change and is a finding.

Origin: #112 split `resource_detail.rs` into three child modules; review
had to tell moved lines from changed ones by eye across a 1,000-line
diff (#112 retro).

## Cluster-dependent copy and choices

UI copy or choices that describe what the cluster will do (a delete's
cascade, which containers can be exec'd) are derived from the fields that
decide it (`ownerReferences`, `restartPolicy`, …), not written as a fixed
string per kind. A test asserting such copy or choices uses a fixture that
carries the deciding field, plus one that lacks it.

For a controller, deletion still garbage-collects dependents through their
`ownerReferences`; an empty related snapshot means no dependents are
currently listed, not that the delete has no cascade semantics. Reviewers
must not recommend "nothing else is deleted" from that empty state alone.
Tests cover both owned dependents and an empty related snapshot while
preserving the controller's cascade warning.

Origin: #17 — a static "will create a replacement" delete warning passed a
test whose Pod had no owner, and Exec offered run-once init containers that
had already exited; the spec review caught both, the standards review
neither (#17 retro).

## Closable dock panels

A change that adds a closable dock panel (a new `impl Panel for`) adds a
case to `crates/app/tests/dock_tabs.rs` that opens the panel and closes it
with `cmd-w`, and the panel's root carries
`.track_focus(&self.focus_handle)`. Only the detail-tab cases fail when
`SessionView` stops focusing a tab it shows, so a new panel's case guards
its own focus path, not that one.

Origin: #51 shipped the close test for detail tabs only; Logs, Exec and
Edit YAML tabs focus an inner terminal or editor and went untested until
the user asked (#51 retro).

## Upstream-alignment assertions

A projection test distinguishes missing (`None`), empty strings, empty
collections, and collections containing an empty item whenever upstream
renders those states differently. When upstream interleaves fields and
tables per item, assert a single ordered sequence at the real rendering
boundary so each child's values remain attached to that item; separate
assertions over all fields and all tables cannot catch interleaving
regressions.

Origin: #55 review found empty-value and repeated-ingress rendering bugs
that the existing presence/absence and separate field/table assertions did
not catch (#55 retro).

## Layout assertions

A UI test that asserts a size or an edge compares it, within 1px, with
the boundary the user sees: the inner viewport, not an outer wrapper. A
loose tolerance against the wrapper is a finding: it hides off-by-border
overflow.

Origin: #68's first test allowed 20px against the `list-table` wrapper,
and missed columns 2px wider than `DataTable`'s bordered viewport (#68
retro).

## Self-driven redraw assertions

A UI test claiming a view updates on its own (a timer, a subscription,
a notify) reads the frame the app last drew, not one `Ui::frame`
renders for the query, and keeps the window's other views quiet for the
interval it asserts. A test that passes because the query re-rendered
the view, or because a neighbour's notify redrew the window, is a
finding.

Origin: #70's detail-age test passed while the detail's own tick was
broken: `Ui::frame` re-rendered it, and the list's per-second tick
redrew the window (#70 retro).

## Docs that describe output

A README or agent-doc line in the diff that names a printed field, a log
line, or what a crate supplies matches what the diff's code prints or
uses, per crate. A claim true of one benchmark but written for all of
them is a finding.

Origin: #85's first draft said every scripted mode prints `load=` at
start and end and that the editor and terminal benchmarks took their
frame stats and load from `spikes/support`; neither was true yet (#85
retro).

## Tests that claim the Registration's path

A test of a built-in kind's list or detail calls the Registration with a
`DynamicObject`: `(BuiltinKind::X.registration().detail)(&object, now)`
or `.list`. `dev/registration-path.sh` (CI) fails a test that calls a
projection function the Registration wraps; review covers the rest: a
helper that rebuilds the Registration's glue (parsing, choosing which
JSON to pass) some other way is a finding, since the copy drifts from
the Registration and passes when the app's path differs.

Origin: #100's test helper rebuilt the Services Registration's
`try_parse` and `.data` call; its first version passed `metadata` the
app never sees, and neither reviewer suggested calling the Registration
(#100 retro). The rule then took one ticket per file (#114, #121) until
the check covered every test (#114 retro).

## View-test steps the support module already has

A view test opens a built-in kind's detail with `support::open_detail`
and builds a `DynamicObject` with `support::object`. A local helper or
an inline run that repeats `open_detail`'s steps (select the namespace,
click the nav item, send the snapshot, click the row) is a finding,
whatever its name.

Origin: #101 shared these helpers after four files had copied them; a
fifth copy, `workload_views`' `open_pod_detail`, was missed because it
had another name, and only the review found it (#101 retro).

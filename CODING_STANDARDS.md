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

## Cluster-dependent copy and choices

UI copy or choices that describe what the cluster will do (a delete's
cascade, which containers can be exec'd) are derived from the fields that
decide it (`ownerReferences`, `restartPolicy`, …), not written as a fixed
string per kind. A test asserting such copy or choices uses a fixture that
carries the deciding field, plus one that lacks it.

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

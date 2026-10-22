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
unverified is phrased as an open question, not a mechanism.

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

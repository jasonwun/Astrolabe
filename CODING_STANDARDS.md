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

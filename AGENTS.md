# Astrolabe

## Language

Reply to the user in Chinese. Everything else — code, comments, commits, issues, docs — in English.

## Commits

Commit messages created by an agent must end with a `Co-Authored-By:` trailer naming the agent that actually did the work (e.g. `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, `Co-Authored-By: Kimi Code <noreply@moonshot.ai>`). Never copy another agent's trailer. The trailer belongs to commit messages only; issue and PR bodies end with their content.

## Agent skills

### Issue tracker

Issues and specs are tracked as GitHub issues on jasonwun/Astrolabe, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Testing

Coverage, mutation testing, and cluster e2e commands: `docs/agents/testing.md`. Before a dependency-upgrade PR: `docs/agents/spike-regression.md`. Review-time judgement rules: `CODING_STANDARDS.md`.

### GPUI views and UI tests

Before writing a DockArea panel or a `gpui_kit::test` UI test: `docs/agents/gpui-kit.md`.

### Upstream (Freelens)

Before writing columns, fields, or behaviour an issue aligns with Lens/Freelens: fetch the upstream source per `docs/agents/reference-products.md`.

### Manual UI check

Before handing over a change that shows in the app: launch it on kind and screenshot it, per `docs/agents/manual-check.md`.

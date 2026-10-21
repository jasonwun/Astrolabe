# Astrolabe

## Language

Reply to the user in Chinese. Everything else — code, comments, commits, issues, docs — in English.

## Commits

Commits created by the agent must end with the trailer `Co-Authored-By: Kimi Code <noreply@moonshot.ai>`.

## Agent skills

### Issue tracker

Issues and specs are tracked as GitHub issues on jasonwun/Astrolabe, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

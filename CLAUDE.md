# control-tower

A taskboard for a team of Claude Code agents in herdr: what the work is, how far
along it is, and where the owner is needed.

- Spec: `control-tower spec.md`, which the owner keeps outside this repo.
- UI: the frozen, signed-off files kept with the spec; the rules for a UI change are in SDLC.md.
- No em dashes in code, UI text or docs.

## How work is done

Process, roles, quality gate and testing: SDLC.md. Build, run and `TOWER_DB`: README.md.

## After a release

`brew upgrade control-tower` and check the watch's log
(`$(brew --prefix)/var/log/control-tower.log`) shows it restarting on the new version.

## Agent skills

### Issue tracker

Issues live in GitHub Issues on daderoode/control-tower, via `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels (needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root, created when needed. See `docs/agents/domain.md`.

---
id: L-01
title: Global skills dir loader
pillar: Language
status: ready
priority: 1
design:
---

# Global skills dir loader

## Goal
flux currently loads skills only from the project's local/relative `.flux/skills`. Support a
**user/global** skills dir (e.g. `~/.flux/skills`) merged with the project dir, so global skills
need not be copied into every project. This is the root cause of the accidental skill copies that
accumulated in this repo's `.flux/skills` (they were duplicated from `~/.claude/skills` because only
the local dir was readable).

## Acceptance
- [ ] Skills are discovered from **both** a user/global dir (`~/.flux/skills`, honoring config
      precedence: CLI > project `.flux/` > user `~/.flux/` > defaults) **and** the project
      `.flux/skills`.
- [ ] On a name collision, **project overrides global**, with the precedence documented.
- [ ] Failing-first test covering three cases: global-only, project-only, and both-with-override.
- [ ] Docs updated: README "Skills" section and AGENTS.md "Add a skill".

## Progress
- (not started)

## Notes
- Skill types live in `flux-skill` (L0); discovery/injection is wired through the context/agent path
  — find where `.flux/skills` is read today and add the global dir alongside it.
- Mirrors the existing config precedence already documented in README (`.flux/config.toml`:
  CLI > project > `~/.flux/config.toml` > defaults).

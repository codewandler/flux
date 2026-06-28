---
id: L-01
title: Global, multi-format skill loading
pillar: Language
status: done
priority: 1
design:
---

# Global, multi-format skill loading

## Goal
flux loaded skills only from the project's local `.flux/skills`, trigger-only and CLI-buried. Support
**user/global** dirs merged with the project dir — and read the **cross-agent skill formats** (Agent
Skills / Claude) — so the skills the user already keeps for other agents work in flux without
per-project copies (the root cause of the accidental `~/.claude/skills` → `.flux/skills` copies).

## Acceptance
- [x] Skills discovered from the project `.flux/skills` **and** the user/global dirs
      (`~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`); precedence documented.
- [x] On a name collision, **project overrides global** (`flux_skill::discover_merged`, dedup before
      sort; dir list from `flux_skill::default_skill_dirs`, reusable by the SDK).
- [x] Reads the **Agent Skills/Claude format** (`name` + `description`, no `triggers`) plus nested
      `metadata.triggers`; trigger-less skills activate on `name`/`description` keywords.
- [x] Failing-first tests: global-only / project-only / both-with-override; Claude-no-triggers
      activates (and doesn't over-fire); nested-metadata triggers; `active_for` rank + cap.
- [x] Docs updated: README "Skills", AGENTS.md "Add a skill", architecture crate map, CHANGELOG.

## Progress
- Done. New `flux-markdown` (L0) crate owns frontmatter parsing (shared by `flux-skill` +
  `flux-orchestrate`) and wraps `codewandler/markdown` for the TUI/CLI render paths (off-by-default
  features). `flux-skill` is now multi-format with `SkillFormat`, `default_skill_dirs`,
  `discover_merged`, a `name`/`description` activation fallback, and `active_for` (ranked + capped),
  used by both the `flux-flow` and `flux-agent` injection sites. Gate green.

## Notes
- Deferred to **L-02**: goldmark-style AST markdown parser, full render consolidation,
  progressive-disclosure activation (name+description always, body on demand), a config key for
  custom skill dirs, and populating `flux-agent`/SDK skills via `default_skill_dirs`.
- The CLI > project > user precedence note from the original scope referred to dir precedence
  (project beats user), not a new CLI flag; the well-known dir set is hardcoded for now.

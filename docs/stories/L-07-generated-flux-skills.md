---
id: L-07
title: Generate Flux skills from live catalogs
pillar: Language
status: done
design: docs/designs/generated-flux-skills.md
note: "`flux skill` renders Claude-format root/CLI/language/plugin/ops skills from live Clap, Flux-Lang, ToolRegistry/OpRegistry, and plugin-manifest sources; `--install` writes root + sections, and project `.claude/skills` is loaded by default after `.flux/skills`"
---

# Generate Flux skills from live catalogs

## Goal
Ship a `flux skill` surface that renders Claude-format skills for Flux itself, with every generated
catalog grounded in the owning source of truth instead of hand-maintained docs.

## Acceptance
- [x] `flux skill` renders a root skill that points agents to `cli`, `lang`, `plugin`, and `ops`
  section skills.
- [x] `flux skill cli|lang|plugin|ops` renders Claude-format `SKILL.md` content for that section.
- [x] `flux skill --install` installs root + all sections; `flux skill <type> --install` installs root
  + that section; `--global` targets `~/.claude/skills`.
- [x] `flux plugin skill` remains available as a plugin-skill alias.
- [x] Project-local `.claude/skills` is loaded by default after `.flux/skills`.
- [x] Tests cover source-of-truth renderers, install layout, command help, and discovery precedence.

## Progress
- Implemented in `flux-cli` and `flux-skill`.
- Focused gates: `cargo test -p flux-skill`, `cargo test -p flux-cli`.

## Notes
- Design: [generated-flux-skills.md](../designs/generated-flux-skills.md).

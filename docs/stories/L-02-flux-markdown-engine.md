---
id: L-02
title: flux-markdown engine + progressive-disclosure skills
pillar: Language
status: backlog
note: AST parser, body-on-demand activation
---

# flux-markdown engine + progressive-disclosure skills

## Goal
Grow `flux-markdown` from a frontmatter parser + render *wrapper* into a first-class markdown engine,
and replace flux's "inject the whole skill body on match" activation with standards-aligned
**progressive disclosure** — so global skills scale without prompt bloat. Builds on L-01.

## Acceptance
- [ ] A goldmark-style, AST-based, extensible markdown parser in `flux-markdown` (own engine, not a
      wrapper); the `ratatui`/`terminal` render paths build on it. Round-trip + render parity tests.
- [ ] Progressive-disclosure skill activation: only `name` + `description` are loaded at startup; a
      skill's body is pulled on demand when the model/engine selects it (Level-1 vs Level-2 loading).
      Failing-first test proving the body is *not* injected until selected.
- [ ] A config key (`.flux/config.toml`) for **custom** skill dirs, layered with the hardcoded
      well-known set (CLI > project > user > defaults).
- [ ] `flux-agent`/SDK populate skills via `flux_skill::default_skill_dirs` (today only the CLI does).

## Progress
- (not started) — carved out of L-01 as the genuinely deferred work.

## Notes
- L-01 shipped: `flux-markdown` (frontmatter + feature-gated render wrappers over
  `codewandler/markdown`), multi-format `flux-skill` with `active_for` (ranked + capped). The cap is
  the interim guard that progressive disclosure should make unnecessary.
- The over-activation risk lives in `flux-flow/src/engine.rs` + `flux-agent/src/lib.rs` (both route
  through `flux_skill::active_for`).
- Spec references: Claude Agent Skills + agentskills.io (progressive disclosure: Level 1 metadata
  always, Level 2 body on trigger, Level 3 resources on demand).
- Relates to A-01 (unify the SDK loop) for the skill-population item.
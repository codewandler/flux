---
id: L-02
title: flux-markdown engine + progressive-disclosure skills
pillar: Language
status: done
priority:
note: flux-markdown is now its OWN two-pass engine (goldmark-style AST: recursive block pass + delimiter-stack inlines; zero external parser deps — the old wrapper crates survive only as dev-dep parity oracles with exact per-line ANSI/ratatui parity pinned); skills load Level-1 (frontmatter head-scan only) and the body reads lazily exactly at injection via the SkillBody Display; [skills] dirs config key layered CLI > project > user > defaults; agent + SDK now populate via default_skill_dirs; live-verified skill activation through the real engine
---

# flux-markdown engine + progressive-disclosure skills

## Goal
Grow `flux-markdown` from a frontmatter parser + render *wrapper* into a first-class markdown engine,
and replace flux's "inject the whole skill body on match" activation with standards-aligned
**progressive disclosure** — so global skills scale without prompt bloat. Builds on L-01.

## Acceptance
- [x] A goldmark-style, AST-based, extensible markdown parser in `flux-markdown` (own engine, not a
      wrapper); the `ratatui`/`terminal` render paths build on it. Round-trip + render parity tests.
- [x] Progressive-disclosure skill activation: only `name` + `description` are loaded at startup; a
      skill's body is pulled on demand when the model/engine selects it (Level-1 vs Level-2 loading).
      Failing-first test proving the body is *not* injected until selected.
- [x] A config key (`.flux/config.toml`) for **custom** skill dirs, layered with the hardcoded
      well-known set (CLI > project > user > defaults).
- [x] `flux-agent`/SDK populate skills via `flux_skill::default_skill_dirs` (today only the CLI does).

## Progress
- **Done (2026-07-02).** All four items:
  - **Own engine:** two-pass, zero external parser deps — a recursive line-based block pass
    (headings, fenced code, blockquotes, nested tight/loose lists per CommonMark, thematic
    breaks, GFM tables, HTML comment blocks) feeding a CommonMark delimiter-stack inline pass
    (escapes, code spans, links/images, autolinks, hard/soft breaks, flanking+mod-3 emphasis,
    GFM strikethrough). New `ast/parser/inline/writer` modules + a shared width-aware `render/`
    layout core feeding BOTH renderers; the old `render::render`/`render_ansi`/`LiveRenderer`
    APIs are preserved exactly (flux-tui/flux-cli compile unmodified). The old
    markdown-stream/-ratatui/-terminal crates survive only as [dev-dependencies] parity oracles:
    exact per-line ANSI + ratatui parity pinned at 80/24 over a 16-snippet suite + 9 committed
    repo-doc corpus fixtures (2 files under a documented waiver for oracle bugs; 2 pinned
    deliberate fixes over the oracle — nested-list bullets, code-in-list ordering). Round-trip
    law (`parse(write(parse(x))) == parse(x)`) on the corpus + writer fixed-point. NOT parsed
    (documented): setext headings, indented code, general HTML, reference links, entities,
    task-list checkboxes, footnotes, lazy continuations.
  - **Progressive disclosure:** `Skill.body` became the lazy `SkillBody` (Display/serde-
    compatible) — discovery reads only a frontmatter head-scan (64 KiB cap) + a stat;
    `active_for` selects/caps on metadata alone; the Level-2 read happens exactly at injection
    (flux-flow's untouched `format!("{}")`); unselected bodies never load (failing-first swap
    test); vanished files degrade to empty.
  - **Config:** `[skills] dirs` (project-before-user merge, `~/` expansion) + a repeatable
    `--skill-dir` CLI flag — layering CLI > project > user > defaults pinned by test.
  - **Agent/SDK parity:** `AgentSpec::try_with_default_skills` + the SDK builder populate via
    `default_skill_dirs`.
  - Live-verified: a scratch-workspace `flux run -m mock` activates a project skill through the
    real engine.
- **Residuals:** terminal fenced-code syntax highlighting dropped (uniform code color, matching
  the old ratatui path); the oracle crates linger in Cargo.lock as dev-deps until the parity
  tests retire; >64 KiB frontmatter degrades to "no frontmatter" (mirrors split_frontmatter's
  lenience); the SDK always discovers default-dir skills (no opt-out setter yet).

## Notes
- L-01 shipped: `flux-markdown` (frontmatter + feature-gated render wrappers over
  `codewandler/markdown`), multi-format `flux-skill` with `active_for` (ranked + capped). The cap is
  the interim guard that progressive disclosure should make unnecessary.
- The over-activation risk lives in `flux-flow/src/engine.rs` + `flux-agent/src/lib.rs` (both route
  through `flux_skill::active_for`).
- Spec references: Claude Agent Skills + agentskills.io (progressive disclosure: Level 1 metadata
  always, Level 2 body on trigger, Level 3 resources on demand).
- Relates to A-01 (unify the SDK loop) for the skill-population item.

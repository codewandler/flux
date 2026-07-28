---
id: D-189
title: Stop silently dropping skill frontmatter — lint, warn, and honor allowed-tools/model
pillar: Agent
status: done
priority:
epic: claude-interop
design: docs/designs/claude-interop.md
note: "allowed-tools/model/context/hooks currently vanish in serde with no warning; validate() is dead code"
---

# Stop silently dropping skill frontmatter — lint, warn, and honor allowed-tools/model

## Goal
Make skill loading honest about Claude frontmatter: recognized-but-unsupported fields warn once at
load instead of vanishing, `flux_skill::validate()` (Agent Skills naming rules — currently never
called) runs as a discovery-time lint, and the fields flux has real equivalents for —
`allowed-tools` (→ op allowlist) and `model` (→ model override) — are honored.

## Acceptance
- [x] `SkillFrontmatter` (`crates/flux-skill/src/lib.rs`) captures the known Claude field set;
      recognized-unsupported fields (`context`, `hooks`, `license`, `compatibility`, …) produce one
      load-time warning naming the skill and field. Truly unknown keys stay silent.
- [x] `validate()` is wired into discovery (warn-level, not fatal) — a skill with an invalid name or
      oversized description reports it; failing-first test.
- [x] `allowed-tools` translates Claude tool names to flux ops via an explicit table; unmappable
      entries warn + ignore. When an `allowed-tools` skill is active, the turn's surfaced ops are
      constrained accordingly; test proves an out-of-allowlist op is not offered.
- [x] `model` on a skill applies as a model override for turns where the skill is active (same
      resolution as role files' `model`); test.
- [x] `disable-model-invocation` and `argument-hint` parse without warning (consumed by D-188/D-186).
- [x] Docs: the claude-compat page's frontmatter matrix (supported / honored / warned / ignored)
      matches the code — keep the table and this story in lockstep.

## Progress

Implemented end-to-end 2026-07-28:

- **`crates/flux-skill/src/lib.rs` (L0, pure)**: `SkillFrontmatter` gained `allowed-tools` (→
  `de_string_list`, shared with `triggers`), `model`, `disable-model-invocation`,
  `argument-hint`, plus presence-only fields for `context`/`agent`/`hooks`/`license`/`compatibility`.
  `Skill` gained `allowed_ops: Vec<String>`, `model: Option<String>`, `disable_model_invocation:
  bool`, `argument_hint: String`. New `parse_checked`/`assemble` return `(Skill, Vec<String>)` —
  `parse` stays the discard-warnings compat wrapper. New `translate_allowed_tool` + the
  `ALLOWED_TOOLS_MAP` table (`Bash`→`bash`, `Edit`→`edit`, `Read`→`read`, `Grep`→`grep`,
  `Glob`→`glob`, `Write`→`write`, `WebFetch`→`web.fetch`, `WebSearch`→`web.search`, `Task`→`task`).
  `validate()` unchanged (already pure) — just gets called now. 8 new tests.
- **`crates/flux-runtime/src/metadata.rs` (L2, warn emission)**: new `SkillDiscovery { skills,
  warnings }` (mirrors `CommandDiscovery`); `discover_skills_from` returns it, `discover_skills`
  stays `Vec<Skill>` (discards warnings — 2 production call sites, both fixed). `parse_skill` now
  calls `flux_skill::parse_checked` + `flux_skill::validate` (directory-skills only checked against
  their expected dir name) and collects every issue into `warnings`. 3 new tests plus the existing
  duplicate-name warning test unchanged.
- **`crates/flux-cli/src/execution.rs`**: `load_skills` prints `discovery.warnings` (same pattern as
  `load_command_files`). New `resolve_model_spec_with_skill` (precedence `--model` > skill `model` >
  config `model` > `sonnet`, mirroring `Role::to_spec`'s `model.unwrap_or(default_model)`).
  `build_agent_with` now loads skills *before* resolving the model spec (skills must be known before
  the primary provider is built) and threads the already-loaded `Vec<Skill>` through `EngineParts`
  into `assemble_engine`, replacing the second discovery call that used to happen there. 1 new test.
- **`crates/flux-flow/src/engine.rs`**: new `FlowEngine::narrow_by_skill_allowed_tools`, called from
  `surfaced_for_turn` right after group/policy gating computes `advertised` — intersects with the
  union of every active skill's `allowed_ops`; a no-allowlist skill (or no active skill) is a no-op.
  This lives at the turn's surfaced-ops computation, not inside `base_system_with_skills`'s
  `<skill>` injection block (kept untouched, as directed — a concurrent D-190 change was landing a
  `path=` attribute there at the same time and both changes compiled together cleanly). 2 new tests.
- **Docs**: `website/docs/agent/claude-compat.md` frontmatter matrix rewritten — `allowed-tools` and
  `model` now say Honored (with the translation table and precedence spelled out),
  `disable-model-invocation`/`argument-hint` say "parsed silently, inert until D-186/D-188 ship",
  `context`/`agent`/`hooks`/`license`/`compatibility` say "recognized but unsupported" (warns).
  `website/docs/agent/skills-and-roles.md` gained an authoring example for `allowed-tools`/`model`.
  `docs/designs/claude-interop.md` Risks section: both open questions marked **Resolved (D-189)**
  with the table and the precedence-chain decision.
- **Verify**: `cargo test -p codewandler-flux-skill -p codewandler-flux-runtime -p flux-cli -p
  codewandler-flux-agent -p codewandler-flux-flow` — all green (22 + 184 + 169(+13 integration) +
  22 + 107 tests). `cargo clippy --all-targets -- -D warnings` clean on all five crates.
  `cargo fmt` applied. `cargo test -p flux-codegate` (layering lint) green — no new crate deps.
  `cargo test -p flux-cli --test website_contract` green.

## Notes
- Roles (`.flux/agents/*.md`) already parse `model`/`tools` — reuse that resolution logic rather
  than a second mapping (`crates/flux-cli/src/execution.rs`).
- Decide precedence when both the session and a skill specify a model: explicit CLI/SDK wins;
  record in the design doc. **Decided**: `--model`/SDK explicit > skill `model` > config `model` >
  `sonnet` (`resolve_model_spec_with_skill`); recorded in `docs/designs/claude-interop.md` Risks.

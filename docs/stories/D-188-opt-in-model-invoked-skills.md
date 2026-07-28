---
id: D-188
title: Opt-in progressive skill disclosure — surface descriptions, load bodies on demand
pillar: Agent
status: done
priority:
epic: claude-interop
design: docs/designs/claude-interop.md
note: "Claude-style model-invoked skills behind an explicit opt-in; manual --skill stays the default"
---

# Opt-in progressive skill disclosure — surface descriptions, load bodies on demand

## Goal
Add an opt-in mode (flag + config) in which every discovered skill's name+description is surfaced to
the model and the model can pull a skill's body into context on demand — Claude Code's progressive
disclosure, for users who want that ergonomics and accept the token cost. Manual `--skill`
activation remains the default per `docs/designs/manual-skill-activation.md` (measured 18% token
reduction); this story must not regress the default path.

## Acceptance
- [x] An explicit opt-in (CLI flag + `[skills]` config key + SDK builder knob) surfaces discovered
      skills' name+description compactly; bodies are NOT injected until requested.
- [x] A guarded mechanism loads a named skill's body mid-turn (design decides: dedicated op vs the
      D-187 invoke op); loading emits the existing `skill.activated` observation.
- [x] `disable-model-invocation: true` in frontmatter excludes a skill from surfacing/loading in
      this mode (and warns nowhere — it's supported, not dropped).
- [x] Default behavior unchanged: with the opt-in off, no descriptions are surfaced and
      `skills_are_disabled_until_named_explicitly` (`crates/flux-cli/src/main.rs`) still passes.
- [x] Failing-first tests: surfacing list content, on-demand load, disable-model-invocation
      exclusion, default-off invariant.
- [x] Docs: claude-compat page's semantics-delta section updated (manual default, opt-in
      disclosure), `skills-and-roles.md` gains the opt-in.

## Progress

Implemented end-to-end 2026-07-28.

**Mechanism decision** (recorded in `docs/designs/claude-interop.md` Risks, "Resolved (D-188)"): a
**dedicated op**, `skill.load(name)` in `crates/flux-tools/src/skill_load.rs` — not a reuse of
D-187's `command.invoke` (the two gate on genuinely different things: `agent-triggerable`+policy+
discovery vs. "is the opt-in catalog non-empty"). Registered unconditionally in
`try_register_builtins` (same stance as `observe`/`evidence` — registry presence isn't exposure);
advertised only when `FlowEngine::narrow_by_skill_catalog` finds a non-empty catalog. **Loaded
skills persist**: recorded per-session on `EngineLoopHost` and re-injected as a full body on every
later turn of that session — the same treatment (and the same `skill.activated` observation) an
explicitly `--skill`-activated skill gets, so activation has one consistent semantics regardless of
which path turned it on.

- **`crates/flux-runtime/src/lib.rs` (L2)**: new `SkillLoadOutcome` + `SkillLoader` trait (mirrors
  `CompositeRegistrar`); `ToolContext.skill_loader: Option<Arc<dyn SkillLoader>>` +
  `with_skill_loader`/`set_skill_loader`.
- **`crates/flux-flow/src/loop_host.rs`**: `EngineLoopHost` gained `skill_catalog: Mutex<Vec<Skill>>`
  + `loaded_skills: Mutex<HashMap<session, HashSet<name>>>`, `set_skill_catalog`/`skill_catalog`/
  `loaded_skill_names`, and `impl SkillLoader for EngineLoopHost` (installed alongside
  `CompositeRegistrar` in `EngineLoopHost::install`).
- **`crates/flux-flow/src/engine.rs`**: `FlowEngine::with_model_invoked_skills(catalog)` builder;
  `base_system_with_skills` (now session-scoped) injects loaded catalog skills' bodies plus a new
  `render_skill_catalog` compact `<available-skills>` block (name/path/description, never bodies)
  whenever the catalog is non-empty; new `narrow_by_skill_catalog` removes `skill.load` from the
  advertised set whenever the catalog is empty (the default). 5 new tests.
- **`crates/flux-tools/src/skill_load.rs` (new file)**: `SkillLoadOp` — delegates to
  `ctx.skill_loader`, records the `skill.activated` observation directly on `ctx.evidence` (same
  event shape manual activation emits). Registered in `try_register_builtins`
  (`crates/flux-tools/src/lib.rs`); `builtins_register`'s exact-name list gained `"skill.load"`.
  3 unit tests.
- **`crates/flux-agent/src/lib.rs`**: `AgentSpec.model_invoked_skills: Vec<Skill>` (empty = off) +
  `AgentSpec::try_with_model_invoked_skills()` (discovers + filters
  `disable_model_invocation`); threaded through `into_engine`. 1 test.
- **`crates/flux-config/src/lib.rs`**: `[skills] model_invoked: bool` (OR-merged project/user, same
  pattern as `enable_shell`).
- **`crates/flux-cli/src/args.rs`**: `--skills-model-invoked` flag. **`execution.rs`**:
  `load_model_invoked_skill_catalog` (shares directory-walk logic with `load_skills` via a new
  private `discover_skills` helper, but is a SEPARATE function — `load_skills`'s signature and the
  pinned `skills_are_disabled_until_named_explicitly` test are untouched); wired through
  `EngineParts`/`assemble_engine` into `AgentSpec.model_invoked_skills`. 2 tests in `main.rs`.
- **`crates/flux-sdk/src/lib.rs`**: `ClientBuilder::model_invoked_skills()` bool knob, resolved at
  `build()` time (root is only known there) via `AgentSpec::try_with_model_invoked_skills`.
- **Docs**: `website/docs/agent/claude-compat.md` — replaced the stale "an opt-in ... mode is
  planned" sentence and added a "Model-invoked skills (opt-in)" section; the
  `disable-model-invocation` frontmatter-matrix row now says Honored. `website/docs/agent/
  skills-and-roles.md` gained a "Model-invoked skills (opt-in)" section (flag/config/SDK).
  `website/docs/reference/config.md` documents `[skills] model_invoked`.
  `crates/flux-flow/docs/ops-reference.md` + `website/docs/language/ops.md` gained `skill.load`.
- **Adjacent pre-existing fix**: `crates/flux-sdk/src/lib.rs`'s
  `sdk_skills_require_an_explicit_agent_spec` asserted the injected `<skill>` tag with no `path`
  attribute — stale against D-190 (which had already landed in this worktree, unrelated to D-188);
  fixed the assertion to match the current `path="..."`-bearing tag so the crate's targeted gate is
  green. Not otherwise in scope for this story.
- **Verify**: `CARGO_TARGET_DIR=/home/timo/projects/flux/target cargo test -p codewandler-flux-tools
  -p codewandler-flux-flow -p flux-cli -p codewandler-flux-sdk -p codewandler-flux-config` — all
  green (27+188+58+4+11+3+3+3+0+19+5+1+2+1+8+149+1+171+3+2+3+1+3+13 tests across the five crates'
  unit/integration suites, 0 failed). `cargo clippy -p <same seven crates incl. flux-runtime/
  flux-agent> --all-targets -- -D warnings` clean. `cargo fmt -p <same seven, individually>`
  applied. `cargo test -p flux-codegate` green (11 tests, layering intact — no new crate deps).
  `cargo test -p flux-cli --test website_contract` green, including
  `operations_reference_covers_the_registered_public_catalog` (now requires `skill.load` in
  `website/docs/language/ops.md`) and `public_config_examples_deserialize_and_have_effect`.

## Open items
- None blocking. `AgentSpec.model_invoked_skills` + `.skills` can both be non-empty simultaneously
  (additive) — not tested as a combination, but each path's injection code is independent and
  covered separately.

## Notes
- Reuses discovery as-is; this is a surfacing/activation change in `flux-flow`'s engine prompt
  assembly (`crates/flux-flow/src/engine.rs`) + CLI/SDK plumbing (`crates/flux-cli/src/execution.rs`,
  `AgentSpec.skills`).
- The dead `active_for`/trigger-ranking code in `flux-skill` is NOT the mechanism here (see D-192);
  disclosure is model-driven, not keyword-matched.

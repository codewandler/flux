---
id: D-187
title: Let the agent invoke commands and skills behind three fail-closed gates (absorbs C-93)
pillar: Agent
status: done
priority:
epic: claude-interop
design: docs/designs/claude-interop.md
note: "guarded op: permitted ∧ accessible ∧ agent-triggerable, through Executor::dispatch; supersedes C-93"
---

# Let the agent invoke commands and skills behind three fail-closed gates (absorbs C-93)

## Goal
Give the agent loop a guarded way to invoke a discovered command or skill mid-turn — only when the
caller's policy **permits** it, the target is **accessible** in the session, and the target is
explicitly marked **agent-triggerable** (default false). This absorbs C-93 unchanged in contract and
extends it to the file-based commands D-186 introduces. No bypass path: invocation traverses
`Executor::dispatch` (policy → approval → guarded IO) under the turn's frozen `TurnIdentity`.

## Acceptance
- [x] All of C-93's acceptance carries over: the agent-triggerable flag (explicit, default false,
      distinct from inert `triggers:`), the guarded op with accurate
      `effects`/`permission_subjects`/`intents`, independent fail-closed gates, identity
      preservation, evidence-gated surfacing, and the four-way gate test matrix.
- [x] File-based commands (D-186) declare agent-triggerability in frontmatter; skills declare it in
      theirs. Where the flag lives and what the invocation contract is (effect vs narrower
      capability) is decided in a design addendum to `docs/designs/claude-interop.md` before code.
- [x] Catalog/docs sync: `crates/flux-flow/docs/ops-reference.md`, tool group registration, and the
      claude-compat page's semantics table.
- [x] C-93 closed as superseded with a pointer to this story.

## Progress

**Implemented 2026-07-28.**

### Design decision (recorded in `docs/designs/claude-interop.md`, Risks section, "Resolved (D-187)")
- The narrower capability, not a slash command's/skill's human-side REPL/TUI effect: invoking a
  command expands `$ARGUMENTS`/`$1..$9` and returns the substituted body as the op's `ToolResult`
  text (prompt material for the model's current turn — no nested turn, no execution). Invoking a
  skill returns its body verbatim (equivalent to `read`ing it).
- `agent-triggerable: true` (default `false`) is a new frontmatter key, parsed silently (no lint
  warning either way), on BOTH `CommandFile` (`crates/flux-runtime/src/metadata.rs`) and `Skill`
  (`crates/flux-skill/src/lib.rs::SkillFrontmatter`) — a separate axis from D-188's
  `disable-model-invocation` (passive surfacing) and D-189's `allowed-tools`/`model`.

### flux-runtime (L2) — `crates/flux-runtime/src/metadata.rs`, `crates/flux-runtime/src/lib.rs`
- `CommandFile.agent_triggerable: bool` + `CommandFrontmatter."agent-triggerable"`.
- New evidence signal `agent_triggerable` in `detect_signals`: re-runs `discover_commands` +
  `discover_skills` on `cwd` and fires when any discovered target opts in; discovery errors degrade
  to "no signal" (lenient, same posture as every other marker). Moved the test-only `HOME_LOCK`
  mutex from `metadata::tests` to `metadata::HOME_LOCK` (`pub(crate)`, `#[cfg(test)]`) so
  `flux_runtime::tests` can serialize `HOME` repointing against `metadata::tests` too.
- Tests: `metadata::tests::command_agent_triggerable_flag_parses_silently_and_defaults_false`,
  `tests::detect_signals_surfaces_agent_triggerable_only_when_a_target_opts_in`.

### flux-skill (L0) — `crates/flux-skill/src/lib.rs`
- `Skill.agent_triggerable: bool` + `SkillFrontmatter."agent-triggerable"`, wired through
  `assemble()`; added to the "Honored" frontmatter tier in the module's own doc comment.
- Test: `tests::agent_triggerable_flag_parses_silently_and_defaults_false`.
- Updated existing `Skill { .. }` struct-literal test fixtures (this crate, `flux-flow/src/engine.rs`,
  `flux-tui/src/lib.rs`) and `CommandFile { .. }` fixtures (`flux-tui/src/lib.rs`) for the new field.

### flux-tools (L2) — `crates/flux-tools/src/command_invoke.rs` (new file)
- `command.invoke(kind: "command" | "skill", name, arguments?)`: a `flux_runtime::Tool` impl.
  - **permitted** — `authority_requirements` overrides the default declaration-derived path,
    returning one `AuthorityRequirement::new("command.invoke", ResourceRef::named(Operation,
    "{kind}:{name}"))` per `permission_subjects` entry, so `Executor::dispatch`'s policy gate
    denies unless a matching grant exists — enforced entirely by the shared envelope, before
    `execute` runs.
  - **accessible** — `execute` re-runs `flux_runtime::metadata::discover_commands` /
    `discover_skills` against `ctx.system.workspace().root()` — the same discovery
    `detect_signals` used to raise the evidence signal, so the two can never disagree.
  - **agent-triggerable** — checks the matched target's own flag; false → refused.
  - Any missing gate (2) or (3) returns `ToolResult::error(..)` (clean, recoverable — never a hard
    `Err`, never partial execution). A command match expands arguments via the existing
    `expand_command_arguments`; a skill match returns `skill.body.text()` verbatim.
  - `effects: [Read, Filesystem]`, `risk: Low`, `idempotency: Idempotent`, `group:
    Some("agent_invoke")`.
- Registered unconditionally in `try_register_builtins` (existence in the registry is not exposure —
  same posture as `observe`/`evidence`/`skill.load`); added to the `builtins_register` expected-name
  list.
- Tests (gate matrix + identity + surfacing):
  `triggerable_permitted_accessible_command_runs` (a), `human_only_target_is_refused` (b),
  `inaccessible_target_is_refused` (c), `policy_denied_target_is_refused` (d),
  `triggerable_permitted_accessible_skill_runs` (skill counterpart of a),
  `dispatch_does_not_touch_frozen_turn_identity` (scopes a `TurnIdentity` via
  `scope_runtime_turn`, dispatches, then asserts the executor's default caller — read via
  `approval_context()` — is unaffected outside the scope), `spec_carries_the_agent_invoke_group_tag`.

### flux-tools (L2) — `crates/flux-tools/src/groups.rs`
- New `ToolGroup` `agent_invoke` — `tools: ["command.invoke"]`, `surface_when: when("agent_triggerable")`.

### Docs
- `crates/flux-flow/docs/ops-reference.md` — `command.invoke` row in the quick-reference table.
- `website/docs/language/ops.md` — new "Agent-invoked commands and skills" section (contract-tested
  by `flux-cli`'s `website_contract::operations_reference_covers_the_registered_public_catalog`).
- `website/docs/agent/claude-compat.md` — replaced the stale "Human-only today" paragraph under
  Slash commands with an "Agent-side invocation" section describing the three gates and the
  narrower-capability contract; added `agent-triggerable` to the skill frontmatter table and the
  command-file frontmatter paragraph.
- `website/docs/agent/skills-and-roles.md` and `website/docs/agent/cli.md` — mention the
  `agent-triggerable` flag with a cross-link to the new claude-compat section.
- `docs/designs/claude-interop.md` — "Resolved (D-187)" entry under Risks & open questions,
  answering C-93's carried-over open question (effect vs narrower capability) and recording the
  accessible-gate/evidence-signal symmetry decision.

### Verification
- `CARGO_TARGET_DIR=.../target cargo test -p codewandler-flux-tools -p codewandler-flux-flow -p
  codewandler-flux-runtime -p codewandler-flux-skill -p flux-cli` — all green except
  `flux-cli`'s `website_contract::operations_reference_covers_the_registered_public_catalog`, which
  fails on a **pre-existing, unrelated** gap (`skill.load`, D-188's concurrently-landing op, missing
  its own website doc entry at the time of this run) — the failure message names `skill.load`, not
  `command.invoke`; re-run once D-188 lands its doc to confirm both are covered.
- `cargo clippy -p codewandler-flux-tools -p codewandler-flux-flow -p codewandler-flux-runtime -p
  codewandler-flux-skill -p flux-cli --all-targets -- -D warnings` — clean.
- `cargo fmt -p codewandler-flux-tools -p codewandler-flux-flow -p codewandler-flux-runtime -p
  codewandler-flux-skill -p flux-cli` — clean (no diff).
- `cargo test -p flux-codegate` — clean (layering intact; `flux-tools` stayed L2, no new
  cross-layer dependency).

### Known gap (not this story's scope)
- The website `operations_reference_covers_the_registered_public_catalog` contract test is red at
  time of writing due to D-188's `skill.load` op landing concurrently in the same worktree without
  its website doc entry yet. Not caused by this story; `command.invoke` itself is fully covered.

## Notes
- Original analysis and invariants: `docs/stories/C-93-agent-invoke-commands-skills.md`.
- Depends on D-186 for file-based commands; skill invocation can land independently.

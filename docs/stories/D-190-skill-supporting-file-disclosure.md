---
id: D-190
title: Disclose a skill's directory path so the model can reach its supporting files
pillar: Agent
status: done
priority:
epic: claude-interop
design: docs/designs/claude-interop.md
note: "multi-file Claude skills (references/, scripts) currently degrade to the SKILL.md body alone"
---

# Disclose a skill's directory path so the model can reach its supporting files

## Goal
Multi-file skills (a `SKILL.md` plus `references/*.md`, scripts, templates) currently degrade to
their `SKILL.md` body: sibling files are neither loaded nor mentioned, so the model has no anchor to
`read` them. Carry the already-captured `Skill.source` (`crates/flux-skill/src/lib.rs`) through to
the injected `<skill>` block as a path attribute so the model can lazily read supporting files —
no eager loading, no token cost until used.

## Acceptance
- [x] The `<skill name=…>` injection (`crates/flux-flow/src/engine.rs`) gains the skill's source
      directory (for `SKILL.md` skills) or file path (flat skills), e.g. `path="…"`; failing-first
      test asserts the attribute for both layouts.
- [x] Path disclosed is the real resolved location (project-jailed dirs disclose the in-workspace
      path); no disclosure for skills whose source is unavailable.
- [x] An end-to-end test: a skill whose body says "see references/extra.md" + a turn that reads it
      via the normal `read` op succeeds under default policy for project-local skills.
- [x] `flux skill … --install` output (which already writes `references/`) round-trips: generated
      skills' references are reachable this way; assert in the generator test.
- [x] Docs: claude-compat page + `skills-and-roles.md` explain supporting-file resolution (lazy,
      via read — not eager injection).

## Progress
- Implemented D-190 end to end in `crates/flux-flow/src/engine.rs`:
  - Added `skill_disclosed_path(&flux_skill::Skill) -> Option<PathBuf>`: for a `source` file named
    `SKILL.md` it returns the parent directory; for any other `source` it returns the file itself;
    `None` when `source` is absent (in-memory/SDK-constructed skills).
  - `FlowEngine::base_system_with_skills` now renders `<skill name="…" path="…">` when a path is
    disclosed, and plain `<skill name="…">` (no attribute) otherwise — no other change to the
    injected block or to `Skill.source` plumbing, which already survived discovery
    (`flux_runtime::metadata::parse_skill` → `flux_skill::parse(text, Some(path))`) into
    `AgentSpec.skills` → `FlowEngine.skills` untouched.
  - Disclosure is display-only: it does not touch permissions, policy, or the executor. A turn that
    reads a disclosed path still goes through the normal `read` op → `Executor::dispatch` →
    `PermissionManager`/approval flow. For project-local skills this succeeds under the CLI's actual
    default policy (`read` is in `flux-cli::execution::DEFAULT_ALLOW`) — no new grant was added for
    this story. The open question in Notes below is resolved: for `~`-trusted user-global skill
    dirs outside the workspace jail, a disclosed path's reads simply follow whatever the standard
    policy/approval outcome already is for that location — no widening, no special-casing.
  - Tests added in `crates/flux-flow/src/engine.rs` (`mod tests`), matching the file's existing
    `ScriptedProvider`/`assemble_test_engine` style:
    - `skill_tag_discloses_directory_for_skill_md_and_file_for_flat_skills` — unit test on
      `skill_disclosed_path` for the three cases (SKILL.md dir skill, flat `.md` skill, no source).
    - `injected_skill_tag_carries_the_disclosed_path_attribute` — exercises
      `base_system_with_skills` directly and asserts the rendered `<skill>` tags (`path=` present /
      absent).
    - `turn_reads_a_skills_supporting_file_via_the_disclosed_path` — full adaptive-turn e2e: writes
      a real `SKILL.md` + `references/extra.md` on disk, drives a scripted turn
      (`declare_intent` → `read` → prose) through the real `Executor`/`PermissionManager`/`ReadTool`
      stack with `PermissionManager::from_rules(&["read".into()], &[])` (mirroring the CLI's
      `DEFAULT_ALLOW`, not a story-specific grant), and asserts the `read` op actually dispatched
      and that the disclosed directory path is present in the system-prompt segments sent to the
      model on the next call.
  - Generator round-trip test added in `crates/flux-cli/src/plugin_cmd.rs`
    (`installed_plugin_skill_references_are_reachable_from_its_discovered_source`): renders a
    `flux-plugin` skill with a real plugin manifest (so `references/` is non-empty), writes it via
    `write_generated_skill` into a project `.flux/skills` root, discovers it back through
    `flux_runtime::metadata::discover_skills` (the same production path the engine uses), and
    confirms the discovered skill's `source` is the installed `SKILL.md` and that
    `<source's parent>/references/<name>.md` on disk matches what was generated — i.e. the D-190
    disclosure rule actually anchors a `read` of the generated reference.
  - Docs: rewrote the stale "Only the skill body is used" paragraph in
    `website/docs/agent/claude-compat.md` ("Where the semantics deliberately differ" section) to
    describe the `path=` disclosure + lazy `read` behavior instead of the old "not implemented"
    framing. Added a short paragraph on supporting-file resolution in
    `website/docs/agent/skills-and-roles.md` (with a link back to claude-compat.md).
  - `crates/flux-runtime/src/metadata.rs` was **not** modified — `parse_skill` already threads
    `source` through `flux_skill::parse`, and `discover_skills_from`/`extend_skills` already keep it
    on every `Skill` value returned to callers; the plumbing hint in the story turned out to already
    be satisfied by production code.
  - Concurrency note: this worktree was shared live with another agent landing D-189
    (`flux-skill::Skill` gained `allowed_ops`/`model`/`disable_model_invocation`/`argument_hint`
    fields, and `claude-compat.md`/`skills-and-roles.md` were being rewritten concurrently). Kept
    edits surgical to the `<skill>` injection, its tests, and the two doc paragraphs named in this
    story; verified green after each concurrent landing rather than fighting the race.

Verification (targeted, `CARGO_TARGET_DIR=/home/timo/projects/flux/target`):
- `cargo test -p codewandler-flux-flow -p codewandler-flux-runtime -p flux-cli` — all green
  (flux-flow 182→185 tests incl. the 3 new ones; flux-runtime 107; flux-cli's lib/bin tests incl.
  the new `plugin_cmd::tests` test; `flux-cli --test website_contract` 13/13).
- `cargo clippy -p codewandler-flux-flow -p codewandler-flux-runtime -p flux-cli --all-targets -- -D
  warnings` — clean.
- `cargo fmt -p codewandler-flux-flow -p flux-cli -- --check` — clean (fmt applied once during
  development).
- No full-workspace gate run (out of scope per the task instructions for this shared worktree).

## Notes
- Read-op policy for `~`-trusted skill dirs (outside the workspace jail) is the open question:
  decide whether disclosure implies a read grant or the standard approval flow applies (standard
  flow is the working assumption — no silent grant widening).

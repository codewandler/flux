---
id: C-238
title: "The git op family cannot create a branch, merge it, or revert a merge — the serial-integration half of the fleet loop has no verbs"
pillar: Core
status: in-progress
priority:
epic: fleet-loop
design:
note: "Milestone 2 / F3 of the fleet-loop plan: git_branch + git_merge + git_revert all land here. The name collision is resolved by renaming the eval pack's `git_revert` (which does `git reset --hard`) to `git_reset` — a BREAKING op-catalog change, clean cutover with no alias"
---

# The git op family cannot create a branch, merge it, or revert a merge — the serial-integration half of the fleet loop has no verbs

## Goal
The track/impl-coord loop's serial-integration half needs merge verbs. A full `git_*` family exists
EXCEPT `branch` / `merge` / `revert` — so a Program can stage, commit, diff and enter/leave a
worktree, but cannot create a branch, merge it, or revert a merge. Add the three, mirroring the
existing family's risk/access/intent declarations and concrete `permission_subjects`.

## Acceptance
- [x] A Program can create a branch, merge it with `--no-ff`, assert the result, then revert the
      merge. **Failing-first test**: drive all three from a real `.flux` journey or the equivalent
      op-call harness; prove the ops are absent at the merge base.
      → branch + merge: `tests::git_ops_branch_create_merge_no_ff_journey`; the full
      branch → merge `--no-ff` → revert journey (all three ops off the live registry):
      `tests::git_revert_mainline_one_restores_the_pre_merge_tree`. Both fail at merge base
      `cedef3f4` on op absence (`op `git_revert` is not registered`).
- [x] `git_merge` on a conflict is a clean recoverable error naming the conflicting files, and the
      tree is left consistent (not silently half-merged).
      → `tests::git_merge_conflict_is_recoverable_and_names_the_files`
- [x] `git_revert -m 1` reverts a merge commit and the pre-merge tree is restored (verify with a
      tree diff). → `tests::git_revert_mainline_one_restores_the_pre_merge_tree` compares
      `rev-parse HEAD^{tree}` before the merge and after the revert, and additionally pins
      `HEAD~1 == <merge sha>` so the revert is proved to be an appended commit, not a reset.
      Conflict contract: `tests::git_revert_conflict_is_recoverable_and_names_the_files`.
- [x] Concrete `permission_subjects` on all three, consistent with the git family.
      → `git_branch:impl/x`, `git_merge:impl/x`, `git_revert:<commit>`; each pinned in its test,
      each falling back to the bare op name rather than an empty vec on malformed params.
- [x] Both op references list all three; the catalog-coherence and website-contract tests stay green.
      → `crates/flux-flow/docs/ops-reference.md:69-71`, `website/docs/language/ops.md:89-91`;
      `operations_reference_covers_the_registered_public_catalog`,
      `the_published_risk_column_matches_the_registry` and
      `every_registered_builtin_spec_is_metadata_coherent` all green.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — implementor (worktree `flux-impl-C-238`, branch `flux-impl-C-238`): `git_branch` +
  `git_merge` implemented with failing-first tests (registry-driven journey, compiles against the
  merge base and fails there on the ops' absence). Full gate green: build, 3083 tests across 153
  suites, clippy `-D warnings`, fmt (root + plugins), flux-codegate, check-crate-versions.
  **`git_revert` is BLOCKED**: `crates/flux-eval/src/git.rs` already registers a public op named
  `git_revert` with *different* semantics (`git reset --hard <snapshot>` + `git clean -fd`,
  `Risk::Destructive`, used by `examples/improve-*.flux`). `execution.rs` registers both packs into
  one registry and a duplicate name is a hard startup error, so the story's name cannot be taken
  without either a registration collision or an unsanctioned breaking rename of the eval op.
  Options reported to the coordinator: (a) rename the eval op to `git_reset` (honest — it runs
  `git reset --hard`), freeing `git_revert` for the true revert semantics (breaking: shipped example
  flows + website docs name it); (b) ship the new op under a different name (e.g.
  `git_revert_commit`) — the catalog then carries two similarly-named ops with opposite history
  semantics forever.
- 2026-07-30 — resuming implementor (same worktree/branch): **UNBLOCKED via option (a)** per the
  coordinator's decision. The eval pack's `git_revert` is renamed to `git_reset` — it runs
  `git reset --hard`, so the old name misdescribed it — freeing `git_revert` for true revert
  semantics. Clean cutover: no alias, no deprecation shim. Call sites and docs updated in the same
  commit (`examples/improve-{synthetic,tbench,multi}.flux`, `website/docs/language/{ops,examples}.md`,
  `website/docs/agent/improvement.md`, `docs/self-improvement/DESIGN.md`,
  `crates/flux-lang/docs/syntax.md` + its `flux-markdown` corpus mirror). A whole-tree grep confirms
  every surviving `git_revert` names the NEW op.
  The new `flux-tools` op appends the inverse commit (`git revert --no-edit`, `-m N` for a merge),
  requires a clean tree up front, and on conflict aborts the sequencer and returns a recoverable
  `ToolResult` error naming the unmerged paths — it never resets and never rewrites history.
  Failing-first proof for the revert leg was taken against a pristine `git archive` of merge base
  `cedef3f4`: both new tests fail there with ``op `git_revert` is not registered``.
  Gate green in this worktree: build; `cargo test --workspace` exit 0, **3085 passed / 153 suites**
  (3083 + the 2 new tests), 0 failed; `clippy --workspace --all-targets -D warnings` clean;
  `cargo fmt --all` clean (and `--check` clean in the nested `plugins/` workspace, which is
  untouched); `cargo test -p flux-codegate` 13 passed.
  **For the version decision:** `codewandler-flux-tools`' public API gains `GitRevertTool`
  (additive); `flux-eval`'s `GitRevertTool` is renamed to `GitResetTool` (breaking, but `flux-eval`
  is not a `codewandler-*` crate and is not published). The **op catalog** change is breaking and
  user-visible — an authored flow calling `git_revert($snapshot)` must become `git_reset($snapshot)`
  — so by the pre-1.0 rule this is a **MINOR** bump. `scripts/check-crate-versions.sh` reports
  `PASS 0 changed crate(s)`: it only guards independently-versioned protocol-line crates, and no
  protocol-line crate was touched, so it has nothing to say about this change either way.

## Notes
- Seam: `crates/flux-tools/src/lib.rs` (the `git_*` family + `register_builtins` + the
  `builtins_register` expected-names test), `crates/flux-tools/src/groups.rs` (the `git` group),
  `crates/flux-flow/docs/ops-reference.md`, `website/docs/language/ops.md`.
- The collision (resolved — the eval op is now `GitResetTool` / `git_reset`):
  `crates/flux-eval/src/git.rs:162-223`, registered by
  `flux_eval::try_register_eval_ops` (`crates/flux-eval/src/lib.rs:76`), wired into production at
  `crates/flux-cli/src/execution.rs:1198`; duplicate-name rejection at
  `crates/flux-runtime/src/lib.rs:1748-1762`. Call sites: `examples/improve-synthetic.flux`,
  `examples/improve-tbench.flux`, `examples/improve-multi.flux` (two each);
  `website/docs/language/ops.md:379`, `website/docs/agent/improvement.md`.
- The two ops are semantically distinct and both needed: the eval loop's is "abandon this round"
  (destructive reset to a snapshot); the integration loop's is "revert on red, never reset, never
  rewrite history" (append an inverse commit, `-m 1` for merges). Merging them into one op is wrong.

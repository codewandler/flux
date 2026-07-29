---
id: C-238
title: "The git op family cannot create a branch, merge it, or revert a merge — the serial-integration half of the fleet loop has no verbs"
pillar: Core
status: in-progress
priority:
epic: fleet-loop
design:
note: "Milestone 2 / F3 of the fleet-loop plan: git_branch + git_merge land here; git_revert is BLOCKED on a name collision — the eval pack already registers a `git_revert` that does `git reset --hard` (different semantics), and two public ops cannot share a name"
---

# The git op family cannot create a branch, merge it, or revert a merge — the serial-integration half of the fleet loop has no verbs

## Goal
The track/impl-coord loop's serial-integration half needs merge verbs. A full `git_*` family exists
EXCEPT `branch` / `merge` / `revert` — so a Program can stage, commit, diff and enter/leave a
worktree, but cannot create a branch, merge it, or revert a merge. Add the three, mirroring the
existing family's risk/access/intent declarations and concrete `permission_subjects`.

## Acceptance
- [ ] A Program can create a branch, merge it with `--no-ff`, assert the result, then revert the
      merge. **Failing-first test**: drive all three from a real `.flux` journey or the equivalent
      op-call harness; prove the ops are absent at the merge base.
      *(branch + merge halves done — `tests::git_ops_branch_create_merge_no_ff_journey`; the revert
      leg is BLOCKED, see Progress.)*
- [x] `git_merge` on a conflict is a clean recoverable error naming the conflicting files, and the
      tree is left consistent (not silently half-merged).
      → `tests::git_merge_conflict_is_recoverable_and_names_the_files`
- [ ] `git_revert -m 1` reverts a merge commit and the pre-merge tree is restored (verify with a
      tree diff). **BLOCKED** — see Progress.
- [ ] Concrete `permission_subjects` on all three, consistent with the git family.
      *(done for `git_branch` / `git_merge` — `git_branch:impl/x`, `git_merge:impl/x`, pinned in the
      journey test; `git_revert` pending the naming decision.)*
- [ ] Both op references list all three; the catalog-coherence and website-contract tests stay green.
      *(both references list the two shipped ops; `operations_reference_covers_the_registered_public_catalog`
      and `the_published_risk_column_matches_the_registry` are green; `git_revert` pending.)*
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

## Notes
- Seam: `crates/flux-tools/src/lib.rs` (the `git_*` family + `register_builtins` + the
  `builtins_register` expected-names test), `crates/flux-tools/src/groups.rs` (the `git` group),
  `crates/flux-flow/docs/ops-reference.md`, `website/docs/language/ops.md`.
- The collision: `crates/flux-eval/src/git.rs:162-223` (`GitRevertTool`), registered by
  `flux_eval::try_register_eval_ops` (`crates/flux-eval/src/lib.rs:76`), wired into production at
  `crates/flux-cli/src/execution.rs:1198`; duplicate-name rejection at
  `crates/flux-runtime/src/lib.rs:1748-1762`. Call sites: `examples/improve-synthetic.flux`,
  `examples/improve-tbench.flux`, `examples/improve-multi.flux` (two each);
  `website/docs/language/ops.md:379`, `website/docs/agent/improvement.md`.
- The two ops are semantically distinct and both needed: the eval loop's is "abandon this round"
  (destructive reset to a snapshot); the integration loop's is "revert on red, never reset, never
  rewrite history" (append an inverse commit, `-m 1` for merges). Merging them into one op is wrong.

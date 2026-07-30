---
id: C-241
title: "`fleet.isolate` — a per-item isolated checkout, because `git_worktree_enter` cannot give N workers their own"
pillar: Core
status: ready
priority: 3
epic: fleet-loop
design: docs/designs/fleet-loop.md
areas: [flux-tools, flux-orchestrate]
note: "F4 — the model-owns-nothing move: the host creates impl/<id> in its own worktree and hands back {worktree, branch}; the caller's root is never rebased"
---

# `fleet.isolate` — a per-item isolated checkout, because `git_worktree_enter` cannot give N workers their own

## Goal
Parallel workers need parallel checkouts, and the existing op cannot provide them:
`git_worktree_enter` rebases the **caller's** root and forbids nesting
(`crates/flux-tools/src/lib.rs:3147-3157`). It is session-local by construction, so N workers in one
wave would fight over one root.

Add `fleet.isolate`: given a board item, create branch `impl/<id>` in its own worktree **on the
coordinator's machine** and return `{worktree, branch}` as an artifact. This is the
model-owns-nothing move — the host creates and names the isolation, so a worker cannot claim
isolation it does not have.

## Acceptance
- [ ] **Failing-first test**: two concurrent `fleet.isolate` calls produce two disjoint worktrees and
      the caller's own root is untouched — the thing `git_worktree_enter` provably cannot do. Prove
      the op is absent at the merge base.
- [ ] Spawning goes through `flux-system`'s guarded spawn (argv-only, workspace-pinned, no second
      `Command::new`), like the rest of the git family.
- [ ] Explicit preflights, with `git_worktree_enter`'s existing checks as the template: no nesting,
      and a clean base. Each refuses as a clean recoverable `ToolResult` error naming what was wrong,
      not a plan-halting raw error.
- [ ] Concrete `permission_subjects` (the worktree path and the branch), accurate
      `effects`/`access`/`intents` — consistent with the `git_*` family.
- [ ] Both op references list the op; the catalog-coherence and website-contract tests stay green.
- [ ] Standard gate green in both workspaces.

## Notes
- **Scope boundary, from the design's correction:** `fleet.isolate` isolates a **local** worker only.
  A remote A2A worker cannot receive a worktree path and the coordinator cannot verify it honoured
  one; `fleet-coordinator.md:303-311` declares that problem dissolved on the grounds the remote
  worker owns its own workspace. Real per-worker isolation for remote workers is A-124 (Docker).
  In-process children already get isolation via C-100 (`SpawnRequest.system` → a fresh
  `WorkspaceContext`, `crates/flux-orchestrate/src/lib.rs:342-352`).
- Do not reuse `git_worktree_enter` by relaxing its nesting check — its caller-local rebase is the
  behaviour a session wants, and weakening it would break that contract to serve a different one.
- Cleanup is the caller's: a worktree holding an unmerged diff must never be removed by the host.

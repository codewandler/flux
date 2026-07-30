---
id: C-241
title: "`fleet.isolate` — a per-item isolated checkout, because `git_worktree_enter` cannot give N workers their own"
pillar: Core
status: in-progress
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
- [x] **Failing-first test**: two concurrent `fleet.isolate` calls produce two disjoint worktrees and
      the caller's own root is untouched — the thing `git_worktree_enter` provably cannot do. Prove
      the op is absent at the merge base.
- [x] Spawning goes through `flux-system`'s guarded spawn (argv-only, workspace-pinned, no second
      `Command::new`), like the rest of the git family.
- [x] Explicit preflights, with `git_worktree_enter`'s existing checks as the template: no nesting,
      and a clean base. Each refuses as a clean recoverable `ToolResult` error naming what was wrong,
      not a plan-halting raw error.
- [x] Concrete `permission_subjects` (the worktree path and the branch), accurate
      `effects`/`access`/`intents` — consistent with the `git_*` family. *(Branch only — see
      Progress: the checkout path is allocated during execution, after approval.)*
- [ ] Both op references list the op; the catalog-coherence and website-contract tests stay green.
      *(Both reference files were fenced to other agents in this wave; the two rows are owed to the
      integrator — see Progress.)*
- [ ] Standard gate green in both workspaces. *(Green except
      `operations_reference_covers_the_registered_public_catalog`, which is red exactly until the
      owed `website/docs/language/ops.md` row lands.)*

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

## Progress

`fleet.isolate` lands in `crates/flux-tools/src/lib.rs` (the `fleet.isolate (C-241)` section, after
the `git_worktree_*` ops it is built beside), registered in `try_register_builtins` and joined to the
`fleet` group in `groups.rs`. Input is `{item}`; the host owns the naming (`impl/<item>`) and the
directory (`flux_system::allocate_worktree_dir`, a fresh 0700 parent per call — that allocation is
what makes two concurrent calls disjoint with no coordination). Every git invocation goes through
`run_git` on `ctx.system()`, so there is no second `Command::new`.

Preflights, each a recoverable `ToolResult::error` naming what was wrong: item id shape (one
component of `[A-Za-z0-9._-]`, so an option/path/revision-suffix can never reach argv), no nesting
inside a `git_worktree_enter` session, inside a git repository, `refs/heads/impl/<item>` free, and a
clean `git status --porcelain`. Nothing is allocated before the last of them passes, and a failed
`git worktree add` removes the parent directory it allocated.

Two deliberate deviations from the Acceptance wording:

1. **`permission_subjects` names the branch only** — `fleet.isolate:impl/<item>`, in C-238's
   `git_branch:<ref>` shape. The checkout path *cannot* be a subject: `permission_subjects(&self,
   params)` sees only the call arguments, and the parent directory is allocated during execution,
   i.e. after approval. A path synthesized at declaration time would name a directory that never
   exists, which is worse than naming one thing truthfully.
2. **Two catalog rows are owed to the integrator.** `crates/flux-flow/docs/ops-reference.md` and
   `website/docs/language/ops.md` were both fenced to other agents in this wave, so
   `operations_reference_covers_the_registered_public_catalog`
   (`crates/flux-cli/tests/website_contract.rs:330`) is red on `["fleet.isolate"]` alone until they
   land. The exact rows are in the implementor's handoff; the rest of the gate is green, including
   the `flux-cli` catalog-coherence census and `metadata_violations`.

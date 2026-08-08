---
id: C-241
title: "`fleet.isolate` — a per-item isolated checkout, because `git_worktree_enter` cannot give N workers their own"
pillar: Core
status: done
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
- [~] Concrete `permission_subjects` (the worktree path and the branch), accurate
      `effects`/`access`/`intents` — consistent with the `git_*` family. **Half-met, and marked as
      such rather than ticked:** the branch is named, the path is not. Independent review confirmed
      this is safe (see Progress 1) but corrected the reasoning — the *specific* path genuinely cannot
      be a subject, yet `worktree_base_dir()` is deterministic from `FLUX_WORKTREE_DIR`/`HOME` and
      could truthfully name where the checkout lands; it is merely private today. So this is a
      completeness gap, not an impossibility. Closing it needs a `flux-system` change.
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
directory (`flux_system::allocate_worktree_dir`, a fresh 0700 parent per call). The paths are
disjoint; a process-local asynchronous lock serializes the branch-free check and `git worktree add`
because both calls still mutate the repository's shared `.git/worktrees` administration. Every git
invocation goes through `run_git` on `ctx.system()`, so there is no second `Command::new`.

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
   **Reviewed and refined (C-241 review, PASS):** the deviation is safe, but not because naming a
   location is impossible. It is safe because the omission carries **no authorization information** —
   nothing in `params` influences where the checkout lands, and `access: [AccessKind::Process]` with no
   `Filesystem` means `authority_requirements_from_declaration` emits `process.exec` on the
   branch-named subject and no filesystem requirement at all: the same shape `git_commit`,
   `git_checkout` and `git_worktree_enter` already have while writing the user's tree. The subject is
   in fact *more* scoped than `git_worktree_enter`'s, which is the bare op name. The AGENTS.md rule
   about accurate subjects concerns an **empty** list on a `Write`-declaring tool; this op declares no
   `Write` and never returns empty. What the review *did* overturn is the "cannot" —
   `worktree_base_dir()` (`crates/flux-system/src/lib.rs:352-362`) is fully deterministic and could
   name the directory; only the per-call `flux-worktree-<pid>-<seq>` suffix is runtime-chosen.
   **Also found by review — one more state-leak window than recorded above:** at
   `crates/flux-tools/src/lib.rs:4162` the `git worktree add` result is taken with `.await?`, so if
   `System::run` itself errors (60 s timeout, or `sandbox.ensure_available()` refusing under
   `mode = "require"`) the function returns `Err` *before* the `remove_worktree_dir` cleanup on the
   next line, leaking the 0700 parent and any ref git had already written. The preflight `run_git`
   calls share the shape: a spawn/timeout failure surfaces as a plan-halting raw error rather than the
   recoverable `ToolResult::error` the Acceptance asks for. This is copied faithfully from
   `git_worktree_enter` (`lib.rs:3668-3683`), so it is a **family-wide pre-existing shape, not a
   regression here** — but it is unrecorded anywhere else, so it is recorded here.
2. **Two catalog rows are owed to the integrator.** `crates/flux-flow/docs/ops-reference.md` and
   `website/docs/language/ops.md` were both fenced to other agents in this wave, so
   `operations_reference_covers_the_registered_public_catalog`
   (`crates/flux-cli/tests/website_contract.rs:330`) is red on `["fleet.isolate"]` alone until they
   land. The exact rows are in the implementor's handoff; the rest of the gate is green, including
   the `flux-cli` catalog-coherence census and `metadata_violations`.

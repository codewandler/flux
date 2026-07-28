---
id: C-97
title: "WorkspaceContext — a context-local, swappable active System"
pillar: Core
status: done
epic: context-local-git-worktrees
design: docs/designs/context-local-git-worktrees.md
note: "runtime seam for the worktree ops: per-agent active-System accessor replaces direct ctx.system; no set_current_dir anywhere"
---

# WorkspaceContext — a context-local, swappable active System

## Goal
Give each agent context its own swappable workspace root so a worktree transition affects only that
context: a `WorkspaceContext` on `ToolContext` holding a snapshot-able active `Arc<System>` plus
optional worktree session state, with every tool going through an active-system accessor instead of
a fixed `ctx.system`. Foundation story for the context-local-git-worktrees epic.

## Acceptance
- [ ] `WorkspaceContext` owned by `ToolContext`: snapshot-able active `Arc<System>`; optional
      worktree session state (original `System`, captured `main` commit, generated branch, worktree
      path, cleanup phase); entering while a session is active is a recoverable error (no nesting).
- [ ] Direct `ctx.system` use replaced by the accessor across filesystem, process, plugin, flow, and
      toolchain operations; a tool already running keeps its initial snapshot, later calls see the
      transitioned root — proven by a failing-first test with two contexts sharing one repository
      where transitioning one changes neither the other context nor process-wide PWD.
- [ ] Guarded `flux-system` helpers: derive a `System` rooted at an existing worktree preserving
      the source sandbox **and the full `Workspace` posture — named roots (`@global_flows`), read
      roots, unconfined flag** (today's constructors lose one or the other); create/clean a private
      `/tmp/flux-worktree-*` parent directory through guarded IO only.
- [ ] Scope stated in docs: one `WorkspaceContext` per `ToolContext` per engine — on the server an
      engine (and thus a worktree session) is shared by that agent's conversations; plugin
      subprocesses (PluginHost/SystemHostCaps hold their own spawn-time `System`) keep the original
      root in v1 (documented limitation).
- [ ] No `std::env::set_current_dir` anywhere in the new paths; full gate green.

## Progress
- 2026-07-28 implemented on the epic worktree branch: `Workspace::with_root` + `System::rerooted`
  (posture-preserving derives) and `allocate_worktree_dir`/`remove_worktree_dir` (fail-closed) in
  flux-system; `WorkspaceContext`/`WorktreeSession`/`WorktreePhase` in flux-runtime with
  `ToolContext::system()` accessor replacing the pub field (~64-site sweep across 8 crates).
  Tests: `worktree_transition_is_context_local`, `worktree_session_phase_marks_merged`
  (flux-runtime), `with_root_preserves_access_posture`, `rerooted_system_keeps_sandbox_and_moves_root`,
  `worktree_dir_alloc_and_guarded_removal` (flux-system). Full workspace suite green.

## Notes
- Design: [docs/designs/context-local-git-worktrees.md](../designs/context-local-git-worktrees.md)
  — distilled from a decision-complete codex Plan Mode session (2026-07-28).
- Blocks C-98/C-99/C-100.

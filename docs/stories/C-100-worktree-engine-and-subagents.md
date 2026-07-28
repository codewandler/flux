---
id: C-100
title: "Worktree-aware engine probing, sub-agent inheritance, and epic docs"
pillar: Core
status: done
epic: context-local-git-worktrees
design: docs/designs/context-local-git-worktrees.md
note: "FlowEngine probes the active root per turn; children get an independent snapshot; ops-reference/CHANGELOG/WHATS-NEW close the epic"
---

# Worktree-aware engine probing, sub-agent inheritance, and epic docs

## Goal
Make the rest of the runtime honest about the transitioned root: `FlowEngine` tool-group discovery
follows the context's active root each turn, spawned sub-agents inherit a snapshot without sharing
the session, and the epic's user-facing docs land.

## Acceptance
- [ ] `FlowEngine` probes the context's active root each turn for tool-group/evidence surfacing
      instead of its assembly-time `cwd` — a signal present only in the worktree (e.g. a
      worktree-local `Cargo.toml`) surfaces its group after `enter`, failing-first. Agent
      configuration, role, permissions, and loaded skills remain fixed for the session.
- [ ] `SpawnRequest`/local spawner give a child a copy of the parent's active-system snapshot but an
      independent `WorkspaceContext`; a child's enter/leave never changes its parent — inheritance
      and isolation both tested. This also fixes the latent child-`cwd` bug (child `AgentSpec.cwd`
      defaults to `"."`, probing the process cwd); nested delegation (`at_depth`) carries the
      child's own snapshot, not the grandparent's.
- [ ] The `enter`/`leave` op results state the new/restored root prominently (the assembly-time
      system-prompt project context goes stale after a transition; the `cwd` op is the model's live
      ground truth). Sticky surfacing of worktree-local groups after `leave` is documented.
- [ ] Both ops documented in `crates/flux-flow/docs/ops-reference.md`; CHANGELOG and WHATS-NEW
      entries (regen the website whats-new mirror in the same commit); full gate green across the
      epic.

## Progress
- 2026-07-28 implemented on the epic worktree branch: `surfaced_for_turn` probes the active
  context root (assembly-time `cwd` retained for config/skills/roles by design);
  `SpawnRequest.system` snapshot field; `LocalSpawner` seeds the child from the snapshot, fixes the
  child `spec.cwd == "."` latent bug, and `at_depth` re-bases nested spawners on the child
  snapshot. Tests: `per_turn_surfacing_probe_follows_the_transitioned_root` (flux-flow),
  `spawned_child_inherits_transitioned_root_but_transitions_independently`,
  `nested_spawner_rebases_on_the_child_snapshot` (flux-orchestrate). Gate green.
- Remaining for close: ops-reference/CHANGELOG/WHATS-NEW docs land with the epic close-out.

## Notes
- Depends on C-97..C-99; closes the context-local-git-worktrees epic.
- Design: [docs/designs/context-local-git-worktrees.md](../designs/context-local-git-worktrees.md).

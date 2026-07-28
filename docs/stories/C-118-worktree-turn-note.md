---
id: C-118
title: "Tell the model per turn when its context is inside a worktree"
pillar: Core
status: done
epic: context-local-git-worktrees
design: docs/designs/context-local-git-worktrees.md
note: "per-turn <workspace-note> in the base system while a worktree session is active; assembly-time project context stays untouched"
---

# Tell the model per turn when its context is inside a worktree

## Goal
The assembly-time project context (cwd, git branch/status) goes stale after `git_worktree_enter` —
the model is told the original root while its operations land in the worktree. Inject a per-turn
`<workspace-note>` into the base system while a worktree session is active, naming the transitioned
root and branch and pointing at the `cwd` op as live truth.

## Acceptance
- [x] `base_system_with_skills` appends the note iff `worktree_session()` is active; it names the
      checkout path and generated branch, and disappears after leave — proven by
      `base_system_carries_a_worktree_note_only_while_transitioned` (flux-flow).
- [x] Assembly-time config/skills/roles/project-context loading is untouched.

## Progress
- 2026-07-28 implemented at the per-turn seam in `FlowEngine::base_system_with_skills`.

## Notes
- Complements the op results (which already state the new root); this covers later turns in the
  same session.

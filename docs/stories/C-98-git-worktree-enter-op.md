---
id: C-98
title: "git_worktree_enter — guarded op that moves this context into a temp worktree"
pillar: Core
status: backlog
epic: context-local-git-worktrees
design: docs/designs/context-local-git-worktrees.md
note: "clean main → generated flux/worktree/* branch → /tmp worktree → context root transition; argv-only git"
---

# git_worktree_enter — guarded op that moves this context into a temp worktree

## Goal
A built-in `git_worktree_enter {}` that creates an isolated temporary Git worktree and transitions
only the calling agent context's root into it, so agents can mutate a repo without stepping on the
original checkout.

## Acceptance
- [ ] Preflight: requires a Git repository, a clean non-detached checkout on `main`, and no active
      worktree session; each rejection covered by a failing-first test.
- [ ] Captures `main`'s `HEAD`, generates a collision-resistant `flux/worktree/...` branch, runs
      `git worktree add -b <branch> <tmp>/checkout <captured-head>` argv-only through `System`.
- [ ] Transitions the context to the equivalent relative directory in the new checkout (worktree
      root as fallback); returns branch and worktree path; the directory is writable and outside the
      original PWD; subsequent writes/process calls land in the worktree.
- [ ] Registered in the Git tool group as a high-risk, non-idempotent guarded op with non-empty
      permission subjects and process/local-system effects; registry/group/catalog tests updated.

## Progress
- (not started)

## Notes
- Depends on C-97 (WorkspaceContext seam).
- Design: [docs/designs/context-local-git-worktrees.md](../designs/context-local-git-worktrees.md).

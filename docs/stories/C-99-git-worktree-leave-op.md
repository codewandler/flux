---
id: C-99
title: "git_worktree_leave — merge back to main, restore context, clean up"
pillar: Core
status: backlog
epic: context-local-git-worktrees
design: docs/designs/context-local-git-worktrees.md
note: "trial merge then --no-ff; conflict never strands main; cleanup-pending state is retryable without re-merging"
---

# git_worktree_leave — merge back to main, restore context, clean up

## Goal
A built-in `git_worktree_leave {}` that integrates the worktree's committed work into the original
`main`, restores the calling context's original root, and removes the temporary worktree and its
generated branch — with failure modes that never lose work or strand `main` in a conflicted state.

## Acceptance
- [ ] Requires a clean (committed) worktree — never stages or commits automatically; verifies
      original `main` is still clean, checked out, and at the `enter` commit, else leaves the
      context untouched.
- [ ] No-commit merge trial aborted on conflict proves the real merge cannot leave `main`
      conflicted; then `git merge --no-ff --no-edit <generated-branch>`, worktree removal, branch
      deletion, and context restore only after successful cleanup — full round trip
      (enter → edit/commit → leave → merge on `main`) covered failing-first in a temp repository.
- [ ] Merge failure keeps the agent in its worktree with the original checkout clean; partial
      cleanup records a recoverable cleanup-pending state with precise diagnostics, and retrying
      `leave` completes cleanup without re-merging — each recovery state tested.
- [ ] Registered beside `git_worktree_enter` (Git group, high-risk, non-idempotent, explicit
      subjects/effects); no force, reset, discard, or shell invocation anywhere.

## Progress
- (not started)

## Notes
- Depends on C-97 and C-98.
- Design: [docs/designs/context-local-git-worktrees.md](../designs/context-local-git-worktrees.md).

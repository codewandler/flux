---
id: C-249
title: "The git family's clean-tree preconditions are per-op accidents, and \"commit or stash them first\" is unactionable for untracked files"
pillar: Core
status: ready
priority: 8
areas: [flux-tools]
note: "surfaced by C-238's review: git_worktree_leave and git_revert each grew their own clean-tree guard for the same reason, git_merge had none, and three ops share advice that a plain `git stash` cannot carry out"
---

# The git family's clean-tree preconditions are per-op accidents, and "commit or stash them first" is unactionable for untracked files

## Goal
Two related inconsistencies in the guarded `git_*` family, both found by the independent review of
C-238 and both deliberately left out of that story's rework so its diff stayed scoped to the
demonstrated defect.

**1. The clean-tree precondition is decided per-op, by whoever wrote the op.**
`git_worktree_leave` refuses unless `git status --porcelain` is empty, precisely so its always-abort
discipline cannot destroy work it did not create (`crates/flux-tools/src/lib.rs:3778-3784`).
`git_revert` independently grew the same guard for the same reason (`:2795-2807`). `git_merge` had
none, which is how C-238's blocking defect existed at all. Three ops, one hazard, three separate
decisions — the next merging or aborting op will make a fourth. Decide the policy once and make it
structural, so an op that can abort or reset **cannot** be written without confronting the
precondition.

**2. `"commit or stash them first"` is advice the caller cannot follow.** The guard triggers on
`git status --porcelain`, whose output includes untracked `??` entries — and a plain `git stash` does
not clear those. So an agent that follows the message retries and fails identically. The wording is
shared by `git_revert`, `git_snapshot` and `git_worktree_enter`.

## Acceptance
- [ ] The clean-tree policy is stated once and enforced structurally rather than restated per op —
      e.g. a shared precondition helper that abort-capable ops must call, with a test that fails if
      an op declaring a destructive/aborting path skips it. A comment convention is **not** enough;
      the point is that the next op cannot silently omit it.
- [ ] **Failing-first test**: an abort-capable `git_*` op without the precondition is rejected by the
      suite. It fails today, because nothing notices that `git_merge` lacked one.
- [ ] The refusal message distinguishes tracked modifications from untracked files and gives advice
      that actually clears the state it names (untracked needs `git clean` or an explicit
      `stash -u`, not a bare `stash`). Reconciled across `git_revert`, `git_snapshot` and
      `git_worktree_enter` so all three say the same true thing.
- [ ] No behavioural weakening: every op that refuses a dirty tree today still refuses it.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — filed from the independent review of C-238. That review confirmed the blocking case
  (a pre-existing `MERGE_HEAD` plus an unconditional `git merge --abort` destroying hand-resolved
  work) and it is fixed in C-238 itself. This story is the *general* policy, which is a different and
  larger change.

## Notes
- Useful negative result from the same review, worth not re-deriving: an attempt to build a
  work-loss case from **ordinary** dirtiness failed. A dirty *index* makes `git merge` refuse to
  start, leaving no `MERGE_HEAD` and the tree untouched; unstaged unrelated edits survived
  `git merge --abort` intact. So the hazard is specifically the **mid-operation** tree
  (`MERGE_HEAD`/`REVERT_HEAD` present), not dirtiness in general. Scope the policy to what is
  actually dangerous rather than refusing every dirty tree reflexively — over-refusing would make the
  family unusable in exactly the multi-author situation C-92's hunk staging exists to serve.
- Related asymmetry, also from that review: `git revert` refuses a dirty **index** but will happily
  commit over unstaged or untracked changes. So flux's in-process check is deliberately *stricter*
  than git's. That is the right direction, but it means the precondition is flux policy rather than
  git behaviour, and it should be documented as such — a comment claiming git enforces it is wrong
  (that specific wrong comment is fixed in C-238).
- Seam: `crates/flux-tools/src/lib.rs`, the `git_*` family plus the shared `run_git` helper.

---
id: C-722
title: "A story worktree holding uncommitted work must be reported, never silently reclaimed"
pillar: "Core"
status: done
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "wave-745 died with an 18KB failing-first test for C-575 uncommitted in its story worktree. handoff --from-worktree derives its write set from base..HEAD so it cannot see the file, doctor reported the branch as holding no commit of its own, and its prescribed fix was reclaim, which is documented to delete worktrees that provably hold no work"
---

# A story worktree holding uncommitted work must be reported, never silently reclaimed

## Goal

A worker that does real work and never commits it is indistinguishable, to every part of the
fleet, from a worker that did nothing. `wave-745` died with a 531-line failing-first specification
for [[C-575]] sitting untracked in its story worktree. `handoff --from-worktree` derives the write
set from `base..HEAD`, so it could not see the file; `doctor` reported the branch as holding no
commit of its own; and the fix `doctor` prescribed was `reclaim`, which is documented to delete
worktrees that provably hold no work. Three independent mechanisms agreed the work did not exist.

Uncommitted is the normal state of a worker that was interrupted. Treating it as absence is what
turns an interrupted turn into lost work.

## Acceptance

- [x] `flux fleet doctor` reports a story worktree holding uncommitted work — tracked
      modifications **or** untracked files not covered by `.gitignore` — naming the worktree path,
      the wave, the story and the number of files.
- [x] That finding is distinct from `branch-without-unique-work`, and takes precedence over it: a
      branch sitting at its pinned base with a dirty worktree reports the dirty finding, never the
      empty one, and never prescribes `reclaim`.
- [x] `flux fleet reclaim` refuses to remove such a worktree and says why. See [[C-720]].
- [ ] A worker turn that ends leaving uncommitted changes in its story worktree is recorded as a
      distinct, queryable outcome rather than a silent success — a turn's recorded status must not
      claim more than its worktree can show. See [[C-721]].
- [x] The fleet offers a supported way to capture that work — committing it on the story's own
      branch — so recovery does not require hand-running `git` in a worktree the fleet owns.
- [x] Regression test: a story worktree at its pinned base whose only content is one untracked
      file is reported as holding work, survives `reclaim`, and is recoverable through the CLI.

> **Acceptance 4 moved to [[C-725]].** The work landed here is complete and gated;
> that criterion is tracked separately rather than left as an open box on a delivered story.

## Progress

Implemented on `impl/C-722`. Five of six acceptance items are satisfied and covered by tests that
run in the workspace gate.

- `fleet doctor` gains `story-worktree-holds-uncommitted-work`, counted from
  `git status --porcelain -uall` so an untracked *directory* does not collapse to one entry.
- It takes precedence over `branch-without-unique-work`: `without_branches_holding_uncommitted_work`
  drops any stale-branch finding whose `subject` names a branch whose worktree is dirty. Both
  findings fire on exactly the `wave-745` state and they prescribe opposite actions, so the
  misleading half is the one that goes.
- `reclaim`'s refusal now reports a file count and the next command instead of "holds uncommitted
  changes or commits absent from the canonical ref".
- `flux fleet capture <wave> [--item BOARD/ITEM]` commits what an interrupted worker left onto the
  story's own branch. It refuses a detached or reassigned HEAD, excludes Fleet's own loop-binding
  snapshot, and re-reads head and worktree before reporting a commit.

The remaining item — recording an interrupted turn as a distinct outcome — is [[C-721]]'s and was
deliberately left to it: both stories edit `board_fleet_cmd.rs` and they ran concurrently.

Note for whoever picks this up: the sibling worktrees share a `CARGO_TARGET_DIR` by default, and
all four hash to the same cargo metadata, so `crates/flux-cli` builds to the same artifact path in
every one. A test run there can report `Finished` against another checkout's binary. Build
correctness-critical runs in a private target directory.

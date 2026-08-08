---
id: C-722
title: "A story worktree holding uncommitted work must be reported, never silently reclaimed"
pillar: "Core"
status: backlog
priority: 1
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

- [ ] `flux fleet doctor` reports a story worktree holding uncommitted work — tracked
      modifications **or** untracked files not covered by `.gitignore` — naming the worktree path,
      the wave, the story and the number of files.
- [ ] That finding is distinct from `branch-without-unique-work`, and takes precedence over it: a
      branch sitting at its pinned base with a dirty worktree reports the dirty finding, never the
      empty one, and never prescribes `reclaim`.
- [ ] `flux fleet reclaim` refuses to remove such a worktree and says why. See [[C-720]].
- [ ] A worker turn that ends leaving uncommitted changes in its story worktree is recorded as a
      distinct, queryable outcome rather than a silent success — a turn's recorded status must not
      claim more than its worktree can show. See [[C-721]].
- [ ] The fleet offers a supported way to capture that work — committing it on the story's own
      branch — so recovery does not require hand-running `git` in a worktree the fleet owns.
- [ ] Regression test: a story worktree at its pinned base whose only content is one untracked
      file is reported as holding work, survives `reclaim`, and is recoverable through the CLI.

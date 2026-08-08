---
id: C-746
title: "branch-without-unique-work prescribes a fix that cannot act on it"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-orchestrate]
---

# branch-without-unique-work prescribes a fix that cannot act on it

## Goal

`fleet doctor` reports `branch-without-unique-work` and prescribes `flux fleet reclaim wave-N`. But
reclaim only removes worktrees, and only for terminal waves — it never deletes a branch that has
moved past its pinned base, and it cannot act at all on a wave that is not terminal. So the finding
is unactionable: an operator who runs the prescribed command sees `reclaimed 0 of 1` and the branch
survives forever.

This is how the repository reached 119 local branches against 40 on the remote, with 61 of them
provably superseded.

## Acceptance

- [ ] `doctor`'s prescription for this finding resolves it. Either reclaim reaps a branch whose every
      commit is already applied to the canonical ref, or the finding stops naming reclaim and names
      the verb that does.
- [ ] Containment is judged by patch equivalence, not ancestry. A wave's commits reach the canonical
      ref by cherry-pick during integration, so an ancestry test reports every integrated branch as
      unmerged — that is why the first sweep of this repository missed 29 reapable branches.
- [ ] A branch holding a commit with no equivalent on the canonical ref is never reaped, and is
      reported as retained with the reason.
- [ ] A branch named by a story or design as durable storage is never reaped regardless. See
      [[C-747]].
- [ ] Regression test: a wave branch whose commits are all patch-applied to the canonical ref is
      reaped by the prescribed command, and one holding a unique commit is retained and named.

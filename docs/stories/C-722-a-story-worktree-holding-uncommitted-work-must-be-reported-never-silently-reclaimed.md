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


## Acceptance

- [ ] Define acceptance.

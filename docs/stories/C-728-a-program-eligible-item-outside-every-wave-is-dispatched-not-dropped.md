---
id: C-728
title: "A program-eligible item outside every wave is dispatched, not dropped"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "A tick with eight free slots and eight program-eligible dependency-satisfied items planned four dispatches and recorded no reason for the other four. Dispatch draws from configured waves, so an item named by no wave never reaches the withhold logic and leaves the schedulable pool with no record at all. C-723 made a withhold state its evidence; this is the path that produces no withhold to state"
---

# A program-eligible item outside every wave is dispatched, not dropped

## Goal


## Acceptance

- [ ] Define acceptance.

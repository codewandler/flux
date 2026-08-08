---
id: C-724
title: "A wave whose supervisor is gone must release its claim on ready items"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "wave-745 kept an exclusive claim on C-575 and C-519 after its supervisor pid 3513527 died. doctor detected agent-supervisor-gone and prescribed cancelling the two workers, but cancelling the workers did not release the wave's claim; only cancelling the wave itself did, taking dispatch from 1 item to 3. Nothing reclaims a claim held by a dead supervisor, so a driver crash removes its items from the schedulable pool permanently"
---

# A wave whose supervisor is gone must release its claim on ready items

## Goal


## Acceptance

- [ ] Define acceptance.

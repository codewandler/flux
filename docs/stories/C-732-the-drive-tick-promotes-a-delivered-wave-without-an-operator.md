---
id: C-732
title: "The drive tick promotes a delivered wave without an operator"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "C-681 built flux fleet promote and it is idempotent, but no drive tick calls it, so landing on a member's local canonical ref is still operator-invoked. With C-730 dispatching the integrator and C-587 gating on review, promote is the last link the tick does not pull. Wiring it closes worker to local main end to end"
---

# The drive tick promotes a delivered wave without an operator

## Goal


## Acceptance

- [ ] Define acceptance.

---
id: C-721
title: "A wave is applied only when the canonical ref contains its commits"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "wave-649 is recorded applied while X-139's two commits exist only on fleet/wave-649/exchange/story/X-139: absent from origin/main, absent from local main, and absent from the wave's own integration branch. apply never pushes, yet exchange and connectors declare canonical_ref = origin/main, so apply cannot reach their canonical ref by construction and still reports success"
---

# A wave is applied only when the canonical ref contains its commits

## Goal


## Acceptance

- [ ] Define acceptance.

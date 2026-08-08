---
id: C-723
title: "The driver must not withhold ready work on an unverified already-built signal"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "A dry-run tick with 8 free slots and 9 ready items dispatched 1. C-570 and C-544 were withheld as already-built on board reconcile's say-so, yet AgentReport and safe_checkpoint do not exist in any crate, the flux-tui skill doc states the C-570 operation is unbuilt, and every C-544 hit in Rust is a doc comment forward-referencing the id. C-718 already establishes reconcile matches a mention, not an implementation; the driver consumes it as a hard block"
---

# The driver must not withhold ready work on an unverified already-built signal

## Goal


## Acceptance

- [ ] Define acceptance.

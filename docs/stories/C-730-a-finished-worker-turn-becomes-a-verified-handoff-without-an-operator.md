---
id: C-730
title: "A finished worker turn becomes a verified handoff without an operator"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "Nothing converts turn_end into a handoff: grep finds zero wiring, fleet_handoff exists only as a CLI verb an operator types, and drive writes only provisional handoffs by reconstruction. The integrator role is worse — the string integrator appears 0 times in flux-cli, so the configured role and wave-integration.flux are dead config the driver never dispatches. Until a turn produces a handoff carrying its own validation evidence and the tick dispatches an integrator, every wave stops at the writers and the rest of the pipeline is an operator typing verbs"
---

# A finished worker turn becomes a verified handoff without an operator

## Goal


## Acceptance

- [ ] Define acceptance.

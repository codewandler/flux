---
id: C-286
title: Add a guarded TCP operation
pillar: Core
status: backlog
epic: network-primitives
design: docs/designs/network-primitives.md
areas: [flux-system, flux-runtime]
note: "Settle connection handles and policy-checked destination reuse before exposing a stream"
---

# Add a guarded TCP operation

## Goal

Expose a bounded TCP contract without turning a connection handle into transferable ambient network
authority.

## Acceptance

- [ ] C-284 accepts addressing, connect/read/write limits, connection lifetime, cancellation, TLS
      interaction, and handle-authority rules.
- [ ] A real registered ToolSpec, guarded implementation, intent/policy mapping, and adversarial tests ship.
- [ ] The core catalogue flips TCP from planned to available and links the real operation in the same change.

## Progress

- Not started.


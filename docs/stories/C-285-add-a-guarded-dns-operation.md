---
id: C-285
title: Add a guarded DNS operation
pillar: Core
status: backlog
epic: network-primitives
design: docs/designs/network-primitives.md
areas: [flux-system, flux-runtime]
note: "Resolution is useful only if the answer consumed later is the answer that passed policy"
---

# Add a guarded DNS operation

## Goal

Make explicit DNS lookup available through a typed, bounded operation whose returned addresses and
subsequent use cannot bypass Flux's rebinding posture.

## Acceptance

- [ ] C-284 has accepted the resolver, record-type, timeout, answer-limit, and rebinding contract.
- [ ] A real registered ToolSpec, guarded implementation, intent/policy mapping, and adversarial tests ship.
- [ ] The core catalogue flips DNS from planned to available and links the real operation in the same change.

## Progress

- Not started.


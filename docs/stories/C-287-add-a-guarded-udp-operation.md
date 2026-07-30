---
id: C-287
title: Add a guarded UDP operation
pillar: Core
status: backlog
epic: network-primitives
design: docs/designs/network-primitives.md
areas: [flux-system, flux-runtime]
note: "Datagrams need explicit destination and reply bounds; they are not a stateless version of TCP"
---

# Add a guarded UDP operation

## Goal

Expose bounded datagram IO while checking every send destination and every accepted reply against the
declared intent and network policy.

## Acceptance

- [ ] C-284 accepts destination, reply-source, payload, datagram-count, timeout, multicast/broadcast,
      and spoofing rules.
- [ ] A real registered ToolSpec, guarded implementation, intent/policy mapping, and adversarial tests ship.
- [ ] The core catalogue flips UDP from planned to available and links the real operation in the same change.

## Progress

- Not started.


---
id: C-284
title: Design guarded network primitives
pillar: Core
status: backlog
epic: network-primitives
design: docs/designs/network-primitives.md
areas: [flux-system, flux-runtime, security]
note: "Protocol families need different authority, addressing, lifecycle, and result contracts; do not flatten them into a generic socket escape hatch"
---

# Design guarded network primitives

## Goal

Define the common security and registry boundary for DNS, TCP, UDP, and ICMP before any of their
planned catalogue records becomes callable.

## Acceptance

- [ ] The design settles capability grants, address validation, DNS rebinding posture, byte limits,
      timeouts, cancellation, observability/redaction, and whether connection state can cross calls.
- [ ] Each protocol child story has an operation shape that does not expose a generic socket or
      bypass `flux-system`'s guarded IO boundary.
- [ ] The core catalogue changes a capability from planned to available only in the same release in
      which its real registered `ToolSpec` and guarded implementation ship.

## Progress

- Placeholder only; no runtime implementation belongs to C-283.

## Notes

- Child stories: C-285 DNS, C-286 TCP, C-287 UDP, C-288 ICMP.


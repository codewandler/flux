---
id: C-288
title: Add a guarded ICMP operation
pillar: Core
status: backlog
epic: network-primitives
design: docs/designs/network-primitives.md
areas: [flux-system, flux-runtime]
note: "ICMP must remain fail-closed on platforms where the host cannot obtain the required privilege"
---

# Add a guarded ICMP operation

## Goal

Provide a portable, bounded reachability operation where the host can do so safely, with an explicit
unsupported result rather than a shell-out or privilege bypass.

## Acceptance

- [ ] C-284 accepts message scope, privilege model, platform support, address-family, timeout,
      response validation, and rate-limit rules.
- [ ] A real registered ToolSpec, guarded implementation, intent/policy mapping, and adversarial tests ship.
- [ ] The core catalogue flips ICMP from planned to available and links the real operation in the same change.

## Progress

- Not started.


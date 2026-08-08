---
id: E-83
title: "Guarded network primitives — DNS, TCP, UDP and ICMP behind one egress decision"
design: docs/designs/network-primitives.md
tracker: C-418
---

# Guarded network primitives — DNS, TCP, UDP and ICMP behind one egress decision

## Why

The design behind this epic is [docs/designs/network-primitives.md](../designs/network-primitives.md).
Its history was written down in [C-418](../stories/C-418-guarded-network-primitives-epic.md), which stays the narrative record.

## Success criteria

- [ ] C-284 lands first and the rest inherit its shape; a second guard is never introduced.
- [ ] Every primitive resolves before it decides, and decides on the resolved address rather than the
      name — the property `guard_url_scoped` already has, and the reason it exists.
- [ ] An unheld `CAP_NET_RAW` refuses at construction: a check that happens on the wire has already
      leaked the attempt (C-396).
- [ ] Every primitive behaves diagnosably under C-410's fail-closed sandbox — the failure names the
      sandbox rather than looking like an unreachable host.
- [ ] The relationship with `execution-substrate` is recorded in `docs/roadmap.md` so the next reader
      does not rediscover it.

## Exit criteria

- [ ] Every story carrying `epic: network-primitives` is `done` (`flux board epics --slug network-primitives`).
- [ ] Every success criterion above is ticked.

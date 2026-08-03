---
id: C-486
title: "Persist and supervise tenant connector channels in Exchange"
pillar: Core
status: done
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-exchange]
note: "external repository slice — durable tenant-owned ChannelStore, independent supervisors, placement resolver, restore/reconnect/rotation"
---

# Persist and supervise tenant connector channels in Exchange

## Goal

Give Exchange durable operator-created channels that resolve a declared binding against an existing
tenant connection and keep its vendor stream alive independently of agent subscribers.

## Acceptance

- [x] A persistent `ChannelStore` records authenticated tenant, connection, binding and selected
      declared events; no caller-supplied tenant, endpoint, credential or placement field exists.
- [x] An operator-owned endpoint/placement resolver selects direct-local, protected endpoint ref or
      trusted selected remote according to deployment profile before credentials are read.
- [x] Supervisors restore after restart, reconnect transient failures, stay stopped on terminal
      configuration failures and restart immediately after credential or connection-setting rotation.
- [x] Tests cover restoration, one vendor connection, no replay, rotation, all admitted placements,
      refused shared caller-host placement and secret/payload-free logs.

## Progress

- 2026-08-02: Exchange has tenant-scoped memory/persistent stores, independent restoration,
  reconnect supervision, rotation restart hooks and an operator-owned placement port. Binding that
  port to Flux's released typed placement remains blocked on the ordered dependency releases.
- 2026-08-03: Exchange v0.15.0 binds the released Flux 0.54.2 and connector 0.17.0 contracts; its
  restoration, reconnect, rotation, placement and redaction suites passed on main and the tag gate.

## Notes

- Implemented and tracked in `../flux-exchange`.

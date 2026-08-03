---
id: C-487
title: "Manage channels and multiplex live Exchange subscriptions"
pillar: Agent
status: in-progress
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-exchange]
note: "operator CRUD, default-deny inbound grants, one authenticated agent WebSocket, bounded per-subscriber fan-out and structured refusals"
---

# Manage channels and multiplex live Exchange subscriptions

## Goal

Expose durable channel management only to operators and live at-most-once event subscription only to
agents whose tenant grants explicitly admit the connector, binding and declared events.

## Acceptance

- [ ] Operator-only `GET/POST /api/channels`, `PUT/DELETE /api/channels/{id}` derive tenant and accept
      only connector/binding/event selection against existing connections.
- [ ] Grants gain explicit inbound `{connector, binding, events}` entries; old grants deserialize to
      no inbound access and every selected event must belong to the binding.
- [ ] `GET /api/subscribe` authenticates agents and multiplexes subscribe/unsubscribe by opaque
      channel id, with request-correlated acknowledgements and structured refusals.
- [ ] Emitted envelopes carry connector, binding, declared event name, receive time and raw typed
      payload; no replay/cursor/acknowledgement persistence is implied.
- [ ] One vendor socket fans out through bounded 32-event subscriber queues; overflow closes and
      counts only the slow subscriber without blocking the vendor loop or peers.
- [ ] Tests cover operator access, tenant derivation, default deny, event subsets, cross-tenant ids,
      fan-out, disconnect loss, slow-subscriber isolation and anonymous/log redaction.

## Progress

- 2026-08-02: Exchange has default-deny inbound grants, operator CRUD, authenticated multiplexing,
  bounded slow-subscriber isolation and tenant-opaque ids behind an optional supervisor. The public
  capability remains deliberately non-live until compatible released dependencies bind the runner.

- (blocked on C-486)

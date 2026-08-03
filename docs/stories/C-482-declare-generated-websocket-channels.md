---
id: C-482
title: "Declare and generate connector WebSocket channels"
pillar: Core
status: in-progress
priority: 1
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-connectors]
note: "external repository slice — socket connect IR, auth/config projections, zero-I/O channel_plan and complete Asterisk ARI event generation"
---

# Declare and generate connector WebSocket channels

## Goal

Make `flux-connectors` the source of truth for generic RFC 6455 handshakes and for Asterisk ARI's
complete event surface, while keeping all network I/O in consuming hosts.

## Acceptance

- [ ] Failing-first loader tests cover socket-only connect declarations, relative paths, fixed
      headers, query parameters, subprotocols, payload-root rules and channel-scoped config binds.
- [ ] Manifest and `connector-catalog` projections carry complete auth, config, event, binding and
      socket-connect facts; field-census tests fail when either projection drops an IR field.
- [ ] `connector_pack::channel_plan` composes and redacts an exact WebSocket URL, query, headers,
      subprotocols and credential placements without holding a client or opening a socket.
- [ ] Asterisk declares `ari-events` at `/events`, Basic auth, `app`, optional default-false
      `subscribe_all` rendered as `subscribeAll`, discriminator `type`, raw payload delivery, and one
      lowercase-kebab event per exact PascalCase ARI `Event` subtype with full schemas.
- [ ] Two-way source-operation and event-subtype census tests leave no unaccounted or silently emptied
      upstream route; scoped generation is green apart from documented coordinator-owned staleness.

## Progress

- 2026-08-02: implemented in `flux-connectors` C-489–C-492. The complete workspace gate is running
  from an isolated Cargo target after the ordinary target directory was removed by storage cleanup.

- (not started)

## Notes

- Implemented and tracked in `../flux-connectors`; this story is the cross-repository dependency
  contract for C-483/C-484.

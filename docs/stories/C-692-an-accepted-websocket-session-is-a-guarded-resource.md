---
id: C-692
title: "An accepted WebSocket session is a guarded resource"
pillar: "Core"
status: backlog
epic: the-substrate-seam
areas: [flux-system, flux-server]
design: docs/designs/the-substrate-seam.md
note: "ingress is guarded at bind_tcp and the TCP framing layer; the upgrade happens above the seam, so an accepted WS session has no per-message ceiling the way an outbound one does"
---

# An accepted WebSocket session is a guarded resource

## Goal

Inbound is in better shape than it looks: every production listener in the tree binds through
`bind_http_listener` → `GuardedNetwork::bind_tcp`, `BindExposure` makes an unauthenticated
non-loopback listener inexpressible, and `InboundLimits` caps connections, frame bytes and
per-operation deadline at the guarded edge. But the WebSocket *upgrade* happens above that seam,
in the serving crate: the port models the TCP listener, not the accepted session. So the
per-message ceiling an outbound socket carries (`max_message_bytes`, `queued_messages`,
`close_timeout`) has no inbound counterpart — the closest is `InboundLimits::max_frame_bytes`,
which bounds TCP framing rather than WebSocket messages. A long-lived inbound socket is guarded
at setup and at the byte layer, and unmodelled thereafter.

## Acceptance

- [ ] An accepted WebSocket session is a guarded resource with the same shape as the outbound one:
      per-message byte ceiling, queue depth, idle and close deadlines, enforced where the frames
      are read rather than by each route.
- [ ] Every production inbound upgrade path goes through it; a route cannot accept an upgrade
      outside the seam, and a census or equivalent check proves it.
- [ ] Exceeding a ceiling closes the session with a typed reason that reaches the operator log, and
      never truncates a message silently.
- [ ] The limits are configurable per listener alongside `InboundLimits` and default to the same
      conservative values the outbound side ships.

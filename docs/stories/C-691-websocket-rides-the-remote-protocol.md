---
id: C-691
title: "WebSocket rides the remote protocol"
pillar: "Core"
status: backlog
epic: the-substrate-seam
areas: [flux-system]
design: docs/designs/the-substrate-seam.md
note: "GuardedNetwork::open_websocket_scoped is on the port and native-only; RemoteSystem answers Unserved and websocket is absent from the wire's bounded operations — HTTP's position before C-674"
---

# WebSocket rides the remote protocol

## Goal

`GuardedNetwork::open_websocket_scoped` already takes a `WebSocketConnect` (URL, headers,
subprotocols, `max_message_bytes`, `queued_messages`, `close_timeout`) and returns a
`GuardedWebSocketSession`; the native `System` serves it and `RemoteSystem` answers a typed
`Unserved`. `websocket` is absent from the remote protocol's bounded operations, so an outbound
socket cannot follow a selected substrate — exactly where HTTP sat before C-674. This is the
harder half of that work: a WebSocket is a long-lived bidirectional stream rather than a
request/response, so the wire needs frame carriage and lifecycle, not one round trip. The
protocol already uses WSS for its own transport, so the machinery exists; what is missing is a
session multiplexed over it whose caps and closure are enforced on both sides.

## Acceptance

- [ ] A versioned protocol change carries WebSocket open, send, receive and close; a peer that
      does not serve it answers the typed `Unserved` without a round trip, and a mixed version
      pair refuses from both seats.
- [ ] `max_message_bytes`, `queued_messages` and `close_timeout` are enforced by the serving side
      and re-enforced by the requesting side; neither trusts the other's promise.
- [ ] Session lifetime is bounded and owned: a dropped handle closes the far-side socket, a
      cancelled turn closes it, and a far side that vanishes surfaces as a typed failure rather
      than a hang.
- [ ] The URL guard's scoped-destination judgment applies to the substrate's resolution of the
      target (composes with C-689), and the census entry for the remote implementation states the
      delegating truth.


## Comments

- C-674's review, for whoever implements this: the handshake advertises framed operations through a blanket predicate — .filter(|_| GuardedHttp::serves_http(&system)) applied to all of framed_operations() at flux-server/src/system.rs:408-413. Correct for a one-element list; a WebSocket family added as a second token will silently inherit HTTP's availability unless that becomes per-operation first.

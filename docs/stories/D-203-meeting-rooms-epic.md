---
id: D-203
title: "Meeting rooms — a multi-party channel where humans and agents meet (epic)"
pillar: Agent
status: ready
priority: 3
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels, flux-flow, flux-audio, docs]
note: "EPIC — feasibility PROVEN live 2026-07-30 against a real Brave Talk room: guest JWT from the public rooms endpoint, focus allocation, XMPP MUC presence and bidirectional chat, all with no browser and no account. Text+presence are native Rust; audio/screenshare need a feature-gated headless-Chrome media peer"
---

# Meeting rooms — a multi-party channel where humans and agents meet (epic)

## Goal

Give flux a channel where it is **one participant among several** — a meeting room containing humans and
agents at once, with presence, text, audio, and an agent-published screenshare. Every channel flux has
today is 1:1 or fire-and-forget; the voice path assumes exactly one caller. This epic adds the many-party
shape, with two swappable backends (generic XMPP MUC, and JaaS/Brave Talk) so the room is a portable
substrate rather than one vendor's feature — the place a fleet convenes.

## Acceptance

- [ ] A `Room` port with `XmppMucRoom`, `JaasRoom`, and `MockRoom` behind it, entering through
      `flux-channels`' `build_channels` as a new `room` kind (D-204, D-205, D-206).
- [ ] The agent responds **only when addressed** and never runs away in agent-to-agent chatter (D-207).
- [ ] Audio in and out through the existing `flux-flow::voice` seam, with per-speaker attribution
      (D-208, D-209, D-210).
- [ ] The agent can publish a rendered surface as a screenshare, redacted (D-211).
- [ ] Two flux agents can meet in one room and exchange structured A2A payloads (D-212).
- [ ] Every invariant in the [design](../designs/meeting-rooms.md) has a named test (D-213 owns the
      safety-envelope ones).
- [ ] The text+presence half builds and its tests pass **with no Chrome installed**.

## Progress

- 2026-07-30 — **feasibility spike, live against a real room.** Derived the handshake from the
  open-source [`brave/brave-talk`](https://github.com/brave/brave-talk) client and drove it from plain
  `curl` + a Python WebSocket client:
  - `OPTIONS` + `PUT /api/v1/rooms/<room>` on `talk.brave.com` → **HTTP 200 with an RS256 JaaS JWT**
    (3 h validity, `moderator: false`, room-scoped). No Brave account, no Premium, no browser. Creating a
    room needs a subscriber cookie; **joining one needs nothing.**
  - `POST https://8x8.vc/<tenant>/conference-request/v1` with the JWT as Bearer → `ready: true`.
  - `wss://8x8.vc/<tenant>/xmpp-websocket?room=…&token=…` → SASL **ANONYMOUS** (`PLAIN` is refused),
    bind, MUC presence → **the agent appeared as a visible occupant** next to the human and `focus`.
  - Outbound `groupchat` landed in the human's Brave Talk chat pane; inbound human messages were read
    off the same socket and answered. **Bidirectional text confirmed by the human in the call.**
- Next: D-204 (the port) then D-205 (the portable XMPP backend). D-213's invariants gate anything that
  publishes media.

## Notes

- Design: [meeting-rooms.md](../designs/meeting-rooms.md) — carries the measured handshake, the media
  decision, and the seven invariants.
- **A-112 (per-delivery bus isolation) is a real prerequisite** for more than one room: `AppDeliverer`
  serializes deliveries deliberately, and a busy room delivers continuously.
- Open question flagged in the design: **Brave Talk acceptable use for a non-browser client.** The
  endpoint is public and was used exactly as the open-source client uses it, against an invited room —
  but bot-joining at scale is a different posture. The generic XMPP backend and an own JaaS tenant are
  the answers for anything beyond own-room use.

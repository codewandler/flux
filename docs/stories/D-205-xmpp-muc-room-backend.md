---
id: D-205
title: XMPP MUC room backend — the portable room, no vendor and no browser
pillar: Agent
status: ready
priority: 25
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "generic prosody/ejabberd MUC over WebSocket: presence, occupants, groupchat, private messages — proven live 2026-07-30 with a hand-rolled client; two RFC 7395 traps recorded in the design (stanzas must be jabber:client-qualified; whitespace keepalive closes the stream 1007)"
---

# XMPP MUC room backend — the portable room, no vendor and no browser

## Goal

Implement `Room` over plain XMPP MUC, so a flux agent can sit in any standards-compliant room
(prosody, ejabberd, or a JaaS tenant) with **no browser and no vendor SDK**. This is the portable
backend and the one CI runs; D-206 layers vendor token acquisition on top of the same machinery.

## Acceptance

- [ ] `XmppMucRoom` implements `Room`: connect (WebSocket, RFC 7395), SASL, resource bind, MUC presence
      join, occupant tracking from presence, `groupchat` send/receive, private-message send/receive, leave.
- [ ] Failing-first test `xmpp_room_joins_and_exchanges_text` against an in-process XMPP double: join →
      occupants contains self → `say` emits a `groupchat` stanza → an inbound stanza surfaces as
      `RoomEvent::Message` with the right `OccupantId`.
- [ ] **Every stanza is `jabber:client`-qualified.** Regression test: an unqualified stanza is never
      emitted (prosody answers `<unsupported-stanza-type/>` and kills the stream — cost real time in the
      spike).
- [ ] **Keepalive is an XMPP ping IQ, never whitespace.** Regression test asserts no whitespace-only
      frame is ever sent (a `" "` frame is closed by the server with `1007`).
- [ ] Room JID case is taken from the server, not rebuilt locally (JaaS lowercases the room while the JWT
      keeps the original case).
- [ ] The whole story's test suite passes with **no Chrome installed**.

## Progress
- (not started — but the protocol sequence is already validated end to end; see the design's
  "Feasibility" section for the exact frames)

## Notes
- Proven live 2026-07-30: `<open/>` → SASL `ANONYMOUS` → `<open/>` → bind → MUC presence → `groupchat`.
  Only `ANONYMOUS` is offered on JaaS; `PLAIN` with the JWT as password is refused `<invalid-mechanism/>`.
- Prefer an existing Rust XMPP crate over hand-rolling the parser, but keep the surface to what `Room`
  needs — flux does not want a general XMPP client in its dependency tree.

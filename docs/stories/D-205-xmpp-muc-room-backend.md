---
id: D-205
title: XMPP MUC room backend — the portable room, no vendor and no browser
pillar: Agent
status: ready
priority: 4
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

- ⚠ **Two latent defects in D-204's port become REACHABLE the moment this story lands a real backend.**
  Both were found by D-204's independent review, are unreachable today only because `MockRoom` is
  infallible and is the sole registered backend, and are therefore this story's to handle:
  1. **A failed send leaves the room un-left and tears the host down.**
     `crates/flux-channels/src/rooms/driver.rs` does `self.room.say(&line).await?` / `whisper(...)?`,
     which returns early and skips `self.room.leave()`. Combined with `flux-channels/src/host.rs`
     ("until a channel *errors* (fatal)"), one failed send on a real backend is fatal to the host —
     the opposite posture from the deliberately non-fatal delivery error in `adapters/room.rs`. Decide
     the posture explicitly and test it.
  2. **Self-echo suppression rests entirely on your backend.** `driver.rs` trusts `Occupant.is_self`,
     and `Occupant::new` defaults it to `false` — so a backend built through `Occupant::new` rather
     than a struct literal is *not* forced to decide. The driver holds `identity.nick` and never
     cross-checks. A backend that omits `is_self` makes the agent answer its own messages in an
     unbounded loop, which costs real provider money. MUC self-presence necessarily precedes our own
     echo, so ordering is safe — the risk is omission, not timing. **Pin it with a test**; nothing in
     the port forces it today.
- `RoomSettings` fields are all `pub` and the type is re-exported, so adding credential fields here is
  source-breaking for external literal construction. `flux-channels` is unpublished, so no released API
  is affected — but consider `#[non_exhaustive]` as part of this story rather than later.
- Proven live 2026-07-30: `<open/>` → SASL `ANONYMOUS` → `<open/>` → bind → MUC presence → `groupchat`.
  Only `ANONYMOUS` is offered on JaaS; `PLAIN` with the JWT as password is refused `<invalid-mechanism/>`.
- Prefer an existing Rust XMPP crate over hand-rolling the parser, but keep the surface to what `Room`
  needs — flux does not want a general XMPP client in its dependency tree.

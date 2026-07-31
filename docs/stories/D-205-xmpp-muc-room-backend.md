---
id: D-205
title: XMPP MUC room backend — the portable room, no vendor and no browser
pillar: Agent
status: done
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

- [x] `XmppMucRoom` implements `Room`: connect (WebSocket, RFC 7395), SASL, resource bind, MUC presence
      join, occupant tracking from presence, `groupchat` send/receive, private-message send/receive, leave.
- [x] Failing-first test `xmpp_room_joins_and_exchanges_text` against an in-process XMPP double: join →
      occupants contains self → `say` emits a `groupchat` stanza → an inbound stanza surfaces as
      `RoomEvent::Message` with the right `OccupantId`.
- [x] **Every stanza is `jabber:client`-qualified.** Regression test: an unqualified stanza is never
      emitted (prosody answers `<unsupported-stanza-type/>` and kills the stream — cost real time in the
      spike).
- [x] **Keepalive is an XMPP ping IQ, never whitespace.** Regression test asserts no whitespace-only
      frame is ever sent (a `" "` frame is closed by the server with `1007`).
- [x] Room JID case is taken from the server, not rebuilt locally (JaaS lowercases the room while the JWT
      keeps the original case).
- [x] The whole story's test suite passes with **no Chrome installed**.

## Progress

Landed. The backend is `crates/flux-channels/src/rooms/xmpp/` — `mod.rs` (`XmppMucRoom`,
`XmppConfig`), `session.rs` (the RFC 7395 handshake, the MUC join, the socket loop), `stanza.rs` (the
element tree). Registered as `backend = "xmpp"` in `adapters/room.rs`; settings in
`config::RoomSettings` (`url`, `domain`, `user`, `password`, `muc_password`, `allow_private_net`),
which is now `#[non_exhaustive]` as the story's note asked.

**The dependency: `quick-xml` 0.41 (MIT), and nothing else new.** It is a *parser*, not an XMPP
client — the protocol is ours, about 200 lines of element tree. `tokio-xmpp` was rejected on a
structural ground rather than taste: it opens its own TCP socket and resolves its own DNS, so its
egress cannot be routed through `flux_system::net::guard_url_scoped`, and it drags a full XEP stack
plus a second TLS backend. quick-xml's only transitive dependency, `memchr`, is already in the graph.
`tokio-tungstenite` (already in the tree for the realtime/codex providers, `rustls-tls-webpki-roots`),
`futures-util`, `base64`, `url` and `flux-system` are new *edges* from `flux-channels`, not new crates
in the lock.

**Egress.** The endpoint is guarded in its `http`/`https` form by the one guard, and the dialled URL is
rebuilt from the guard's normalized answer — no second URL guard. Loopback needs
`allow_private_net`, which is the guard's own scoped grant. Known gap inherited from the guard's
URL-returning API and recorded in the design: the connection is not *pinned* to the vetted addresses,
so this closes SSRF-by-configuration, not DNS rebinding. Credentials are redacted from `XmppConfig`'s
`Debug` and never appear in an error.

**The two inherited defects are closed.**
1. *The leaked room.* `RoomTurnDriver::run` now splits into `run` + `session`, and leaves the room on
   **every** path out including a failed send (`a_failed_send_still_leaves_the_room`). The posture
   question is decided explicitly with a new `RoomSessionEnd`: a **join** failure is `run`'s `Err` and
   stays fatal to the host (a silently absent agent is worse than a loud stop), while a session that
   fails *after* joining is `RoomSessionEnd::Failed` — logged under the channel's name, ends that room,
   and leaves the program's other channels serving
   (`a_room_that_dies_mid_meeting_ends_its_channel_but_not_the_host`).
2. *Self-echo.* The backend decides `is_self` from two independent signals — XEP-0045's
   `<status code='110'/>` and the nick we joined under — and the driver no longer trusts the flag
   alone, re-checking `Occupant.nick` against `identity.nick`
   (`an_occupant_whose_backend_forgot_is_self_is_still_not_answered`, plus the end-to-end
   `exactly_one_occupant_is_self_and_the_agent_never_answers_its_own_echo`).

Tests — `crates/flux-channels/tests/xmpp_room.rs` against an in-process WebSocket double
(`tests/support/xmpp_double.rs`) that speaks the exact spike sequence, plus unit tests per module:
- `xmpp_room_joins_and_exchanges_text` (the failing-first one; at the merge base `XmppMucRoom` did not
  exist)
- `every_stanza_the_xmpp_backend_emits_is_jabber_client_qualified` — asserted on the raw frames the
  double recorded, not on a helper
- `the_xmpp_keepalive_is_a_ping_iq_and_never_whitespace`
- `the_room_jid_case_comes_from_the_server` — configured `StandUp@…`, server answers `standup@…`
- `the_endpoint_is_guarded_and_loopback_needs_a_grant`,
  `the_debug_rendering_never_carries_a_credential`, `text_and_attributes_are_escaped`
- `the_xmpp_backend_builds_from_a_decl_and_needs_an_endpoint`

**Left for the stories that own them:** `address_rule` is still carried and not enforced (D-207), so
every inbound line is a turn; the design's invariant 5 (self-announcement on join) is still in no
story's Acceptance. `OccupantKind` is `Unknown` for every occupant but us and `focus` — XMPP presence
carries no human-or-bot signal, which is a constraint D-207's ping-pong rule has to work with rather
than a gap here. Docs updated: the design's "As landed (D-205)" block and invariant 6, and
`website/docs/channels/inventory.md` (the `room` auth row, the "no public URL" cell, the `xmpp`
settings, the failure posture, and the stale "rooms are not a channel yet" known limit).

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

- 2026-07-31 — integrated, reviewed `PASS`. The review verified the two claims that mattered rather
  than accepting them: **egress** has exactly one dial site and the dialled URL is rebuilt from the
  guard's own normalized answer, so the vetted authority and the dialled authority cannot diverge;
  and the **dependency** claim was checked against `Cargo.lock`, not the manifest — `quick-xml` is the
  only new `[[package]]`, and `native-tls`/`openssl`/`rustls` hunks are absent, so no second TLS stack
  was linked. `cargo deny --offline check licenses` passes.
  ⚠ **Four things carried forward, none blocking, two of which the next room story should fix:**
  - **Outbound text is escaped for the five XML metacharacters but not filtered for codepoints XML 1.0
    forbids outright** (e.g. `\u{1}`). A control character in a model reply produces a frame a real
    server rejects — the same "one bad frame kills the stream" class the spike already paid for once —
    and there is no test. This is the highest-value follow-up here.
  - `quick-xml = "0.41"` is declared **bare** in `crates/flux-channels/Cargo.toml`, the only normal
    dependency in any workspace crate not declared `workspace = true`. A second crate wanting it will
    drift.
  - The endpoint URL is rendered verbatim into two error strings and into `XmppConfig`'s `Debug`, while
    `password`/`muc_password` are redacted beside it. Harmless for a generic MUC; **D-206 mints tokens
    that ride the endpoint URL**, so that asymmetry needs a decision no later than that story.
  - The egress guard is not connection-pinned, so DNS rebinding stays open. Consistent with
    `flux-web/src/browser.rs`'s precedent and unreachable from model output today — `RoomSettings`
    comes from a parse-time literal in the program, with only `secret "KEY"` resolution in between.

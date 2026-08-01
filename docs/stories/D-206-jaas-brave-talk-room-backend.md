---
id: D-206
title: JaaS / Brave Talk room backend — guest-token acquisition and refresh
pillar: Agent
status: in-progress
priority: 5
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "layers vendor token acquisition on D-205's XMPP machinery: CSRF + PUT /api/v1/rooms/<room>, conference-request, 3h token refresh; ⚠ read Brave's acceptable-use before this is more than own-room use"
---

# JaaS / Brave Talk room backend — guest-token acquisition and refresh

## Goal

Let a flux agent join a **Brave Talk** room (and any 8x8 JaaS tenant) by acquiring the token the way
Brave's own client does, then reusing D-205's XMPP MUC machinery for everything else. This is the
zero-setup path for a human who already runs Brave Talk.

## Acceptance

- [ ] `JaasRoom` acquires a guest token: `OPTIONS` for the CSRF header, then
      `PUT /api/v1/rooms/<room>` with the CSRF token and cookie jar → JWT.
      **Blocked: needs an HTTP client dependency.** The seam it goes through — `JaasTokens::guest_token`
      — has landed; the vendor implementation of that seam has not.
- [x] Focus allocation via `POST /<tenant>/conference-request/v1` with the JWT as Bearer, and the MUC JID
      is taken **from that response** (it lowercases the room; the JWT does not).
      *Partially:* the JID handling is implemented and tested
      (`JaasTokens::conference` → `JaasRoom::id`, `tests/jaas_room.rs::the_muc_jid_comes_from_the_conference_response_not_the_token`);
      the HTTP call itself is blocked with the item above.
- [x] The token rides the WebSocket URL as `?token=`, and SASL is `ANONYMOUS` — asserted, because `PLAIN`
      with the JWT is refused and a future maintainer will otherwise "fix" this the wrong way.
      → `tests/jaas_room.rs::the_jaas_token_rides_the_websocket_url_and_sasl_is_anonymous`
- [x] **Token refresh:** a session crossing the 3 h expiry re-mints and stays joined. Failing-first test
      `jaas_session_survives_token_expiry` against a fake token service with a short TTL.
      → `tests/jaas_room.rs::jaas_session_survives_token_expiry`, `src/rooms/jaas.rs`'s `SessionPump`
- [ ] An own-tenant mode where the JWT is signed locally from a configured JaaS API key, so production use
      needs no dependency on Brave's endpoint.
      **Blocked: needs an RS256 signing dependency** (none in the workspace). The shape is accommodated —
      an own-tenant `JaasTokens` is a second implementation of the same trait and nothing else changes.
- [ ] Credentials (API key, private key) come from the credential seam, never a literal in a `.flux` file.
      Not reachable until there is a credential-bearing token source to feed; `backend = "jaas"` is
      deliberately not declarable yet for the same reason.

## Progress
- **2026-08-01 — landed the mechanism, blocked on two dependencies.**
  `crates/flux-channels/src/rooms/jaas.rs` adds `JaasRoom`, the `JaasTokens` network seam
  (`guest_token` + `conference`, scoped to a room rather than a URL, per `flux_plugin::pack::Fetcher`),
  `GuestToken` (JWT claim decode, redacting `Debug`), and the refresh pump that re-mints ahead of the
  expiry and re-joins underneath its consumer. Tests: `crates/flux-channels/tests/jaas_room.rs`
  (4 integration tests against a fake token service + the in-process XMPP double — no vendor is
  reached) plus 7 unit tests in the module.
- **Also landed, and worth knowing:** the D-205 backend now renders an endpoint **without its query
  string** in every error and `Debug` that names one (`xmpp::endpoint_for_display`). A guest JWT rides
  `?token=`, so a failed connect previously would have published it into a log.
- **The two blockers, precisely.** `flux-channels` carries no HTTP client, so Brave's
  `OPTIONS`/`PUT /api/v1/rooms/<room>` handshake and the `conference-request` POST cannot be written:
  they need `reqwest.workspace = true` in `crates/flux-channels/Cargo.toml` (the workspace already
  pins reqwest 0.13; the `_gorilla_csrf` cookie is one cookie on one host and can be echoed by hand,
  so no cookie-jar feature is needed). Own-tenant local signing needs an RS256 implementation —
  `rsa` + `sha2` + `pkcs8`, or `jsonwebtoken` — none of which is in the workspace at all. Both are
  dependency-list edits, which is why they are not in this diff.
- **Next step:** add the dependency, implement `BraveTalkTokens` (and `OwnTenantTokens`) against the
  existing `JaasTokens` trait, then register `backend = "jaas"` in `adapters/room.rs` and add the
  `RoomSettings` fields for the token service + credential refs. Nothing above needs to change.

## Notes
- **Acceptable use is an open question and gates this story's scope.** The endpoint is public and
  unauthenticated, and the spike used it exactly as the open-source client does against an invited room.
  Bot-joining at scale is a different posture — read Brave's ToS first; prefer own-tenant JaaS or D-205's
  generic backend for anything beyond own-room use.
- Free-tier ceiling is 4 participants and our token carried `x-brave-features.group-room: "false"`, so
  multi-agent meetings (D-212) hit the cap immediately on Brave's free tier.

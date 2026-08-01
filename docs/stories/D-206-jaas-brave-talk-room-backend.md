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

- [x] `JaasRoom` acquires a guest token: `OPTIONS` for the CSRF header, then
      `PUT /api/v1/rooms/<room>` with the CSRF token and cookie jar → JWT.
      → `BraveTalkTokens::guest_token` (`src/rooms/jaas/tokens.rs`),
      `tests/jaas_tokens.rs::the_guest_token_handshake_sends_the_csrf_header_and_the_cookie_jar`
- [x] Focus allocation via `POST /<tenant>/conference-request/v1` with the JWT as Bearer, and the MUC JID
      is taken **from that response** (it lowercases the room; the JWT does not).
      → `BraveTalkTokens::conference`,
      `tests/jaas_tokens.rs::focus_allocation_sends_the_jwt_as_bearer_and_answers_with_the_lowercased_muc_jid`
      and `…::the_muc_jid_comes_from_the_conference_response_not_the_token`
- [x] The token rides the WebSocket URL as `?token=`, and SASL is `ANONYMOUS` — asserted, because `PLAIN`
      with the JWT is refused and a future maintainer will otherwise "fix" this the wrong way.
      → `tests/jaas_room.rs::the_jaas_token_rides_the_websocket_url_and_sasl_is_anonymous`
- [x] **Token refresh:** a session crossing the 3 h expiry re-mints and stays joined. Failing-first test
      `jaas_session_survives_token_expiry` against a fake token service with a short TTL.
      → `tests/jaas_room.rs::jaas_session_survives_token_expiry`, `src/rooms/jaas.rs`'s `SessionPump`
- [ ] An own-tenant mode where the JWT is signed locally from a configured JaaS API key, so production use
      needs no dependency on Brave's endpoint.
      **Deferred to its own story: needs an RS256 signing dependency** (`rsa`/`jsonwebtoken`/`ring` are all
      absent from the workspace). The shape is accommodated — an own-tenant `JaasTokens` is a second
      implementation of the same trait and nothing else changes. The vendor endpoints are already
      operator-configurable (`token_service` / `conference_service`), so a JaaS front end that mints
      guest tokens the Brave way needs no code at all.
- [ ] Credentials (API key, private key) come from the credential seam, never a literal in a `.flux` file.
      **There is no credential on the guest path** — Brave's endpoint is unauthenticated, which is *why*
      the CSRF handshake exists — so no `RoomSettings` field accepts a JWT, API key or private key
      (`tests/jaas_tokens.rs::the_jaas_backend_is_declarable_and_needs_no_credential_field`). The API key
      and private key this item names belong to own-tenant mode and are deferred with it; when it lands it
      inherits the existing seam (`flux_app::resolve_secrets` resolves `secret "KEY"` in a channel's
      settings at load and registers the value with the host's `Redactor`), so this is left unticked
      rather than claimed on a technicality.

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
- **2026-08-01 — the guest-token half landed** once `reqwest.workspace = true` was granted (one line
  in `crates/flux-channels/Cargo.toml`; the workspace already pinned reqwest 0.13 and five crates
  already depended on it). `BraveTalkTokens` (`src/rooms/jaas/tokens.rs`) implements the whole
  handshake against the seam that was already there — the `JaasTokens` trait needed no change, which
  is the point of having put it there. `backend = "jaas"` is now declarable.
  - **Every request is pinned**, not merely guarded: `guard_url_scoped_pinned` +
    `resolve_to_addrs` + `no_proxy`, redirects refused, empty pin set failing closed — the
    `flux-web` crawler's posture, and stronger than the WebSocket path, which the guard's
    URL-returning API cannot pin. Redirects are refused specifically because one would carry the
    `Authorization: Bearer <jwt>` header off the vetted origin.
  - **No response body reaches an error.** A vendor can echo our own token back at us.
  - **Vendor assumptions are marked in the source.** Brave publishes no API for this; every shape is
    a 2026-07-30 observation of the open-source client's traffic, so each load-bearing one carries a
    `VENDOR ASSUMPTION` comment at the line that depends on it. One is explicitly *inferred rather
    than measured*: the spike only saw `ready: true` from focus allocation, so `ready: false` is
    retried on a fixed backoff instead of keyed on a response field this repo has never seen.
  - Tests: `crates/flux-channels/tests/jaas_tokens.rs` (7) against an in-process Brave/JaaS double,
    including one that drives the **whole** of D-206 — HTTP handshake plus D-205's MUC join — with
    the vendor faked at both seams and nothing on the network.
- **Known gap worth a follow-up: the runtime-minted JWT is not registered with the `Redactor`.**
  `flux-channels` does not depend on `flux-secret` (the constraint `adapters/webhook.rs` already
  documents), and unlike a declared secret this token is minted at runtime, so `resolve_secrets`
  never sees it. It is held out of logs structurally — redacting `Debug`, query-trimmed endpoints, no
  bodies in errors, `set_sensitive` on the Bearer header — but a tool that echoed it would not be
  scrubbed. Closing it needs `flux-secret` in the manifest and a redactor threaded to the channel.
- **Next step:** own-tenant RS256 signing, as its own story and its own `JaasTokens` implementation.

## Notes
- **Acceptable use is an open question and gates this story's scope.** The endpoint is public and
  unauthenticated, and the spike used it exactly as the open-source client does against an invited room.
  Bot-joining at scale is a different posture — read Brave's ToS first; prefer own-tenant JaaS or D-205's
  generic backend for anything beyond own-room use.
- Free-tier ceiling is 4 participants and our token carried `x-brave-features.group-room: "false"`, so
  multi-agent meetings (D-212) hit the cap immediately on Brave's free tier.

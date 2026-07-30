---
id: D-206
title: JaaS / Brave Talk room backend — guest-token acquisition and refresh
pillar: Agent
status: ready
priority: 27
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
- [ ] Focus allocation via `POST /<tenant>/conference-request/v1` with the JWT as Bearer, and the MUC JID
      is taken **from that response** (it lowercases the room; the JWT does not).
- [ ] The token rides the WebSocket URL as `?token=`, and SASL is `ANONYMOUS` — asserted, because `PLAIN`
      with the JWT is refused and a future maintainer will otherwise "fix" this the wrong way.
- [ ] **Token refresh:** a session crossing the 3 h expiry re-mints and stays joined. Failing-first test
      `jaas_session_survives_token_expiry` against a fake token service with a short TTL.
- [ ] An own-tenant mode where the JWT is signed locally from a configured JaaS API key, so production use
      needs no dependency on Brave's endpoint.
- [ ] Credentials (API key, private key) come from the credential seam, never a literal in a `.flux` file.

## Progress
- (not started — the full handshake is recorded in the design, measured 2026-07-30)

## Notes
- **Acceptable use is an open question and gates this story's scope.** The endpoint is public and
  unauthenticated, and the spike used it exactly as the open-source client does against an invited room.
  Bot-joining at scale is a different posture — read Brave's ToS first; prefer own-tenant JaaS or D-205's
  generic backend for anything beyond own-room use.
- Free-tier ceiling is 4 participants and our token carried `x-brave-features.group-room: "false"`, so
  multi-agent meetings (D-212) hit the cap immediately on Brave's free tier.

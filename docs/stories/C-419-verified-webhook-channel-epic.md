---
id: C-419
title: "The verified webhook channel — a delivery flux can prove came from who it claims (epic)"
pillar: Core
status: backlog
epic: verified-webhook-channel
areas: [flux-channels]
note: "tracker filed 2026-08-01 by a board audit: four stories (C-292..C-295) carried this slug with nothing stating the initiative. ⚠ It has gained two neighbours since they were written — C-409 (the listener has none of flux-server's limits) and C-416 (the payload's principal is authenticated by nothing)"
---

# The verified webhook channel (epic)

## Goal

Make an inbound webhook delivery something flux can **prove** the origin of, route by, and hand on
with its provenance intact — rather than a JSON body it trusts because it arrived.

## Members

| story | status | what it is |
|---|---|---|
| C-292 | backlog | Signature schemes — one parameterized HMAC, constant-time, replay-bounded |
| C-293 | backlog | Challenge/handshake — answer endpoint verification without waking an agent |
| C-294 | backlog | Route a webhook to a trigger label by its event discriminator |
| C-295 | backlog | The delivery envelope — an `Event` carries no id, no source, no `verified` |

## ⚠ Two neighbours that arrived after these were written

The 2026-08-01 security-posture review and the day's identity work both landed on this surface. An
implementer picking any member up should know:

- **[C-409](C-409-channel-served-http-has-no-resource-limits.md)** — the webhook adapter binds its own
  listener and has **none** of `flux-server`'s body caps, timeouts, rate limits or concurrency
  admission (C-189 gave those to the server only). Not an auth bypass — the adapter refuses a
  non-loopback bind without authentication — but a webhook behind a proxy inherits none of its
  sibling's hardening. C-292's signature work and C-409's limits meet on the same request path.
- **[C-416](C-416-a-channel-adapter-should-declare-its-principal.md)** — a webhook body's principal is
  *authenticated by nothing*, and the adapter is the only component that knows that. C-295's
  "envelope carries no `verified`" is the same gap seen from the delivery side: once C-292 can prove
  an origin, C-295 is what carries the proof, and C-416 is what turns it into a trust level.

That ordering is the useful part: **prove it (C-292) → carry it (C-295) → decide with it (C-416)**.

## Acceptance (for the epic)

- [ ] One signature implementation, parameterized — not one per vendor. Constant-time compare and a
      replay bound are properties of the shared implementation, not of each caller.
- [ ] `verified` is a fact the envelope carries, and **nothing downstream infers it from arrival**.
- [ ] A challenge/handshake is answered without waking an agent — an endpoint-verification GET must
      not cost a turn.
- [ ] Routing by event discriminator does not require the agent to parse the body to decide whether it
      cares.
- [ ] The relationship with C-409 and C-416 is stated in `docs/roadmap.md`, so the three are not
      implemented as three unrelated answers to one request path.

## Notes

- Filed as part of the C-406 curation sweep. It is a **tracker**, not new scope: C-292..C-295 already
  existed and already carried this slug.
- C-418 (guarded network primitives) is the outbound counterpart — this one is inbound.
- ⚠ `AGENTS.md` names constant-time comparison and deny-by-default as safety invariants; this epic
  must tighten them, never relax them to make a vendor's scheme fit.

## Progress

- Filed 2026-08-01. Members unchanged; the neighbour relationships above are the new content.

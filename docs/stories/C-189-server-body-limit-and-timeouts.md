---
id: C-189
title: "Server body limits and request timeouts — SECURITY.md declares daemon DoS in scope, the routers have no limit"
pillar: Core
status: ready
priority: 6
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — a gap against flux's OWN published scope: SECURITY.md names --serve DoS as in-scope while flux-server/src/lib.rs mounts every router with no DefaultBodyLimit, TimeoutLayer or concurrency cap"
---

# Server body limits and request timeouts — SECURITY.md declares daemon DoS in scope, the routers have no limit

## Goal
`SECURITY.md` lists "denial of service in the `--serve` daemon" as an in-scope vulnerability class.
The routers carry no request-body limit, no request timeout and no concurrency cap, so an
unauthenticated-shaped request can consume memory or hold a connection indefinitely. Close the gap
between the published promise and the mounted middleware.

## Acceptance
- [ ] Every router mounted in `crates/flux-server/src/lib.rs` carries a `DefaultBodyLimit` with a
      documented default, overridable by config for the streaming/upload paths that legitimately
      need more.
- [ ] Every router carries a request `TimeoutLayer` with a documented default, with the SSE/streaming
      routes explicitly exempted (a long-lived stream is not a stuck request — the exemption must be
      deliberate and commented, not accidental).
- [ ] Failing-first tests: a request exceeding the body limit is rejected with `413` before the
      handler runs, and a handler exceeding the timeout yields `408`/`504` rather than hanging.
- [ ] The limits apply to the `a2a` routes as well (`crates/flux-server/src/a2a.rs`), not only the
      session routes.

## Progress
- (not started)

## Notes
- Verified absent: routers constructed at `crates/flux-server/src/lib.rs:584,:593,:603,:765,:775`
  carry no `DefaultBodyLimit`, `TimeoutLayer`, `ConcurrencyLimit` or rate-limit layer.
- **Rate limiting is deliberately NOT in this story.** It needs a keying decision — per token, per
  principal, or per realm — and that is a design question that would stall the uncontroversial half.
  Body limits and timeouts need no API decision and should not wait for one. File rate limiting
  separately when the keying question is answered.
- Note the composition risk the review flags: `--yes` permits automatic approval, so an exposed
  daemon with no resource limits is the worst-case configuration. Worth a line in the serving docs
  once this lands.
- Source: [2026-07-29 review](../../reviews/2026-07-29-security-posture-desk-review.md), finding
  "Server hardening appears incomplete" — verified.

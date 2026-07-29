---
id: C-189
title: "Server body limits and request timeouts — SECURITY.md declares daemon DoS in scope, the routers have no limit"
pillar: Core
priority: 6
epic: security-assurance
design: docs/designs/security-assurance.md
status: in-progress
note: "REVIEW — a gap against flux's OWN published scope: SECURITY.md names --serve DoS as in-scope while flux-server/src/lib.rs mounts every router with no DefaultBodyLimit, TimeoutLayer or concurrency cap"
---

# Server body limits and request timeouts — SECURITY.md declares daemon DoS in scope, the routers have no limit

## Goal
`SECURITY.md` lists "denial of service in the `--serve` daemon" as an in-scope vulnerability class.
The routers carry no request-body limit, no request timeout and no concurrency cap, so an
unauthenticated-shaped request can consume memory or hold a connection indefinitely. Close the gap
between the published promise and the mounted middleware.

## Acceptance
- [x] Every router mounted in `crates/flux-server/src/lib.rs` carries a `DefaultBodyLimit` with a
      documented default, overridable by config for the streaming/upload paths that legitimately
      need more.
- [x] Every router carries a request `TimeoutLayer` with a documented default, with the SSE/streaming
      routes explicitly exempted (a long-lived stream is not a stuck request — the exemption must be
      deliberate and commented, not accidental).
- [x] Failing-first tests: a request exceeding the body limit is rejected with `413` before the
      handler runs, and a handler exceeding the timeout yields `408`/`504` rather than hanging.
- [x] The limits apply to the `a2a` routes as well (`crates/flux-server/src/a2a.rs`), not only the
      session routes.

## Progress
- Landed on `impl/C-189`. Added `ServerLimits` (`max_body_bytes`, `request_timeout`) to
  `crates/flux-server/src/lib.rs` with documented defaults (`DEFAULT_MAX_BODY_BYTES` = 1 MiB,
  `DEFAULT_REQUEST_TIMEOUT_SECS` = 300s) and env overrides `FLUX_SERVER_MAX_BODY_BYTES` /
  `FLUX_SERVER_REQUEST_TIMEOUT_SECS` (mirroring the existing `FLUX_A2A_MAX_INFLIGHT_PER_REALM`
  knob style). Both single-agent (`router_with_ttl_and_limits`) and multi-agent
  (`router_multi_with_ttl_and_limits`) mounts apply a `DefaultBodyLimit` (axum built-in → `413`
  before the handler) over the whole surface and a `tower_http::timeout::TimeoutLayer`
  (`with_status_code(REQUEST_TIMEOUT, …)` → `408`) on every non-streaming route.
- The A2A routes (`POST /a2a`, `POST /{agent_id}/a2a`) carry both limits — they sit in the timed
  group.
- The SSE route `GET /sessions/{id}/stream` is split into its own `sessions_stream` sub-router
  (still realm-guarded) and deliberately exempted from the timeout, with the exemption commented.
  The A2A `message/stream` path is unaffected because the timeout bounds response *production*, not
  body streaming (verified against tower-http 0.6.11 source).
- Added `tower-http = { version = "0.6", features = ["timeout"] }` to `flux-server/Cargo.toml`
  (version already resolved transitively; `Cargo.lock` gains flux-server as a dependent). Body cap
  uses axum's built-in `DefaultBodyLimit` — no dep needed there.
- Tests: `body_over_limit_is_rejected_with_413` (red: 400, green: 413),
  `slow_handler_times_out_with_408` (red: 200, green: 408), plus
  `sse_stream_route_is_exempt_from_the_request_timeout`. Full gate green.
- Rate limiting remains out of scope per the story (needs a keying decision).

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

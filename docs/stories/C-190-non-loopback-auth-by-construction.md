---
id: C-190
title: "Make the non-loopback auth invariant hold by construction, not only inside serve_on"
pillar: Core
status: done
priority: 7
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — AGENTS.md says 'there are no bypass paths, don't add one'; the unauthenticated-non-loopback refusal lives in serve_on (lib.rs:457) so a caller mounting the router directly gets no guard at all"
---

# Make the non-loopback auth invariant hold by construction, not only inside serve_on

## Goal
`serve_on` refuses to bind an unauthenticated listener to a non-loopback address — a genuinely good
safeguard for a daemon that can execute operations. But the check lives in the serving function, not
in the router, so a lower-level caller that mounts the router into their own server inherits none of
it and is silently responsible for re-deriving the invariant. `AGENTS.md` is explicit that there are
no bypass paths; this is one that already exists. Make the invariant structural.

## Acceptance
- [x] The unauthenticated + non-loopback combination is unrepresentable or refused at **router
      construction**, not only at `serve_on`. Options to weigh in the design: a bind-address-aware
      constructor, or an explicit opt-out type (e.g. `UnauthenticatedLoopbackOnly`) a caller must
      name to get the open router. → chose the **bind-address-aware constructor**: `router`/
      `router_multi` now take `bind: SocketAddr` and return `anyhow::Result<Router>`, refusing
      `ServerAuth::Open` on a non-loopback bind via the one `guard_open_bind` helper
      (`crates/flux-server/src/lib.rs`).
- [x] Failing-first test: a caller that mounts the router directly and serves it on a non-loopback
      address cannot reach a protected route unauthenticated. This test must fail against the
      current tree — that failure is the proof the gap is real. →
      `unauthenticated_non_loopback_router_is_refused_at_construction` drives the REAL `router(...)`
      construction path (not the hand-built `guarded_app`).
- [x] `ServerAuth::Open`'s doc comments are updated to describe the enforced guarantee rather than
      the caller's responsibility (the `Open` variant doc + `from_token`; the stale
      "prefer serve_multi … a caller wiring axum::serve must enforce itself" caveat on `router_multi`
      is likewise rewritten now that construction enforces it).
- [x] Any intentional escape hatch is explicit, named, and logged loudly at startup. → **no escape
      hatch was added** (the safe default): `Open` off-loopback is refused outright, matching the
      pre-existing serving behavior, so there is nothing to name or log. A network-facing
      deployment fronts the loopback daemon with an authenticating proxy or configures
      shared-secret/principal auth.

## Progress
- Done. Design choice: **bind-address-aware constructor**, not an opt-out marker type. Rationale:
  the marker (`UnauthenticatedLoopbackOnly`) only witnesses "a caller named this type", not "this
  listener is loopback" — the address and the auth mode must be checked *together*, and the only
  place both are known is where the bind address is in hand. `router(engine, auth, card, bind)`
  makes that check unavoidable and keeps ONE enforcement point (`guard_open_bind`) that `serve`,
  `serve_multi`, and every direct router mounter (the `a2a` channel) share. C-435 subsequently
  removed the already-bound native-listener serving helpers. Public API break is sanctioned by the
  story (commit uses `!`).
- The real bypass caller was `flux-channels`' a2a adapter (`crates/flux-channels/src/adapters/a2a.rs`):
  it built `flux_server::router(...)` and served it with raw `axum::serve`, re-deriving the loopback
  refusal itself in `from_decl_and_app`. That early check stays (nice config-time error, same
  `addr.ip().is_loopback()` predicate) but is now backstopped by construction.
- Loopback semantics vs `a2a.rs`: confirmed they answer **different questions** and are correctly
  separate. The server bind check is IP-classification (`SocketAddr::ip().is_loopback()`, covers all
  of `127.0.0.0/8` + `::1`) on the *inbound* listen address. `a2a.rs:configured_push_private_net`'s
  "three loopback spellings" is a hostname allow-list for the *outbound* push-notification SSRF guard
  (`FLUX_A2A_PUSH_ALLOW_LOCAL`), routed through the DNS-aware `guard_url`. Unifying them would be
  wrong (an IP-classification bind check must not degrade to a 3-string allow-list that misses
  `127.0.0.2`; the egress guard must resolve hostnames, not classify a literal IP). Documented the
  divergence in `unauthenticated_bind_allowed`'s doc comment; no code change — out of scope, by design.

## Notes
- Verified: `crates/flux-server/src/lib.rs:457` — `"refusing unauthenticated non-loopback bind on
  {addr}; set FLUX_SERVER_TOKEN or bind …"`. The refusal is real and correct; its **placement** is
  the finding. `lib.rs:438` confirms it is `serve_on` that requires the loopback bind for
  `ServerAuth::Open`.
- Ranked above the review's own ordering for this one: the reviewer mentions it as a passing caveat
  under "server hardening", but it is not a hardening nice-to-have — it touches envelope integrity,
  which is flux's central claim.
- Related but distinct: `a2a.rs:1575` admits "only the three loopback spellings" for its own target
  check. Worth confirming both paths agree on what loopback means rather than each deciding
  separately.
- Source: [2026-07-29 review](../reviews/single/2026-07-29-security-posture-desk-review.md), the
  "Lower-level users can also mount the router directly" caveat — verified.

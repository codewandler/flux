---
id: C-261
title: "Add principal-aware daemon admission and completed-usage circuit breakers"
pillar: Core
status: done
priority: 6
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [flux-server]
note: "MEDIUM — authentication, body caps, and timeouts do not bound request arrival, queued work, or provider spend"
---

# Add principal-aware daemon admission and completed-usage circuit breakers

## Goal

Contain abuse by a stolen, shared, or noisy valid credential across REST, webhook, and A2A routes
without collapsing all tenants into one process-global bucket.

## Acceptance

- [x] A design-level key is explicit: principal identity where available, otherwise the authenticated
      realm/token identity; unauthenticated loopback use has its own bucket.
- [x] Failing-first tests show a valid principal can exceed request-rate and live-work bounds today,
      then receive `429` without minting sessions or starting model/tool work after the limit.
- [x] Bounded concurrency covers REST turns, webhooks, blocking A2A calls, and long-lived REST SSE;
      the existing A2A per-realm task cap remains a compatible inner bound.
- [x] Completed provider spend/call circuit-breaker thresholds are configurable per principal/realm;
      already-admitted overshoot is bounded by live-work concurrency, and failures are observable
      without revealing secrets or token values.
- [x] Zero-token and pre-usage-failure provider attempts remain countable call facts; permits charge
      their exact turn so reverse completion/drop order cannot move usage between principals.
- [x] Limit state is bounded/swept, emits metrics/audit context, and has documented reverse-proxy
      guidance for multi-replica deployments.
- [x] Server tests, docs, and the standard gate are green.

## Progress

- Added one cardinality-bounded, stale-swept in-process governor shared by REST, webhook, and A2A.
  Principal mode keys a fixed-size digest of the verified principal id; shared-secret and open
  loopback modes use constant realm buckets, so bearer/shared-secret values are never retained or
  logged. Admission precedes every relevant session mint and rejects with `429`, `Retry-After`, and
  `X-Flux-Limit`.
- Request rate is admitted once at the authenticated router boundary, covering protected reads as
  well as work without double-counting handlers. Permits cover full blocking, background A2A, and
  SSE producer lifetimes. Completed provider-call and priced-spend thresholds are retrospective
  circuit breakers, not prepaid hard caps: durable per-session usage is charged on permit drop,
  already-admitted turns may overshoot within the concurrency bound, and new work is rejected once
  completed usage reaches the threshold. Every provider attempt is persisted as a call fact even
  when it reports zero tokens. Each work permit is bound to its exact durable turn id while the
  engine gate is held, so overlapping principals and reverse completion order cannot claim one
  another's calls. Rejections and accounting failures emit secret-free metrics-shaped operational
  signals.
- Tests cover valid-principal isolation, protected reads, single boundary charging, no-mint
  rejection, cross-REST/webhook contention, background A2A permit ownership, completed-usage
  circuit breaking with bounded in-flight overshoot, overlapping same-session accounting,
  Retry-After ceiling, and cardinality fail-closed behavior. The website HTTP/config references and
  A2A docs include the retrospective semantics plus multi-replica and pre-admission authentication
  guidance.
- Scoped verification is green: `cargo test -p flux-server`, full `flux-config` tests,
  `cargo clippy -p flux-server --all-targets -- -D warnings`, package `cargo fmt --check`, and
  `cargo test -p flux-codegate`. The integrated workspace build/test/Clippy/format gate is also
  green.
- Closure regressions cover zero-usage calls tripping the call breaker and overlapping principals
  dropping their permits in reverse order while each is charged only its own turn.

## Notes

- Evidence: all reviews' daemon abuse-control findings; C-189 deliberately deferred rate limiting
  because the keying decision belonged in its own story.

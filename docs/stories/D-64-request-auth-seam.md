---
id: D-64
title: Per-request bearer→principal auth seam in flux-auth
pillar: Agent
status: done
design: docs/designs/request-auth-seam.md
note: "design approved + implemented same day via D-68/D-69"
---

# Per-request bearer→principal auth seam

## Goal
Add the seam flux-auth's own docs defer: a per-request token→principal resolution
(`authenticate(bearer) -> AuthContext { account, principal, roles }`) with a pluggable OAuth2
introspection implementation, so multi-tenant flux servers stop hand-rolling it.

## Why (evidence)
flux-auth's only seam is `IdentityProvider::resolve(&self) -> (Caller, Trust)`
(`crates/flux-auth/src/lib.rs:12`) — no token/request argument, so it structurally cannot express
per-request bearer auth; the crate doc explicitly defers OIDC/multi-user "when flux runs as a shared
server". That time arrived: the reviewed downstream consumer implements
`authenticate(bearer) -> AuthContext` + an OAuth2 introspection impl app-side, consumed by every
handler — and nothing in it is app-specific except the account-field name.

## Design sketch (to be developed before implementation)
- New trait beside (not replacing) `IdentityProvider` — e.g. `RequestAuthenticator`: async
  `authenticate(token: &str) -> Result<AuthContext>`; `AuthContext { account, principal, roles }`.
- Ship one impl: OAuth2 token-introspection (RFC 7662) with configurable endpoint/credentials;
  the consumer's introspection impl is the reference.
- Open questions: where the trait lives so flux-server AND external servers share it without
  layering violations; caching/negative-caching of introspection results; how `AuthContext` maps
  onto the existing `Caller`/`Trust` model (the envelope must stay the authorization source of truth).

## Acceptance
- [x] Design doc answering the open questions; explicit statement of how AuthContext relates to
      Caller/Trust and the safety envelope. → [request-auth-seam](../designs/request-auth-seam.md)
- [x] Implementation story split out after design review. → drafted as
      [D-68](D-68-request-auth-seam-impl.md) (seam + introspection) and
      [D-69](D-69-server-per-principal-isolation.md) (server per-principal isolation); promote to
      `ready` once the design review lands.

## Progress
- 2026-07-06 filed (design-first) from the downstream-consumer review.
- 2026-07-07 design doc drafted (`docs/designs/request-auth-seam.md`). Answers the three open
  questions: trait lives in flux-auth (L5; impl behind `introspect` feature so the seam stays
  dep-light); caching = `CachedAuthenticator` decorator (SHA-256 keys, positive TTL = min(exp,
  config), negative cache for Unauthorized only, capacity-bounded); AuthContext *contains* the
  projected `(Caller, Trust)` (one projection point) plus `account` as a tenancy-only key —
  envelope stays the sole authorization source of truth, trust clamped ≤ Verified, fail closed.
  Impl split: D-68 (flux-auth seam), D-69 (flux-server adoption; also answers D-63's auth-injection
  open question: auth is a layer, resolver never verifies tokens). Awaiting design review.
- 2026-07-07 design revised on user direction (A2A conformance / downstream adoptability /
  security): grounded against the A2A v1.0 spec (card `securitySchemes` MUST be declared to
  require auth; §13.1 existence hiding; `contextId` explicitly not a security boundary), against
  the downstream consumer's hand-rolled auth (optional client auth, dot-path account claim, roles
  as space-separated string, principal fallback chain, bare-token trait for non-header
  transports), and hardened via an adversarial review — 15 confirmed design-level fixes folded in
  (structural realm guard incl. the write path, redirect-refusing introspection client,
  non-optional realm key vs `NULL` collapse, reserved `account:` group prefix, refresh-token
  rejection, segregated cache stores, wire-constant errors, `dispatch` realm parameter replacing
  the unsound defaulted context field). D-68/D-69 acceptance updated to match.
- 2026-07-07 design approved by the user ("stick to A2A specs, keep usable by the downstream
  consumer, otherwise make it secure") and implemented the same day: D-68 + D-69 both DONE — see
  their Progress logs and the Unreleased CHANGELOG entry.

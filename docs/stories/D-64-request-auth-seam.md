---
id: D-64
title: Per-request bearer→principal auth seam in flux-auth
pillar: Agent
status: backlog
note: "design-first (2026-07-06 downstream-consumer review): IdentityProvider::resolve takes no request/token, so per-request introspection structurally can't plug in; flux-auth's docs already anticipate this"
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
- [ ] Design doc answering the open questions; explicit statement of how AuthContext relates to
      Caller/Trust and the safety envelope.
- [ ] Implementation story split out after design review.

## Progress
- 2026-07-06 filed (design-first) from the downstream-consumer review.

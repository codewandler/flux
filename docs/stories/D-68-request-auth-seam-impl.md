---
id: D-68
title: Implement RequestAuthenticator seam + RFC 7662 introspection in flux-auth
pillar: Agent
status: done
design: docs/designs/request-auth-seam.md
note: "shipped with D-69 (Unreleased)"
---

# Implement RequestAuthenticator seam + RFC 7662 introspection in flux-auth

## Goal
Land the per-request bearer→principal seam designed in
[request-auth-seam](../designs/request-auth-seam.md): `RequestAuthenticator::authenticate(bearer)
-> AuthContext { account, caller, trust }` in flux-auth, plus the shipped RFC 7662 introspection
impl and the TTL/negative/capacity-bounded caching decorator — so multi-tenant flux servers stop
hand-rolling it. flux-auth only; no server wiring (that is D-69).

## Acceptance
- [x] `flux_auth::request`: `AuthContext`, `AuthError { Unauthorized, Unavailable }`
      (`Unavailable` payload log-only; wire bodies + `WWW-Authenticate: Bearer
      error="invalid_token"` are byte-constant), `RequestAuthenticator` (object-safe, async), and
      `bearer_from_header` (case-insensitive scheme, single space, rejects empty / non-`b64token`
      chars / >8 KiB before any hashing or network; documented surface contract: >1 Authorization
      header is rejected by the caller). Trait + types add no heavy deps (async-trait only).
- [x] `Introspector` behind cargo feature `introspect` (workspace reqwest/rustls): RFC 7662
      form-serialized POST (never string concat); client auth **optional** (`Some` →
      `client_secret_basic`, `None` → bare POST); `redirect::Policy::none()` — any 3xx →
      `Unavailable`; https required unless `allow_http: true`; response body capped (256 KiB);
      non-access `token_type`/`token_use`/`typ` → `Unauthorized`; non-200/timeout/malformed →
      `Unavailable` (fail closed).
- [x] Claims mapping per the design: account claim literal-key-first then dot-path +
      `require_account`; roles claim accepts JSON array or space-separated string, verbatim except
      the reserved `account:` prefix is stripped (authenticator is the mirror group's sole writer);
      principal chain `sub`→`username`→`client_id`, account fallback namespaced
      (`account:<value>`, source `introspect:account-fallback`); `CallerKind::Agent` heuristic for
      client-credentials-shaped tokens; trust level clamped ≤ `Verified`.
- [x] `CachedAuthenticator<A>` decorator: SHA-256(token) keys; **segregated** positive/negative
      stores (negatives can never evict positives); positive TTL =
      `exp.saturating_sub(now).min(config)`, `ttl == 0` → never cached; negative cache
      (`Unauthorized` only) with its own quota; `Unavailable` never cached; capacity-bounded.
- [x] Failing-first tests per the design's Testing section: mock-endpoint contract (incl. redirect
      refusal, form-injection/edge tokens, over-cap body, non-access token, claim-path
      literal-vs-dotted), trust clamp, cache (exp math, segregation flood, negative window,
      eviction), leak/redaction (Debug, wire constants), reserved-prefix stripping, and the shared
      claims→(Caller,Trust) projection test against `OidcIdentity`.
- [x] Workspace gate green (build, test, clippy -D warnings, fmt, codegate — flux-auth stays L5).

## Progress
- 2026-07-07 split out of D-64 after the design doc was drafted; blocked only on design review.
- 2026-07-07 acceptance hardened from the design revision (A2A-spec + adoptability + security
  red-team): header helper, optional client auth, redirect-none, https/`allow_http`, response cap,
  non-access-token rejection, literal-then-dot-path account claim, reserved `account:` prefix,
  namespaced principal fallback, segregated cache stores, saturating exp math, wire-constant errors.

## Notes
- Design: [docs/designs/request-auth-seam.md](../designs/request-auth-seam.md) — the invariants
  (envelope stays authorization SoT; trust ceiling; fail closed) are acceptance-relevant.
- Server adoption (per-principal isolation, realm-keyed A-48 continuity) is **D-69**, not here.
- 2026-07-07 DONE: `flux_auth::request` (trait + AuthContext + AuthError + bearer_from_header) +
  `introspect` feature (Introspector + CachedAuthenticator) landed with 42 new hermetic tests
  (45 total with the feature); package gate + codegate green; CHANGELOG entry under Unreleased.
- 2026-07-07 pre-release review: broadened the non-access-token guard from an exact allowlist to a
  normalized family-prefix match (`"Refresh Token"`, `"refresh-token"`, `REFRESH_TOKEN`, logout all
  rejected) with tests.

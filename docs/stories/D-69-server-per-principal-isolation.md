---
id: D-69
title: Per-principal isolation in flux-server via the request-auth seam
pillar: Agent
status: done
design: docs/designs/request-auth-seam.md
note: "shipped (Unreleased); D-63 composes: auth stays a layer, resolver never verifies tokens"
---

# Per-principal isolation in flux-server via the request-auth seam

## Goal
Wire D-68's `RequestAuthenticator` into flux-server so a bearer token resolves to a principal per
request, sessions are account-tagged and account-scoped, and A-48 `contextId` continuity is keyed
within the caller's realm — delivering the per-principal isolation `docs/a2a.md` promises for D-64.

## Why (evidence)
Today `require_auth` (`crates/flux-server/src/lib.rs:262`) verifies one static shared secret — the
whole server is a single bearer-auth realm. A-48 session reuse keys on `contextId` alone, which its
own story flags as safe *only* within one realm; and the engine's `(Caller, Trust)` is fixed at
build time (`crates/flux-flow/src/engine.rs:46`), so every caller's turns run under the service
identity.

## Acceptance
- [x] Three explicit auth modes in flux-server: loopback-open / shared-secret (unchanged) /
      per-request principal (`Option<Arc<dyn RequestAuthenticator>>` in `ServerState`; resolved
      `AuthContext` in request extensions; `Unauthorized`→401 with byte-constant
      `WWW-Authenticate: Bearer error="invalid_token"`, `Unavailable`→503 with constant body,
      fail closed). The principal-mode constructor is **type-coupled** to the per-turn
      envelope-identity mechanism — mode 3 is unconstructible without it.
- [x] Realm key non-optional in principal mode: `account` if present else `user:<principal.id>`;
      SQL `IS`-vs-`=` semantics specified; legacy `NULL`-account sessions unreachable in principal
      mode; loopback/shared-secret behavior byte-for-byte unchanged (regression test).
- [x] One **structural realm guard** on every `/sessions/:id/*` route — including
      `POST /sessions/:id/messages` — returning 404 with status+body byte-identical to a
      nonexistent id (A2A §13.1); router-level test enumerates all routes so new ones fail closed;
      `/usage` account-scoped; `/webhook` + `POST /sessions` tag the caller's account or are
      disabled in principal mode.
- [x] A-48 continuity realm-keyed in **both** dispatchers (flux-server `a2a.rs` and
      `flux_a2a::server::dispatch`): same `contextId` under two realms yields two sessions
      (failing-first test). `dispatch` gains a required authenticated-realm parameter supplied by
      the mount after its own auth (breaking; SemVer minor).
- [x] A2A card conformance: flux-a2a `AgentCard` gains additive `security_schemes` + `security`
      fields; flux-server advertises `{type:"http", scheme:"bearer"}` + requirement whenever auth
      is enabled; card stays public/structurally exempt and its `url` derives from configured
      external base, never the Host header.
- [x] Envelope identity: a principal's turn runs under that principal's `(Caller, Trust)`, never
      the service identity — mechanism decided here (per-realm engines vs per-turn override under
      `turn_gate`); test proves a policy deny for caller A is not bypassed via caller B's realm.
- [x] `docs/a2a.md` single-realm caveat updated; workspace gate green.

## Progress
- 2026-07-07 split out of D-64 after the design doc was drafted; blocked on D-68 (the seam itself).
- 2026-07-07 acceptance hardened from the design revision: structural realm guard (incl. the
  `POST /messages` write path), non-optional realm key + `IS` semantics, mode-3 type-coupling to
  envelope identity, card `securitySchemes` advertisement + external-base URL, byte-identical
  404s, realm param through both dispatchers.

## Notes
- Design: [docs/designs/request-auth-seam.md](../designs/request-auth-seam.md) (Consumption sketch
  section) — including the honest gap on the shared `Arc<Executor>`.
- D-63 (multi-agent A2A mount): its "auth injection point" open question is answered here — auth
  stays a middleware layer; the resolver consumes `AuthContext`, never verifies tokens.
- 2026-07-07 DONE: ServerAuth three modes + principal middleware (constant 401/503,
  WWW-Authenticate, duplicate-header rejection), structural realm guard on the whole
  `/sessions/:id/*` subtree (write path included, byte-identical 404s), realm-tagged mints,
  realm-scoped `/usage`, realm-keyed continuity in BOTH dispatchers (`find_correlated_in_realm`
  in flux-events; `dispatch(runner, realm, body)` + `A2aTurnContext.realm` in flux-a2a —
  breaking, minor bump), card `securitySchemes`/`security` + external-base URL, and per-turn
  envelope identity via `IdentityCell` (flux-runtime) shared executor↔spawner with gate-witnessed
  `enter_turn`; the deny-not-bypassed invariant is unit-proven in flux-runtime
  (`set_identity_swaps_the_policy_subject_between_turns`) and the server identity swap in
  `tests/principal_auth.rs` (8 new integration tests). `docs/a2a.md` security notes rewritten.
  Full workspace gate green.
- 2026-07-14 — A-87 supersedes that historical mutable-identity implementation: the server now
  constructs an immutable `TurnIdentity` and passes it through the engine-owned `run_turn*_as`
  entry points; mutable `IdentityCell` retargeting, `enter_turn`, and the old swap test no longer
  exist.
- 2026-07-07 pre-release review hardening: realm keys namespaced `acct:`/`user:` (disjoint, closes
  the account-value/principal-realm collision) with a regression test; realm-scoped `/usage` now
  goes through the store's `cost_summary_for_account` (shared canonical fold, no key-split/unpriced
  divergence); shared-secret mode carries `external_url` (card no longer Host-phishable);
  `serve_multi` guards the D-63 mount's bind through its required `ExecutionSystem` (C-435 removed
  the already-bound native-listener helper); duplicate-`Authorization` rejection
  applied to shared-secret too; secret-without-client-id is now a hard error; account-claim-without-
  require_account warns. Full gate green.

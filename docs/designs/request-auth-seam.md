# Design: per-request bearer→principal auth seam

**Status:** implemented (stories [D-64](../stories/D-64-request-auth-seam.md) →
[D-68](../stories/D-68-request-auth-seam-impl.md) + [D-69](../stories/D-69-server-per-principal-isolation.md))
· **Layer:** L5 (`flux-auth`) + L6 (`flux-server`) · **Owner:** Timo

Revised 2026-07-07 after an A2A-spec conformance pass, a downstream-adoptability pass, and an
adversarial security review (15 confirmed design-level fixes folded in).

## Why

flux-auth's only seam is process-ambient: `IdentityProvider::resolve(&self) -> (Caller, Trust)`
(`crates/flux-auth/src/lib.rs:12`) — no token argument, resolved **once per process**. Its crate doc
explicitly defers multi-user "when flux runs as a shared server". That time arrived: flux-server
authenticates every request against **one static shared secret** (`require_auth`,
`crates/flux-server/src/lib.rs:262` — constant-time compare, no principal), so it authenticates the
*deployment*, never a caller. The reviewed downstream consumer hand-rolls exactly the missing piece —
`authenticate(bearer) -> AuthContext` plus an OAuth2 token-introspection impl — and nothing in it is
app-specific except claim names. Meanwhile [docs/a2a.md](../a2a.md) already promises "per-principal
isolation arrives with the request-auth seam (D-64)".

The A2A spec (v1.0.0) makes three of this design's obligations explicit:

- Clients "MUST authenticate the request using one of the schemes declared in the public
  `AgentCard.securitySchemes` and `AgentCard.security` fields" — so a bearer-requiring server whose
  card declares no scheme is non-conformant in practice. flux-a2a's `AgentCard`
  (`crates/flux-a2a/src/types.rs:418`) has neither field today.
- §13.1: servers MUST scope authorization and "MUST NOT reveal the existence of resources the
  client is not authorized to access" — cross-account probes get 404, indistinguishable from
  nonexistent.
- `contextId` is a logical grouping mechanism, **not a security boundary** — so A-48 `contextId`
  continuity must be keyed within the caller's realm, exactly the caveat the A-48 story recorded.

## The seam — `flux_auth::request`

```rust
/// Request-scoped identity: everything a multi-tenant surface needs from one bearer token.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub account: Option<String>, // tenancy key → EventContext.account (D-02); NOT a policy input
    pub caller: Caller,          // principal + roles-as-groups → Executor identity
    pub trust: Trust,            // level (≤ Verified) + token scopes → Executor identity
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthError {
    Unauthorized,        // invalid / expired / inactive / wrong-type token → HTTP 401
    Unavailable(String), // authenticator backend failure; NEVER a pass-through
}

#[async_trait::async_trait]
pub trait RequestAuthenticator: Send + Sync {
    async fn authenticate(&self, bearer: &str) -> Result<AuthContext, AuthError>;
}

/// RFC 7235/6750 extraction: case-insensitive `Bearer` scheme, exactly one space, non-empty
/// token; rejects non-`b64token` characters and tokens over 8 KiB *before* any hashing or
/// network work (bounds attacker-driven CPU as well).
pub fn bearer_from_header(header: Option<&str>) -> Result<&str, AuthError>;
```

- **The trait takes the bare token, not the header.** Tokens legitimately arrive via non-header
  transports (e.g. a WebSocket subprotocol on a voice surface); the header concern lives in
  `bearer_from_header`, which every HTTP surface uses so parsing is uniform. **Surface contract:**
  a request presenting more than one `Authorization` header is rejected by the surface before
  extraction — the helper sees a single value and cannot detect duplicates (smuggling-style proxy
  divergence otherwise).
- **Error hygiene:** `Unavailable`'s payload is **log-only** — wire responses (body *and* any
  header) are constant strings, because backend error text can carry internal endpoint URLs and
  header interpolation is CRLF injection. A 401 carries the byte-constant challenge
  `WWW-Authenticate: Bearer error="invalid_token"` (RFC 6750; the A2A spec's "SHOULD include
  authentication challenge information"). Surfaces map `Unavailable` to 502 or 503 as fits their
  topology (flux-server: 503) — fail closed either way; there is no anonymous fallback identity.
- **Refines the story sketch** `{ account, principal, roles }`: `principal` ≡ `caller.principal`,
  `roles` ≡ `caller.groups`. `AuthContext` carries the *already-projected* `(Caller, Trust)` so
  there is exactly **one** claims→identity projection point (the authenticator) and no consumer can
  invent its own mapping.
- **Sibling, not replacement:** `IdentityProvider` stays as the ambient process identity (CLI);
  `RequestAuthenticator` is its per-request generalization. Both terminate in the same
  `(Caller, Trust)` the envelope evaluates. Object-safe (`Arc<dyn RequestAuthenticator>`).

## AuthContext vs Caller/Trust and the safety envelope (the invariant)

**The envelope stays the sole authorization source of truth.** `AuthContext` makes no authorization
decisions; it only *feeds* the identical inputs the CLI feeds today — `(Caller, Trust)` into
`Executor` → `flux_policy::evaluate` (default-deny). The authenticator **authenticates**; the
envelope **authorizes**. Concretely:

- `account` is a **tenancy/storage key only**: it flows to `EventContext.account` (the D-02
  substrate) to tag runs and scope session reads/continuity. Policy never consults the field.
  Account-conditional *grants* work via a mirror group `account:<id>` in `caller.groups` — and that
  prefix is **reserved: the authenticator is its only writer**. Values arriving through a roles
  claim with the `account:` prefix are stripped (a tenant-managed role named `account:victim-org`
  must not mint cross-tenant group grants).
- **Trust ceiling:** a network-presented token yields at most `TrustLevel::Verified`.
  `Privileged`/`System` are not derivable from any claim; the shipped impl clamps its configured
  level. (`LocalIdentity` keeps `Privileged` — the machine owner is a different trust story.)
- **Fail closed**, as specified under error hygiene above.

## Claims → identity mapping (shipped impl)

- **Account:** a configurable claim path, resolved **literal-key first, dot-path on miss** —
  namespaced OIDC claim names (`"https://idp.example/account_id"`) contain dots and must not be
  silently split into a missing nested path (which would drop every conformant deployment into the
  no-account case below). Nested paths like `info.accountId` resolve on the miss branch. A
  `require_account: bool` knob maps a missing/empty account to `Unauthorized` for deployments where
  tenancy is mandatory.
- **Roles:** the configured claim accepts a JSON array of strings **or** one space-separated
  string (both occur in the wild). Values pass through verbatim into `caller.groups` — no
  normalization; that's app policy — except the reserved `account:` prefix per above.
- **Principal id:** first non-empty of `sub` → `username` → `client_id`. Final fallback: the
  account value, **namespaced** as `principal.id = "account:<value>"` with
  `caller.source = "introspect:account-fallback"` — a *shared service principal*, distinguishable
  in audit and never colliding with a real `sub`/`client_id` in policy subjects. All absent →
  `Unauthorized`.
- **CallerKind (heuristic, documented):** `Agent` when the token is client-credentials-shaped
  (`sub == client_id`, or `client_id` present with no `username`) — real IdPs set `sub` on service
  tokens, so keying on "came from client_id" alone would leave `SubjectKind::User` grants silently
  matching machines. Otherwise `User`.
- **Token type:** if `token_type` / `token_use` / `typ` identifies a non-access token (refresh, id)
  → `Unauthorized`. RFC 7662 happily introspects refresh tokens as `active:true`; a leaked refresh
  token — useless at the token endpoint without the client secret — must not become a working
  bearer credential here.
- `scope` (space-split) → `trust.scopes`; `caller.source = "introspect"`; token `exp`, when
  present, bounds the cache TTL (below).

## Shipped impl — RFC 7662 token introspection (feature `introspect`)

The trait + types land dependency-light (flux-auth today depends only on `flux-policy`; the seam
adds `async-trait`). The concrete impl sits behind a cargo feature `introspect` (off by default)
pulling workspace `reqwest` (rustls) + `sha2`.

```rust
pub struct IntrospectionConfig {
    pub endpoint: String,               // deployment config, never model/request-reachable
    pub client: Option<(String, String)>, // (client_id, client_secret) → client_secret_basic;
                                        // None → bare POST (brokers that gate at the network layer)
    pub allow_http: bool,               // default false: https required; explicit opt-in for
                                        // trusted-network/cluster-internal endpoints
    pub account_claim: Option<String>,  // literal-first/dot-path claim → AuthContext.account
    pub require_account: bool,
    pub roles_claim: Option<String>,    // array or space-separated → caller.groups
    pub trust_level: TrustLevel,        // clamped to ≤ Verified
    pub timeout: std::time::Duration,
}
pub struct Introspector { /* reqwest::Client + config */ }  // impl RequestAuthenticator
```

- `POST endpoint` with form-serialized `token=<bearer>&token_type_hint=access_token` (serializer,
  never string concatenation — tokens containing `&`/`=`/`%` must arrive as exactly one form
  field). RFC 7662 §2.1 expects the caller to be authorized; deployments whose broker enforces that
  at the network layer run with `client: None`.
- **`redirect::Policy::none()` — any 3xx is `Unavailable`.** reqwest's default policy follows up to
  10 redirects and 307/308 preserve the POST body: a compromised or misconfigured endpoint
  answering `307 Location: https://attacker/` would forward every caller's live bearer token.
  https-by-default validates only the *configured* URL; refusing redirects is what makes it mean
  something.
- **Response cap 256 KiB** (Content-Length check + limited body read); over-limit → `Unavailable`.
  An unbounded `.json()` read makes the pre-authorization path the cheapest OOM in the process.
- `active:false` → `Unauthorized`; non-200 / timeout / malformed body → `Unavailable`.

## Caching — a decorator, not baked in

`CachedAuthenticator<A: RequestAuthenticator>` wraps **any** authenticator (introspection stays
cache-free and hermetically testable):

- **Key = SHA-256(token)** — raw bearers never sit in a map (and the hash normalizes lookup timing
  across token prefixes).
- **Segregated positive/negative stores.** Positive TTL:
  `ttl = exp.saturating_sub(now).min(positive_ttl)` (default 60 s); `ttl == 0` (expired-but-active,
  clock skew, millisecond-unit `exp`) → return the result but **never cache**. Saturating math —
  wrapping subtraction would cache an expired token for the full window.
- **Negative cache** for `Unauthorized` only (default 5 s, own capacity quota): it absorbs
  *repeated identical* bad tokens (retry loops, misconfigured clients). It does **not** protect the
  IdP from a unique-garbage flood — every unique token is a first sight; that protection is
  upstream rate limiting, and the design says so rather than pretending otherwise. Negative inserts
  can never evict positive entries — a shared evict-oldest map would let ~200 rps of garbage churn
  out every legitimate entry and hand the IdP the full load precisely under attack.
- `Unavailable` is **never** cached in either direction. Capacity-bounded (default 1024 positive
  entries, evict oldest). No single-flight in v1: N concurrent first sights of one token may
  introspect N times — correct, merely not maximally cheap.
- Cache lifetime is bound to its authenticator instance; if config ever becomes reloadable they are
  rebuilt together.

## A2A conformance

- **Card declares the scheme.** flux-a2a's `AgentCard` gains additive optional fields
  `security_schemes` (map name → scheme object, serde `camelCase`) and `security` (requirement
  array). flux-server advertises `{"type": "http", "scheme": "bearer"}` plus a matching requirement
  whenever auth is enabled (shared-secret or principal mode) — the spec's MUST-use-declared-schemes
  is only satisfiable if servers actually declare. (`protocolVersion` is an adjacent observed card
  gap; out of scope here.)
- **The card stays public** and structurally auth-exempt (registered outside the middleware, as
  today). Because the card now tells clients *where to send bearer tokens*, its `url` must derive
  from configured external base URL — never from the request's Host header, or poisoning an exempt
  route becomes a token-phishing primitive.
- **Existence hiding (§13.1):** cross-account access to a session returns 404 with status+body
  **byte-identical** to a nonexistent id (today's handlers interpolate "session s_X not found" —
  the realm guard returns one constant shape).
- **Realm-keyed continuity:** `contextId` is not a security boundary, so the A-48 lookup
  (`find_correlated`) must also match the caller's realm. This lands in **both** dispatchers —
  flux-server's own `a2a.rs` handlers *and* `flux_a2a::server::dispatch` — or they drift.
- **`dispatch` carries the realm explicitly.** `flux_a2a::server::dispatch` builds
  `A2aTurnContext` internally from the request body, so a defaulted `account` field could never be
  "set by the mount" and would silently hand every implementor `None`. Instead `dispatch` gains a
  required authenticated-realm parameter (supplied by the mount *after its own auth*, never derived
  from message content). **Breaking** — SemVer minor per repo rule; a one-line change at downstream
  call sites, and exactly the value they need to adopt continuity safely.

## Consumption sketch (the split-out stories)

- **D-68 (implementation of this design):** the seam + `bearer_from_header` + `Introspector` +
  `CachedAuthenticator` in flux-auth, tests below. No server changes.
- **D-69 (flux-server per-principal isolation, adoption):** three explicit auth modes — loopback
  open / shared secret (today's, unchanged) / per-request principal. In principal mode:
  - **The realm key is non-optional:** the caller's realm is `account` when present, else
    `user:<principal.id>` — `account: None` must not collapse all account-less principals into one
    shared realm. Comparisons are specified with SQL `IS` semantics in mind: a naive
    `AND account = ?` never matches NULL (silently breaking continuity), while `IS`-matching NULL
    would re-open the shared realm. Legacy untagged (`NULL`) sessions are simply unreachable in
    principal mode; loopback/shared-secret modes keep today's behavior byte-for-byte.
  - **One structural realm guard** wraps *every* `/sessions/:id/*` route — including
    `POST /sessions/:id/messages`, the write path (session ids are guessable `s_<n>`; enumerating
    read routes while leaving the write route open is cross-tenant read+write). A router-level test
    enumerates all routes so a future route fails closed. `/usage` returns only the caller's
    account; `/webhook` and `POST /sessions` tag the caller's account or are disabled in principal
    mode.
  - **Envelope identity is type-coupled to mode 3.** `FlowEngine.executor` is one shared
    `Arc<Executor>` with `(Caller, Trust)` fixed at build (`crates/flux-flow/src/engine.rs:46`).
    The principal-mode constructor *requires* the per-turn identity mechanism (per-realm engines or
    a per-turn override serialized under the existing `turn_gate` — decided in D-69), so "every
    principal's tools run under the service identity" cannot ship by accident. Invariant: *a
    principal's turn runs under that principal's `(Caller, Trust)`, never the service identity.*
- **D-63 composition (answers its open question):** auth stays a **layer**, not resolver-owned. The
  multi-agent `AgentResolver` receives the already-authenticated `AuthContext` and may key agent
  resolution on it, but never verifies tokens itself — one verification point, and the structural
  exemption of discovery routes is preserved.

## Adoptability (the downstream consumer)

The reviewed consumer's hand-rolled seam maps onto this design with no adapter logic beyond glue:
bare-token trait + header helper (it also presents tokens over a WebSocket subprotocol), optional
client auth (its broker takes none), literal/dot-path account claim (its account lives at a nested
path, name env-configured), roles as a space-separated string, the `sub`→`username`→`client_id`
principal chain, and a 401-vs-upstream-error split its surfaces already map. It runs **no**
introspection caching today — every authenticated request is a synchronous IdP round-trip — so the
decorator is immediate net win. Its internal-network `http://` introspection endpoint is one
explicit `allow_http: true` away.

## Testing (hermetic)

- **Introspection contract** (local mock endpoint, dev-dep axum): active token → full mapping
  (principal chain incl. namespaced account fallback, kind heuristic, roles array *and*
  space-separated, account literal-vs-dot-path, scopes, level clamped to Verified);
  `active:false` / non-access `token_type` / missing principal → `Unauthorized`; 500 / timeout /
  malformed JSON / over-cap body → `Unavailable`.
- **Redirect refusal:** mock returns 307 to a second listener; introspector does not follow,
  returns `Unavailable`, second listener saw nothing.
- **Form injection / edge tokens:** tokens containing `&`, `=`, `%`, unicode, 8 KiB+ — mock asserts
  exactly one `token` form field; over-length / bad-charset rejected `Unauthorized` **without** a
  network call.
- **Cache:** hit within TTL performs no network call; `exp` math (`exp ≤ now`, absent,
  milliseconds) never caches-forever and never panics; negative window observed for a *repeated*
  token; segregation flood — insert a positive, flood 2 000 unique garbage tokens, positive still
  hits; `Unavailable` not cached; capacity eviction.
- **Leak/redaction:** `Debug` for config/authenticator/cache carries no secret and no raw token;
  `Unavailable` wire mapping contains no endpoint URL or backend error text; `WWW-Authenticate` is
  byte-constant across all 401 causes.
- **Reserved prefix:** `account:*` values in a roles claim never reach `caller.groups`; the mirror
  group appears iff the account claim resolved.
- **Seam:** `AuthContext` → `(Caller, Trust)` reaches `flux_policy::evaluate` exactly as
  `OidcIdentity`'s output does (shared projection test).
- **Server (D-69):** indistinguishable-404 (byte-identical for nonexistent vs cross-account, on
  every `/sessions/:id/*` route including `POST …/messages`); NULL-realm (two account-less
  principals can't see each other's sessions; legacy NULL session 404s in principal mode;
  loopback/shared-secret continuity unregressed); realm-keyed `contextId` (same `contextId`, two
  realms → two sessions, in both dispatchers); router audit (every route either realm-guarded,
  account-scoped, or explicitly exempt); card advertises the scheme and derives `url` from the
  configured external base.

## Non-goals / follow-ups

- Local JWT/JWKS validation — a second `RequestAuthenticator` impl later; introspection first
  (revocation-aware by construction, and the reference downstream shape).
- flux-server wiring, per-principal envelope identity, realm-keyed continuity — **D-69**.
- Single-flight introspection dedup; upstream rate limiting (deployment concern, stated in the
  cache section); `protocolVersion` on the agent card.
- Per-account policy grants, encryption-at-rest, an account registry — out, as in D-02.
- The CLI / `LocalIdentity` path is byte-for-byte untouched.

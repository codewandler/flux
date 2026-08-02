---
id: C-459
title: "A secret has no scope — once resolved it can go anywhere the egress guard already allows"
pillar: Core
status: in-progress
priority: 5
design: docs/designs/secrets-the-agent-never-sees.md
epic: secrets-the-agent-never-sees
areas: [flux-system, flux-web]
note: "⚠ CORRECTED after a survey: flux DOES scope which secret may be NAMED (http.request's allowed_secrets, C-76; plugin grants.secrets), it does not scope WHERE a named secret may GO. Two different axes, and only the first exists. Per-principal scoping is newly expressible after C-408/C-415"
---

# Which destinations, and on whose behalf

## Goal

A secret carries its own scope: **which destinations** it may be sent to, and **which principal** may
cause it to be used.

## The two gaps

**1. Destination.** ⚠ **flux already scopes one axis and not the other, and the distinction is the
story.**

*What exists* — flux scopes **which secret may be named**:
- `http.request`'s `$secret` **allowlist** (`crates/flux-web/src/http.rs:50`, enforced in
  `resolve_secret_env` at `:407-413` **before the value is read**), from `[web] allowed_secrets` or
  `FLUX_WEB_SECRET_ALLOW`. C-76's point exactly: a prompt-injected model cannot name
  `AWS_SECRET_ACCESS_KEY` and exfiltrate it in one call.
- Plugin manifest grants: `grants.secrets` is a per-plugin **key list** and `grants.credential` a
  per-plugin boolean, both deny-by-default.
- Cross-plugin use is gated by an operator config grant per `(consumer, provider)` pair, then optional
  first-use approval, then audit — **before** materialization (`broker.rs:665-702`).

*What does not exist* — flux scopes nothing about **where a named secret may go**. `guard_url_scoped`
decides whether *this caller* may reach *this host*; it knows nothing about which secret is in the
request. Once `resolve_in` has substituted plaintext into the settings bag
(`crates/flux-app/src/secrets.rs:52`), that value is an ordinary string usable by anything holding
those settings, bound for any host the caller may already reach.

⚠ Also dangling: `Sensitivity` (`crates/flux-secret/src/lib.rs:124`) is **defined and read by nothing**.
Either it becomes the carrier for this scope or it should go.

Vaults scopes per credential — `networking.allowed_hosts`, described as preventing *"your key from ever
being shared with unauthorized hosts"* — and pairs it with `injection_location` (header, body, or both)
on the reasoning that *"request payloads are often assembled from content the agent is working with, so
the request body is the broader exposure surface."*

**2. Principal.** A vault is *"the collection of credentials associated with an end user"*, referenced
per session. flux has no equivalent — and ⚠ **it newly could**: [C-408](C-408-room-participants-share-one-identity.md)
and [C-415](C-415-a-room-triggered-journey-still-runs-as-the-operator.md) established per-speaker
`TurnIdentity`, so "which principal may use this secret" is expressible where it was not before. On a
shared surface — a room with several humans in it — that is the difference between a credential the
operator holds and a credential anyone in the room can spend.

## Acceptance

- [x] **Failing-first**: a test asserting a destination-scoped secret is refused for an out-of-scope host
      that the caller is otherwise permitted to reach — failing at the merge base.
      → `a_destination_scoped_secret_is_refused_for_an_out_of_scope_host`
      (`crates/flux-web/src/http.rs`). The caller holds `PrivateNetAllow::Any`, so only the secret's own
      scope can refuse the request; the test also pins that nothing reached the wire.
- [x] Destination scope is **default-deny** where declared, and the check happens on the **resolved,
      vetted** address, matching the discipline `guard_target_host_pinned` already enforces. ⚠ A scope
      matched against the pre-resolution hostname is a bypass.
      → `flux_system::secret_scope::Destination::vetted` is the *only* constructor and takes the guard's
      own `(Url, Vec<SocketAddr>)` pair, refusing an empty pin set. `http.request::execute` now guards
      **before** resolving any `$secret`, and the same vetted set becomes the connection's pin, so the
      address authorized is the address dialled. Every redirect hop is re-authorized.
- [x] ⚠ **A secret with no declared scope keeps working.** Breaking every existing `secret "NAME"` to add
      scoping would guarantee nobody adopts it. Unscoped must remain valid and must be *visible* as
      unscoped.
      → a bare `NAME` entry parses to `SecretGrant::is_unscoped`, pinned by
      `an_unscoped_secret_keeps_travelling_wherever_the_caller_may_reach`. Legibility is structural:
      `SecretGrant::describe` names **every** axis including the open ones
      (`PLAIN (unscoped: to any destination; by any principal; in header or query)`) and
      `SecretAllowlist::unscoped_names` reports the unscoped entries. ⚠ See Progress for what this
      *does not* give an operator.
- [x] Principal scope, built on the existing `TurnIdentity` — ⚠ **not a second identity concept.**
      C-415's lesson holds: one constructor, one trust decision.
      → the id matched is `ctx.turn_identity().caller().principal.id`; nothing new is constructed. A turn
      for which no principal was resolved **refuses** a `by=` grant rather than passing as a wildcard
      (`a_principal_scoped_secret_admits_its_principal_and_refuses_every_other_turn`).
- [x] Injection location (header / body) decided — implement it, or record why flux's shape does not
      need it.
      → implemented, on the axis flux actually has. `http.request` resolves a `{"$secret": …}` marker
      **only** in `headers` and in the `query` record — `body` is a plain string with no marker
      resolution — so Vaults' header/body split does not transfer: the body exposure they scope against
      does not exist here. The split that does exist is header vs query, and it matters for the same
      reason: a query-placed credential lands in a URL, which proxies, gateways and access logs keep.
      `in=header|query`, pinned by `a_header_only_secret_is_refused_in_a_query_parameter`.
- [x] Full gate green.

## Notes

- ⚠ Worth doing **independently of [C-458](C-458-substitute-at-egress.md)**: scoping applies to every
  secret whatever the transport, while substitution only fits HTTP-shaped egress. If only one of the two
  ships, this is the one with broader reach.
- Related from the other side: [D-227](D-227-outbound-a-call-is-an-effect-that-costs-money.md)'s
  destination allowlist for outbound calls is the same idea for a different resource — check whether one
  mechanism serves both before building two.

### ⚠ D-227 checked: shared discipline, separate mechanisms — and why

Both stories want the same **shape**, and both should keep it: default-deny, matched *after*
canonicalization, enforced at the guarded send rather than at the point the model proposes a value. That
much is one rule and D-227 should not restate it differently.

They cannot share one *matcher*, and the reason is the part each story calls its own bypass. Here the
canonicalization step is **resolve the hostname and pin the answer** — a name is not a destination until
DNS has been consulted and the result frozen, which is why `Destination::vetted` refuses an empty pin
set. In D-227 it is **dial-plan normalization** — `+49…`, `0049…`, `00 49…` and a prefixed extension are
one destination, and an allowlist matching unnormalized text is "a bypass wearing a whitelist" in that
story's own words. A shared `Vec<String>` host matcher would force the phone side to normalize *outside*
the check, which is precisely the hole D-227 exists to close; a shared abstraction over both would have
to be parameterized by its canonicalizer, at which point it is an interface over two implementations and
buys nothing but a false claim of uniformity.

So: two mechanisms, one discipline. If a third destination-scoped resource appears, that is the moment to
extract the shape — not before, and not by making the phone number pretend to be a host.

### ⚠ `Sensitivity` (`crates/flux-secret/src/lib.rs`) is **not** the carrier, and was not removed

It is an ordinal classification — `Public < Internal < Restricted < Confidential < Secret` — and a scope
is a *set of permitted destinations and principals*. "May go to `api.github.com`" has no rank; encoding
it as one would be a category error that reads as coherent, which is the worse kind. So the scope landed
in `flux_system::secret_scope`, where the vetted-address discipline it depends on already lives.

It was also **not deleted**, deliberately: `codewandler-flux-secret` is a published protocol-line crate
(1.2.0, C-143's independent version), so removing a public type is a breaking change this story does not
sanction and a version decision that belongs with whoever cuts the release. It remains read by nothing.
[C-461](C-461-what-our-secret-model-is.md) — "say what our secret model is" — is where that should be
settled, either by giving it a reader or by retiring it behind a proper bump.

## Progress

- Filed 2026-08-02 from the Vaults comparison.
- **2026-08-02 — implemented (C-459).** `flux_system::secret_scope` is the mechanism; `http.request` is
  the enforcement point. Full gate green including `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test
  --workspace`.

  **What is enforced, and where.** `http.request` is flux's only model-facing path where a *named* secret
  meets a *guarded, address-pinned* egress, so it is the one place the two axes can both be checked and
  the check can mean something. `execute` now runs the egress guard **first**, builds a `Destination`
  from its `(Url, pinned)` pair, authorizes each `$secret` against it, and only then reads the value —
  keeping C-76's ordering (a name the operator never opted in is still refused without touching the
  environment) and adding the scope check behind it. The query is appended *after* the guard rather than
  before; appending cannot move the authority, and the code refuses rather than assuming so. This also
  removes a second DNS resolution the previous ordering implied.

  ⚠ **What is NOT covered, stated plainly.** The other `secret "NAME"` resolution sites are untouched and
  remain unscoped: `flux-app`'s `resolve_in` substitutes plaintext into the settings bag with no
  destination in view at all, and `flux-channels`' adapter resolution does the same for XMPP/SIP
  credentials. Scoping those needs a destination to exist at the moment of resolution, which is a
  different design (and is what [C-458](C-458-substitute-at-egress.md)'s substitution seam would create).
  A `to=` scope on such a secret would have nothing to match and must not be offered until it does.

  **Known limits of the destination match**, none of them silent:
  - **Host, not port.** `to=127.0.0.1` admits every port on that host, because `host_matches` is the
    guard's own vocabulary and a second one would let a scope and a private-net grant disagree about a
    hostname. A per-port scope is a real gap for a multi-tenant loopback surface.
  - **Only a value flux was told is a secret.** A credential pasted into a plain string header is not a
    `$secret` marker, so nothing here sees it. That is the containment/prevention gap the epic is about.
  - **Redirects are refused conservatively.** The whole chain must stay in scope, even though
    `send_guarded` already clears headers cross-origin — reasoning per-hop about which bytes survive is
    how this class of check goes wrong. The failure direction is a refused redirect.

---
title: Server authentication & tenancy
description: How a networked flux authenticates inbound callers — Open, shared-secret, and OIDC principal modes — and how it isolates tenants into disjoint realms.
---

# Server authentication & tenancy

When you expose flux as a **server** (for example to drive it over HTTP or from an agent-to-agent
channel), you need to control **who** may call it and keep tenants apart. This page covers that
inbound direction. Authentication only establishes *who* a caller is; authorization of what they may
then do stays with the one [safety envelope](../agent/safety.md) — the authenticator never makes a
policy decision.

## Authentication modes

A flux server runs in one of three modes:

| Mode | How a caller proves identity | Notes |
|---|---|---|
| **Open** | Nothing | Refused on a non-loopback bind — you cannot accidentally expose an unauthenticated server to the network |
| **Shared secret** | A bearer token compared in **constant time** | Simple single-tenant setups |
| **Principal** | A bearer token validated by OIDC introspection | Multi-tenant, identity-provider-backed |

Both authenticated modes reject a request that carries **duplicate `Authorization` headers** (a
request-smuggling guard), and the bearer is length- and charset-checked before any work happens.

## Principal mode (OIDC)

In principal mode, flux validates each bearer token against your identity provider using RFC 7662
token introspection, then projects the returned claims into a caller identity at a **single mapping
point**. The security properties that matter:

- **Trust ceiling.** A token presented over the network can never mint more than `Verified` trust.
  The elevated `Privileged` / `System` levels are reachable only by the local machine owner, never
  derived from a claim.
- **Only real access tokens pass.** Introspection requires `active: true` and **rejects non-access
  token types** — a leaked refresh, logout, or id token can't be replayed as a working bearer.
- **No oracles.** Every rejection returns a byte-constant `WWW-Authenticate` challenge and a constant
  body; a `401` never leaks *why* it failed. Any diagnostic detail is log-only.
- **Hardened introspection call.** The introspection client **never follows redirects** (so a
  compromised endpoint can't replay your live bearer to a `Location`), caps the response size, and
  requires HTTPS unless explicitly told otherwise.
- **Cached by token hash.** Validated results are cached keyed by **SHA-256 of the token** — raw
  bearers never sit in a map — with positive and negative results segregated so a garbage-token flood
  can't evict live sessions.

## Tenant isolation with realms

Multi-tenant flux keeps callers apart with **realms**. Every caller resolves to a realm key in one of
two **disjoint namespaces**:

- `acct:<account>` — for a caller carrying a tenant/account claim,
- `user:<principal>` — for an account-less caller.

Because the namespaces can't overlap, an identity provider that emitted `account = "user:victim"`
still can't collide with a principal literally named `victim`. Sessions are tagged with their realm,
and every `/sessions/:id/*` route is wrapped by a guard that returns the **same constant `404`** for
a session in another realm as for one that doesn't exist — so a caller can't even probe which session
ids are in use in another tenant.

> `account` is a **tenancy/storage key only** — the authorization policy never consults it. Account-
> conditional access works through a reserved, authenticator-only group, not by treating the account
> as a policy input.

## Hardened deployment profile

For a server holding credentials for many tenants, combine principal mode with the Vault
[credential-store backend](./credentials.md#storage-backends): callers are authenticated per request
and clamped to `Verified`, tenants are isolated into disjoint realms, and per-customer tokens live in
Vault rather than a file on the pod. Bind and permission settings live in
[Configuration](../reference/config.md).

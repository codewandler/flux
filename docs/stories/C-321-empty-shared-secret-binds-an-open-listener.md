---
id: C-321
title: "An empty shared secret is functionally `Open`, and it walks past the guard that refuses `Open`"
pillar: Core
status: done
areas: [flux-server, flux-channels]
note: "security · the third live instance of C-317's bypass, found by C-317's implementor and confirmed link-by-link — `Some(\"\")` becomes SharedSecret{secret:\"\"} rather than Open, authenticates every request including one with no header, and guard_open_bind keys on Open ONLY, so it binds 0.0.0.0 unchallenged against the auto-approving daemon"
---

# An empty shared secret is functionally `Open`, and it walks past the guard that refuses `Open`

## Goal

Close the third live instance of the bypass D-216 and C-317 each closed in their own adapter. This
one is the worst of the three, because it ends at a public bind rather than at a single channel.

The chain, verified link by link on the post-C-317 tree:

1. `crates/flux-channels/src/adapters/a2a.rs:92` passes the `[a2a]` channel's `token` straight into
   `flux_server::ServerAuth::shared_secret` with **no empty filter**. The two CLI producers
   (`crates/flux-cli/src/app_cmd.rs:486-488` and `:671-673`) both spell
   `.ok().filter(|t| !t.is_empty())`; this producer does not.
2. `crates/flux-server/src/lib.rs:89-96` — `shared_secret` is a bare `match` on `Option`, so
   `Some("")` becomes `SharedSecret { secret: "" }`. Only `None` becomes `Open`.
3. `crates/flux-server/src/lib.rs:1155-1156` — `presented` is `""` when no `Authorization` header is
   sent, and `constant_time_eq(b"", b"")` is **true**. Every request authenticates, including one
   carrying no header at all.
4. `crates/flux-server/src/lib.rs:519` — `guard_open_bind` is
   `matches!(auth, ServerAuth::Open) && !unauthenticated_bind_allowed(addr)`. A
   `SharedSecret { secret: "" }` is **not** `Open`, so the guard does not fire and the router binds
   a non-loopback address unchallenged.

**The doc comment immediately above that guard is what makes this a defect rather than a gap.** It
states: *"`ServerAuth::Open` on a non-loopback bind is remote code execution against the
auto-approving daemon, so it is refused outright — there is deliberately no escape hatch."* An empty
shared secret is functionally `Open`. It is the escape hatch that comment says does not exist, and
it defeats the safety invariant at `AGENTS.md:114` verbatim.

Reachability is the same as C-317's: `token secret "K"` in a channel declaration with `K` exported
empty. An operator who exports an empty or unset-to-empty variable gets a listener they believe is
authenticated.

## Acceptance

- [x] **Failing-first, at the bind level, not only at the compare.** A test showing that an `[a2a]`
      channel (or a server built through the same path) with an empty token **binds a non-loopback
      address** today. A test that only asserts the 401 would leave link 4 — the actual severity —
      unobserved.
- [x] **A second failing-first test for the request half**: a request with no `Authorization` header
      is authenticated by an empty-secret server today.
- [x] **Decide where the refusal belongs and say why.** There are three candidate layers — the
      producer (`a2a.rs`, matching the CLI's `.filter(|t| !t.is_empty())`), the constructor
      (`shared_secret`, so *every* producer inherits it), and the guard (`guard_open_bind`, so the
      public-bind refusal stops keying on a variant that no longer captures the property it means).
      **Fixing only the producer leaves the next caller of `shared_secret` to rediscover this** —
      that is exactly how this became the third instance. State what you chose and what you rejected.
- [x] **Mutation-test the wiring**: revert each guard line individually on the *shipped* code and
      confirm a named test changes. A ticked box no test observes is this repo's recurring defect
      class, and this story is the third appearance of one specific instance of it.
- [x] Grep every producer of `ServerAuth` and every `constant_time_eq` call site once more, and list
      them with their empty-token status. C-317 found three instances by doing this; the point of
      this item is that the list ends up in the story rather than in an agent's context.
- [x] `guard_open_bind`'s doc comment is corrected or its claim is made true. It currently promises
      no escape hatch.
- [x] Full gate green in both workspaces.

## Notes

- **Found by [C-317](C-317-empty-bearer-token-authenticates.md)'s implementor**, which fixed the
  webhook adapter and correctly refused to reach outside its own fence into `flux-server`. The chain
  above was re-verified independently at file:line before this story was filed.
- The two already-closed instances are the reference fix shape: [D-216](D-216-connector-channel-arm.md)
  (connector adapter — `crates/flux-channels/src/adapters/connector.rs:647-655`, refused at load,
  loopback included) and [C-317](C-317-empty-bearer-token-authenticates.md) (webhook adapter — refused
  at `from_decl` plus an empty guard inside `authorized`). Both refuse in **two** places on purpose,
  so neither half silently carries the other.
- ✅ **Correction (coordinator, at integration).** This story originally asserted that `flux-server`
  is a **published** crate and that a version decision was therefore owed. That was wrong, and it was
  my error, not the implementor's — `crates/flux-server/Cargo.toml` is `name = "flux-server"` and
  neither it nor `flux-channels` appears in `scripts/publish-crates-io.sh`'s `codewandler-flux-*`
  closure. `scripts/check-crate-versions.sh` reports `PASS 0 changed crate(s)`. The implementor
  checked rather than taking the claim on trust, reported it, and did not price a bump that was not
  owed; the reviewer confirmed it independently. The behavioural break is real but **in-tree only**:
  an `[a2a]` channel with `token ""` now fails at load, and `router()`/`router_multi()` now `Err` for
  an empty secret on a non-loopback bind.
- Related: `unauthenticated_bind_allowed` is the other half of link 4 and is worth reading before
  choosing a layer; a fix that makes an empty secret `Open` would route it into that helper's
  loopback allowance rather than into a hard refusal, which may or may not be what you want.

## Progress

**Done** on `impl/C-321`, merge base `47424ad9`. Three refusals in three independent places, each
one individually mutation-tested.

### The layer decision

The refusal went in **three** places, not one — matching D-216/C-317's "neither half silently
carries the other" doctrine, extended by one because this chain crosses a crate boundary:

1. **The guard** — `guard_open_bind` (`crates/flux-server/src/lib.rs`) now tests
   `ServerAuth::is_effectively_open()` instead of `matches!(auth, ServerAuth::Open)`. The new
   predicate is `Open`, **or** `SharedSecret` with an empty secret. This is the primary fix: it is
   the single construction-time enforcement point that `router`, `router_multi`, `serve`, `serve_on`,
   `serve_multi`, `serve_multi_on` and the `a2a` channel's own `axum::serve` mount all share (C-190),
   so **every** producer of `ServerAuth` inherits the refusal without touching the constructor. It
   also makes the doc comment's "there is deliberately no escape hatch" true, and states the property
   once so a future mode that is open-in-effect is caught without anyone remembering to extend a
   `matches!`.
2. **The compare** — `require_auth` returns `unauthorized()` when the expected secret is empty,
   before the constant-time compare, exactly as `authorized()` does in `webhook.rs` and
   `connector.rs`. This is the half that holds on a **loopback** bind, which the guard admits by
   design, and the half that survives a future path reaching the middleware without going through
   `router`.
3. **The producer** — `a2a_auth_from_settings` (`crates/flux-channels/src/adapters/a2a.rs`) bails on
   an empty-or-whitespace `token`, on loopback too, mirroring both reference adapters verbatim. This
   is the only half that produces an *actionable config error* naming the channel, before a port is
   bound, rather than a listener that 401s everything.

**Rejected — changing `ServerAuth::shared_secret` to return `Result`.** It is the "every producer
inherits it" option the story names, but it is a breaking signature change on a crate's public API,
and it is not needed: keying the guard on the property gives every producer the same inheritance
through the enforcement point that already exists. (`flux-server` turns out **not** to be in the
crates.io publish closure — `scripts/publish-crates-io.sh` ships only the `codewandler-flux-*`
names — so the "published crate" caveat in the Notes above is inaccurate. The break is still real
for in-tree callers and for anyone with `token ""`, just not a registry break.)

**Rejected — normalising `Some("")` → `Open` inside `shared_secret`.** Silent, exactly as D-216 and
C-317 each argued; it would ship an operator a config they believe is authenticated. And as this
story's own Notes predicted, it would route the case into `unauthenticated_bind_allowed`'s loopback
allowance instead of a hard refusal, so a loopback empty-secret server would keep pass-through-
admitting anonymous requests while printing as "shared-secret".

**Rejected — fixing only the producer.** That is how this became the third instance.

### Every `ServerAuth` producer, with its empty-token status

| Site | Empty-token status |
| --- | --- |
| `flux-cli/src/app_cmd.rs:671-673` (`server_auth_from_config`) | Safe — `.ok().filter(\|t\| !t.is_empty())` on `FLUX_SERVER_TOKEN` |
| `flux-cli/src/app_cmd.rs:486-488` (the `--serve` synthesized `a2a` decl) | Safe — same filter, then routed through the adapter below |
| `flux-cli/src/app_cmd.rs:727` (`ServerAuth::Principal`) | N/A — no shared secret |
| `flux-channels/src/adapters/a2a.rs` (`a2a_auth_from_settings`) | **Was the hole.** Fixed here — refused at load |
| `flux-server/src/lib.rs` `from_token` / `shared_secret` | Infallible by design; empty now refused downstream at the guard and the compare |
| `flux-server/tests/support/mod.rs:156`, `src/a2a.rs:2319`, `src/resource.rs:454`, the `ServerAuth::Open` literals in `tests/*` | Test fixtures — `None` or a real token |

### Every `constant_time_eq` call site

| Site | Empty-expected guard |
| --- | --- |
| `flux-server/src/lib.rs:1225` (`require_auth`, shared secret) | **Was unguarded.** Now `if secret.is_empty() { return unauthorized(); }` immediately above |
| `flux-channels/src/adapters/webhook.rs:168` (`authorized`) | Guarded — `if expected.is_empty() { return false }` (C-317) |
| `flux-channels/src/adapters/connector.rs:973` (`authorized`) | Guarded — same shape (D-216) |
| `flux-server/src/lib.rs:1712-1714`, `webhook.rs:249` | Unit tests of the primitive itself |

No fourth instance found. The connector's `verification.kind = "hmac"` path is refused at load
(flux has no HMAC verifier), so there is no second secret-comparison family to audit.

### Mutation test (each guard reverted individually on the shipped code)

| Reverted | Test that turned red | Tests that stayed green |
| --- | --- | --- |
| `guard_open_bind` → `matches!(auth, ServerAuth::Open)` | `empty_shared_secret_is_effectively_open`, `an_empty_shared_secret_may_not_bind_a_public_address` | `an_empty_shared_secret_authenticates_nothing` |
| dropped `if secret.is_empty()` in `require_auth` | `an_empty_shared_secret_authenticates_nothing` (200 vs 401) | both bind tests |
| dropped the `a2a_auth_from_settings` bail | `adapters::a2a::tests::empty_token_is_refused_at_load` | all flux-server tests |

Each half is observed by a test the others do not cover, so no half silently carries another.

---
id: C-321
title: "An empty shared secret is functionally `Open`, and it walks past the guard that refuses `Open`"
pillar: Core
status: ready
priority: 1
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

- [ ] **Failing-first, at the bind level, not only at the compare.** A test showing that an `[a2a]`
      channel (or a server built through the same path) with an empty token **binds a non-loopback
      address** today. A test that only asserts the 401 would leave link 4 — the actual severity —
      unobserved.
- [ ] **A second failing-first test for the request half**: a request with no `Authorization` header
      is authenticated by an empty-secret server today.
- [ ] **Decide where the refusal belongs and say why.** There are three candidate layers — the
      producer (`a2a.rs`, matching the CLI's `.filter(|t| !t.is_empty())`), the constructor
      (`shared_secret`, so *every* producer inherits it), and the guard (`guard_open_bind`, so the
      public-bind refusal stops keying on a variant that no longer captures the property it means).
      **Fixing only the producer leaves the next caller of `shared_secret` to rediscover this** —
      that is exactly how this became the third instance. State what you chose and what you rejected.
- [ ] **Mutation-test the wiring**: revert each guard line individually on the *shipped* code and
      confirm a named test changes. A ticked box no test observes is this repo's recurring defect
      class, and this story is the third appearance of one specific instance of it.
- [ ] Grep every producer of `ServerAuth` and every `constant_time_eq` call site once more, and list
      them with their empty-token status. C-317 found three instances by doing this; the point of
      this item is that the list ends up in the story rather than in an agent's context.
- [ ] `guard_open_bind`'s doc comment is corrected or its claim is made true. It currently promises
      no escape hatch.
- [ ] Full gate green in both workspaces.

## Notes

- **Found by [C-317](C-317-empty-bearer-token-authenticates.md)'s implementor**, which fixed the
  webhook adapter and correctly refused to reach outside its own fence into `flux-server`. The chain
  above was re-verified independently at file:line before this story was filed.
- The two already-closed instances are the reference fix shape: [D-216](D-216-connector-channel-arm.md)
  (connector adapter — `crates/flux-channels/src/adapters/connector.rs:647-655`, refused at load,
  loopback included) and [C-317](C-317-empty-bearer-token-authenticates.md) (webhook adapter — refused
  at `from_decl` plus an empty guard inside `authorized`). Both refuse in **two** places on purpose,
  so neither half silently carries the other.
- ⚠ `flux-server` is a **published** crate. Refusing a previously-accepted configuration at
  construction is a behavioural break at load time and needs to be priced accordingly — the same
  call C-317 flagged for its own, smaller blast radius.
- Related: `unauthenticated_bind_allowed` is the other half of link 4 and is worth reading before
  choosing a layer; a fix that makes an empty secret `Open` would route it into that helper's
  loopback allowance rather than into a hard refusal, which may or may not be what you want.

---
id: C-317
title: "An empty bearer token authenticates every request on a webhook channel's public port"
pillar: Core
status: done
areas: [flux-channels]
note: "FIXED — found by D-216's review in the new connector arm; the identical hole was pre-existing in the webhook adapter. `constant_time_eq(b\"\", b\"\")` is true and the guard only tested is_none(), so a request with no Authorization header at all authenticated. Refused now at load AND at the comparison. The sweep that closed this found a THIRD instance in flux-server → C-321"
---

# An empty bearer token authenticates every request

## Goal

`crates/flux-channels/src/adapters/webhook.rs:40` and `:88-97` carry an authentication bypass that
is **shipped and live**, not hypothetical.

The chain:

1. The token is an `Option<String>`. `token ""` yields `Some("")` — and so does `token secret "K"`
   where `K` is exported empty, because `crates/flux-app/src/secrets.rs:37` calls `std::env::var`
   with no empty filter. An operator does not have to write `""` to get here; an unset-looking
   environment variable that is actually set-and-empty is enough.
2. The non-loopback guard tests only `is_none()`. `Some("")` sails through it, so the bind is
   permitted on a public interface.
3. The handler reads `headers.get(AUTHORIZATION)…strip_prefix("Bearer ").unwrap_or("")` and compares
   with `constant_time_eq`. Equal lengths, empty loop, `diff == 0` → **true**.

So a request carrying **no `Authorization` header at all** authenticates. This is an open listener on
a host that auto-approves tools, which `AGENTS.md:110` names in as many words: "The daemon
auto-approves tools — an open listener is RCE."

The rest of the codebase already knows the correct shape. `crates/flux-cli/src/app_cmd.rs:486-488`
and `:671-673` both spell `.ok().filter(|t| !t.is_empty())`, and
`crates/flux-server/src/lib.rs:519-524` then *refuses* the resulting `Open` auth on a non-loopback
bind. The webhook adapter has neither half.

## Acceptance

- [x] **Failing-first, two tests, and they must be separate.** One proves a request with no
      `Authorization` header is rejected by a channel configured with an empty token. One proves the
      channel with an empty token **refuses to bind** on a non-loopback address in the first place.
      Both halves exist in `flux-server`'s precedent and both are needed: the comparison fix alone
      still leaves an operator believing an empty token is a token.
- [x] The set-but-empty environment variable path is covered — `token secret "K"` with `K=""` — not
      only the literal `token ""`. That is the spelling an operator reaches by accident.
- [x] Decide whether an empty token is a **load error** or is normalised to `None` and then refused
      by the existing no-token rule. Either is defensible; say which and why. A load error is more in
      keeping with the channels' "refuse everything refusable before a port is bound" thesis.
- [x] Grep every other `constant_time_eq` / bearer comparison in the tree and account for each one.
      This story exists because the same mistake was made twice independently; a third instance is
      more likely than not.
- [x] Full gate green in both workspaces.

## Notes

- Found by the D-216 review (2026-07-31) in the **new** connector arm, where it is blocking and is
  being fixed under that story. The connector fix is deliberately fenced to that adapter so this one
  gets its own failing-first test and its own review rather than riding along.
- Priority 1 because it is live on main and is an authentication bypass on a public port. It is
  mitigated only by the fact that reaching it requires an operator to configure a webhook channel
  with an empty token — which the set-but-empty environment path makes more reachable than it sounds.
- Related: [D-216](D-216-connector-channel-arm.md) is where it was found.
- `constant_time_eq` itself is **not** at fault and should not be changed: it is a correct pure
  comparison with a length pre-check. Two empty strings genuinely are equal. The defect is that an
  empty expected-token was ever allowed to reach it.

## Progress

Fixed in `crates/flux-channels/src/adapters/webhook.rs`, mirroring D-216's landed connector fix.

**Load error, not normalisation to `None`.** An empty token is refused in `from_decl` — and refused
on a **loopback** bind too. Normalising to `None` would be silent: on loopback it is not even an
error, so the operator ships a channel they believe is authenticated, one `addr` edit away from
being public. Refusing says the thing that is wrong, at the moment it is fixable. This matches the
connector arm exactly (`connector.rs:647-655`), so the two adapters do not drift. `trim().is_empty()`,
so `" "` and `"\t\n"` count as empty.

**Two halves, independently attributable.** The comparison moved into a standalone
`fn authorized(expected: Option<&str>, headers: &HeaderMap) -> bool` that returns `false` on an
empty expected token *before* comparing. Extraction is what makes the halves testable apart: once
the constructor makes `Some("")` unreachable, a test routed through `from_decl` can only cover one
of them. Verified by disabling each condition alone — with only `authorized`'s empty check disabled
the request test reds and the bind tests stay green; with only the load-time refusal disabled the
bind tests red and the request test stays green.

`constant_time_eq` was left untouched, as the story directs.

**Base proof** (merge base `819e35d5`, base implementation with both tests grafted on, own target dir):
`a_request_with_no_authorization_header_is_rejected_by_an_empty_token_channel` reds with
`left: 200, right: 401` — an anonymous request authenticating is the bypass itself, observed;
`an_empty_token_is_refused_before_a_port_is_bound` and `a_set_but_empty_secret_env_var_is_refused_too`
red with "an empty token must be refused" on both the loopback and the `0.0.0.0` bind.

Full gate green in both workspaces: `cargo test --workspace` (187 suites), `clippy --all-targets
-D warnings`, `fmt --all --check`, `cargo test -p flux-codegate` (26), and `fmt --check` on
`plugins/Cargo.toml`.

**A third instance exists and is NOT fixed here** (out of this story's fence — it is in
`flux-server`, not `flux-channels`, and warrants its own story + review):
`crates/flux-channels/src/adapters/a2a.rs:92` passes the `[a2a]` channel's `token` straight into
`flux_server::ServerAuth::shared_secret` with **no empty filter**, unlike the two CLI producers
(`flux-cli/src/app_cmd.rs:486-488` and `:671-673`, which both spell `.filter(|t| !t.is_empty())`).
`ServerAuth::shared_secret` (`flux-server/src/lib.rs:89-95`) is a bare `match`, so `Some("")` becomes
`SharedSecret { secret: "" }`; the compare at `flux-server/src/lib.rs:1155` has no empty guard; and
`guard_open_bind` (`:520`) keys on `ServerAuth::Open` only, so `SharedSecret { secret: "" }` binds
`0.0.0.0` unchallenged. Same bypass, same auto-approving daemon.

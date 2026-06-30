# Design: scoped private-network egress (D-20)

**Status:** proposed · **Pillar:** Core · **Layer:** L2 (`flux-system` net guard) + L4 (`flux-plugin` host
caps) + L0 (`flux-config`) + L6 wiring (`flux-cli`) · **Owner:** Timo ·
**Story:** [D-20](../stories/D-20-scoped-private-net-egress.md)

## Why

flux's SSRF guard (`flux_system::net::guard_url` / `dial`) refuses private, loopback, link-local, unique-local,
and CGNAT addresses — defense against an attacker-influenced URL reaching cloud metadata (`169.254.169.254`),
in-cluster services, or `localhost`. The **only** escape hatch today is a single global boolean,
`Config.allow_private_net` (`crates/flux-config/src/lib.rs:36`), merged from user + project config and wired
into both egress callers:

- `web_fetch` — `crates/flux-cli/src/main.rs:957` (`WebFetchTool::allow_private`)
- the plugin host — `crates/flux-cli/src/main.rs:976` (`flux plugin call`) and `:3335` (`flux app run`)
  (`SystemHostCaps::allow_private_net`)

That boolean is all-or-nothing. To let one trusted internal GitLab through, the operator must flip the guard
**off for every fetch in the process** — re-opening metadata endpoints and the whole RFC-1918 range to
`web_fetch` and every other plugin. The integration pack (gitlab/jira/confluence/prometheus/loki/kubernetes) is
precisely the surface that points at internal infra, so in any real enterprise deployment the pack is either
unusable (guard on) or the SSRF protection is globally defeated (guard off). `scripts/smoke-plugins.sh` shows
the failure mode: an internal GitLab host resolves to a private address and is refused.

**This is not a bypass-path story.** AGENTS.md's invariant ("there are no bypass paths; don't add one") stays
intact: we are making the *existing* envelope finer-grained, keeping deny-by-default, and routing every
allowance through declaration → grant → audit — exactly the model `PluginCapabilities` already uses for
`process` / `conn` / `secrets`.

## The model

Replace one global bool with a **scoped allow-set** resolved per egress caller. Two granularities, both
deny-by-default:

1. **Per-plugin** — "this plugin may reach the private hosts it declares." Anchored on the existing
   `PluginManifest.endpoints` (the base URLs the host resolves from env) plus a new opt-in flag.
2. **Per-endpoint/host** — "this plugin may reach *these* declared hosts privately" (a subset of its endpoints),
   for when a plugin legitimately talks to both public and internal backends.

A request to a private address is permitted **iff** the calling plugin declared that host (or declared
private-net broadly) **and** the operator granted it. `web_fetch` is its own scope — a plugin's allowance never
widens `web_fetch`, and the `web_fetch` allowance never widens a plugin. Any undeclared private host stays
refused even while some other allowance is active.

### Shape (sketch — finalize during implementation)

`flux_system::net` stops taking a bare `bool`:

```rust
/// What a single egress caller is allowed to reach beyond public addresses.
pub enum PrivateNetAllow {
    None,                       // default — full SSRF guard (today's behaviour with the flag off)
    Hosts(Vec<HostPattern>),    // only these declared hosts may resolve to private addrs
    Any,                        // caller-wide (e.g. an explicit `web_fetch` opt-in) — still per-caller
}

pub fn guard_url(raw: &str, allow: &PrivateNetAllow) -> Result<url::Url>;
```

`PluginCapabilities` gains a declared private-net request, mirroring `conn`'s allow-list idiom:

```rust
pub struct PluginCapabilities {
    // … process / secrets / http / conn / blob …
    /// Declared hosts this plugin may reach at private/loopback addresses (empty = none).
    /// `*` segment wildcard like `conn`. Subset of / cross-checked against `endpoints`.
    #[serde(default)]
    pub private_hosts: Vec<String>,
}
```

`Config` replaces the scalar with a grant the operator writes per plugin (and a separate `web_fetch` opt-in),
e.g.:

```toml
# ~/.flux/config.toml
[private_net]
web_fetch = false                          # web_fetch stays guarded
gitlab    = ["gitlab.internal.example"] # this plugin, these declared hosts only
prometheus = true                           # whole plugin: all hosts it declares
```

The host resolves, per plugin, the intersection of *declared* (`private_hosts` / `endpoints`) and *granted*
(config) into a `PrivateNetAllow`, and passes it to `guard_url`/`dial`. Nothing undeclared or ungranted is
reachable.

## Cutover (no parallel semantics)

Per the no-fallbacks rule, the global `allow_private_net` boolean does not coexist with the scoped model. Pick
one at implementation:

- **Preferred:** remove the global scalar; migrate it to `[private_net].web_fetch = true` + per-plugin grants.
  A bare legacy `allow_private_net = true` is read once with a deprecation note mapping it to "web_fetch = Any";
  it never widens plugins.
- The three wiring sites (`main.rs:957/976/3335`) each resolve their own caller-scoped `PrivateNetAllow`.

## Audit

Reaching a private host is a security-relevant event. When `guard_url` admits a private address under a grant,
the host emits a `flux-events` record (plugin, host, grant source) so an operator can answer "what internal
addresses did flux reach, and under whose grant." DNS-rebinding caveat from `net.rs` is unchanged (this is
defense-in-depth, not a TOCTOU fix) and stays documented.

## Smoke

`scripts/smoke-plugins.sh` today reports the guard refusal as a flat `FAIL` and exits non-zero. After this:

- Distinguish "egress guard refused an internal host (no scoped grant)" from a genuine op error — the former is
  an informative `SKIP`/note, not a red `FAIL`, when no allowance is configured.
- When a scoped grant *is* configured (the smoke can set one in its isolated `$HOME`), an internal GitLab is a
  real `PASS`.

## Out of scope

- TLS termination / cert pinning for internal hosts (orthogonal; `conn.dial` TLS is already deferred in D-12).
- A general per-tool network ACL beyond private-net (this story is scoped to the SSRF private-range decision).
- DNS-rebinding TOCTOU hardening (acknowledged limitation, unchanged).

## Touch points

- `crates/flux-system/src/net.rs` — `guard_url`/`dial`/`guard_target_host` take `&PrivateNetAllow`.
- `crates/flux-plugin/src/lib.rs` — `PluginCapabilities.private_hosts`; `SystemHostCaps` resolves the scoped
  allow per plugin from manifest ∩ config; replaces `allow_private_net: bool`.
- `crates/flux-config/src/lib.rs` — `[private_net]` grant shape; merge semantics (project refines user); legacy
  scalar migration.
- `crates/flux-cli/src/main.rs:957,976,3335` — the three egress wiring sites resolve caller-scoped allows.
- `crates/flux-events` — the private-egress audit record.
- `scripts/smoke-plugins.sh` — guard-refusal vs. failure distinction + a scoped-grant exercise path.

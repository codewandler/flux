---
id: D-20
title: Scope private-network egress to declared plugins/endpoints
pillar: Core
status: backlog
priority:
design: docs/designs/scoped-private-net-egress.md
---

# Scope private-network egress to declared plugins/endpoints

## Goal
Replace the global, all-or-nothing `allow_private_net` switch with a **narrowly-scoped, declared, audited**
private-network allowance, so flux can reach trusted internal infra (in-cluster Prometheus/Loki/Kubernetes
API, internal GitLab, Jira/Confluence Data Center) **without disabling SSRF protection for everything else**.
This is a safety-envelope refinement, not a bypass: private addresses are reachable only for hosts a plugin
**declares** and the user **grants**; everything undeclared stays refused by default.

## Why
Today the only way to reach a private/loopback/link-local address is `allow_private_net = true` in
`~/.flux/config.toml` or project `.flux/config.toml` (`crates/flux-config/src/lib.rs:36`), which is wired into
**both** the web-fetch tool and the plugin host (`crates/flux-cli/src/main.rs:957,976,3335`). It is a single
global boolean: flipping it on to reach one trusted internal GitLab simultaneously re-opens `169.254.169.254`
(cloud metadata) and the entire RFC-1918 range to **any** attacker-influenced URL — including `web_fetch` and
every other plugin. The `scripts/smoke-plugins.sh` gitlab case (an internal host resolving to a private
address and being refused) is the symptom: the integration pack is effectively unusable against the enterprise infra it targets
unless the operator accepts that global blast radius.

## Acceptance
- [ ] **Per-plugin and per-endpoint granularity.** An allowance can be granted to a whole plugin (every host
      it declares) **or** to specific declared endpoints/hosts of a plugin. A fetch/dial to a private host that
      is **not** declared-and-granted is still refused **even when another allowance is active** — proven by a
      failing-first test (`net::scoped_allow_does_not_widen_to_undeclared_private_host`).
- [ ] The global `allow_private_net` no longer silently widens plugin egress: either it is scoped to
      `web_fetch` only, or (preferred) it is superseded by the scoped model with a clear migration. Document the
      chosen cutover (no parallel old+new semantics).
- [ ] The allowance flows through the existing envelope: declared in the plugin manifest (anchored on the
      `PluginManifest.endpoints` / a new private-net capability flag), granted via config/policy, and **audited**
      (an event records that a private host was reached under which grant).
- [ ] `web_fetch` is unaffected by a plugin's allowance and vice-versa (no cross-contamination between the two
      egress callers that share `flux_system::net::guard_url`).
- [ ] Smoke ergonomics: `scripts/smoke-plugins.sh` distinguishes "egress guard refused an internal host" from a
      genuine op failure, and can exercise an internal host when a scoped allow is configured (so an internal
      GitLab is a real PASS, not a blanket FAIL or a silently-skipped case).
- [ ] Gate green across both workspaces: `cargo test -p flux-system -p flux-plugin -p flux-config -p flux-cli`,
      clippy `-D warnings`, fmt, `flux-codegate` layering lint.

## Progress
- (not started — design-first; this touches the SSRF policy, so the design lands and is reviewed before code.)

## Notes
- **Honors AGENTS.md "there are no bypass paths."** This does not add a path that skips the envelope; it makes
  the *existing* envelope finer-grained. Deny-by-default is preserved — the default with no grant is exactly
  today's behavior.
- Touch points: `crates/flux-system/src/net.rs` (`guard_url`/`dial` take a scoped allow-set, not a bare bool);
  `crates/flux-plugin/src/lib.rs` (`PluginCapabilities` + `PluginManifest.endpoints`; `SystemHostCaps` resolves
  the per-plugin/per-endpoint allow-set instead of one `allow_private_net` flag); `crates/flux-config/src/lib.rs`
  (`allow_private_net` → a scoped grant shape); `crates/flux-cli/src/main.rs:957,976,3335` (the three wiring
  sites); `flux-events` for the audit record.
- Reuse the deny-by-default precedent already set by `PluginCapabilities` (`process`/`conn`/`secrets` are
  declared allow-lists; private-net should look the same).
- Design: [scoped-private-net-egress.md](../designs/scoped-private-net-egress.md). Surfaced by the
  C-02/D-08 plugin pack + the `smoke-plugins.sh` gitlab case against internal infra.

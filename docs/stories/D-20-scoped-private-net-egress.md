---
id: D-20
title: Scope private-network egress to declared plugins/endpoints
pillar: Core
status: done
design: docs/designs/scoped-private-net-egress.md
note: "finished as the endpoint-epic Phase-2 prereq: the 0.2.7 scoped model gained **per-endpoint** grants (`PrivateNetConfig.endpoints`) and a **private-net-admit audit event** (`PrivateNetAdmit` via the `flux_plugin::EgressAudit` seam + a flux-cli event-store impl)"
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
- [x] **Per-plugin and per-endpoint granularity.** Per-plugin grants shipped in 0.2.7
      (`PrivateNetConfig.plugins`); per-endpoint grants added here (`PrivateNetConfig.endpoints`, keyed
      `"<plugin>:<endpoint>"`, merged with the plugin-level grant via `endpoint_private_hosts`). An
      undeclared private host stays refused even under another active allowance (the `PrivateNetAllow`
      intersection logic + `net` guard tests).
- [x] The global `allow_private_net` is superseded by the scoped model (0.2.7 cutover —
      `PrivateNetAllow::from_legacy_bool` bridges, no parallel semantics).
- [x] The allowance flows through the existing envelope and is **audited**: a new
      `EventKind::PrivateNetAdmit { caller, host, grant_source }` is emitted whenever the host admits a
      private/internal address under a scoped grant — via a `flux_plugin::EgressAudit` seam (no
      flux-plugin→flux-events dep), with the flux-events-backed impl wired at the `flux-cli` surface.
- [x] `web_fetch` is unaffected by a plugin's allowance and vice-versa (separate scoped allow-sets).
- [~] Smoke ergonomics in `scripts/smoke-plugins.sh` — **deferred**: that file currently carries an
      unrelated in-progress change from another session, so it is left untouched (the global
      "never discard uncommitted changes" rule). Pick up once that WIP lands.
- [x] Gate green: `cargo test -p flux-system -p flux-plugin -p flux-config -p flux-events -p flux-cli`,
      clippy `-D warnings`, fmt, `flux-codegate`, full workspace build.

## Progress
- **Done** (pulled into the endpoint-discovery epic as the Phase-2 prerequisite for D-27). The scoped
  model shipped in 0.2.7; this finished the two remaining pieces — per-endpoint grant granularity
  (`flux-config`) and the private-net-admit audit event (`flux-events` `PrivateNetAdmit` + the
  `flux-plugin` `EgressAudit` seam + the flux-cli `EventStoreEgressAudit` impl). Tests:
  `flux-config::per_endpoint_grant_merges_with_plugin_level`, `flux-plugin::egress_audit_fires_on_private_admit_only`.
- Carried: the `flux app run` path has no `EventStore` in scope (`flux_app::App` owns its own store),
  so its egress audit is a `TODO(D-20)` at that call site; wire it when the app runner threads its store
  through. The smoke-script ergonomics item is deferred (see above).

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

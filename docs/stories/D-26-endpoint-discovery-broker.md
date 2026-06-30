---
id: D-26
title: Discovery provider role & host fan-out broker
pillar: Core
status: backlog
priority:
design: docs/designs/endpoint-discovery.md
---

# Discovery provider role & host fan-out broker

## Goal
Let a plugin declare it can **discover** endpoints for a set of products, and let a consumer plugin
ask the host *"which endpoints exist for product X?"*. The host **fans out** to matching provider
plugins, aggregates and ranks their candidates, and returns **weak references only** (URLs + credential
references, never secrets). Discovery becomes "just another resolver" feeding the
[D-25](D-25-endpoint-reference-model.md) registry — the `sql`-asks-host-asks-`kubernetes` mechanism.

## Why
flux has no cross-plugin discovery: each plugin is configured with one static endpoint and cannot
learn about services another plugin can see. fluxplane's `DiscoveryRegistry`/`DiscoveryProvider`
(`~/projects/fluxplane/fluxplane-endpoint/discovery_registry.go`) is the proven model — providers
declare the products they can discover, the registry matches a consumer's query and fans out. See the
[epic design](../designs/endpoint-discovery.md) — *Discovery protocol*.

## Acceptance
- [ ] **Provider declaration** — the plugin manifest gains `discovers: [products]` and a standard
      `endpoint.discover(product, query, limit) -> [candidate]` op contract; manifest parse + round-trip
      covered. Test `endpoint::manifest_declares_discovery_products`.
- [ ] **Consumer host capability** — a new `endpoint.discover` host capability, **deny-by-default** and
      manifest-gated like `process`/`conn`/`secrets`; an ungranted call is refused. Failing-first test
      `endpoint::discover_capability_gated`.
- [ ] **Fan-out broker** — matches the requested product against registered providers, calls each, and
      aggregates + ranks candidates by score across **≥2 mock providers**. Test
      `endpoint::broker_fans_out_and_ranks`.
- [ ] **Secret-free results** — discovery returns weak refs only; no `Resolved`, no `Material`, no
      secret-shaped value appears in any candidate. Test `endpoint::discovery_results_carry_no_secrets`.
- [ ] **Registry commit** — accepted candidates are committed to the D-25 registry with `owner` + `ttl`,
      and a provider's `replace_owned` refresh reconciles only its own entries.
- [ ] Gate green: `cargo test -p flux-plugin` (+ schema crate), clippy `-D warnings`, fmt, `flux-codegate`.

## Progress
- (not started — needs [D-25](D-25-endpoint-reference-model.md).)

## Notes
- Broker lives in `flux-plugin` (L4), invoked by the L6 surfaces that hold the loaded-plugin set
  (`load_plugin_tools`); providers and consumers never address each other — the broker is the only
  intermediary.
- Prior art (shapes only): `fluxplane-endpoint/discovery_registry.go`.
- Blocks [D-27](D-27-reference-based-io.md), [D-28](D-28-kubernetes-endpoint-provider.md).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).

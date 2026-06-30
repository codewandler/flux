---
id: D-26
title: Discovery provider role & host fan-out broker
pillar: Core
status: done
design: docs/designs/endpoint-discovery.md
note: manifest `discovers`/`discover` + the L5 `EndpointBroker` (fan-out over a `ProviderInvoker` seam, rank, re-entrancy guard) + `EndpointBrokerHostCaps`; wired into both `flux run` and `flux app run` so a consumer plugin's `endpoint.discover` reaches provider plugins through the host
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
- [x] **Provider declaration** — `PluginManifest.discovers: Vec<String>` + the standard
      `endpoint.discover(product, query, limit) -> { candidates: [EndpointCandidate] }` op contract a
      provider exposes; host-kit gained a `.discovers(product)` builder.
- [x] **Consumer host capability** — `PluginCapabilities.discover: bool` gates a new `endpoint.discover`
      host capability, **deny-by-default**; `EndpointBrokerHostCaps` refuses it when ungranted. Test
      `endpoint::host_caps::tests::discover_capability_gated`.
- [x] **Fan-out broker** — `EndpointBroker.discover(product, query, limit, requester)` matches
      `providers_for(product)`, calls each via the `ProviderInvoker` seam, aggregates + stable-sorts by
      score desc, truncates, with a re-entrancy guard (skips the requester / in-flight providers). Tests
      `broker_fans_out_and_ranks`, `discover_skips_the_requester`.
- [x] **Secret-free results** — discovery returns weak `EndpointCandidate`s only (no `ResolvedEndpoint`,
      no injected headers, no secret value). Test `discovery_results_carry_no_secrets`.
- [x] **Registry commit** — accepted candidates are committed to the D-25 `EndpointRegistry` with the
      discovering provider as `owner`; `replace_owned` (from D-25) reconciles a provider's own entries.
- [x] Gate green: `cargo test -p flux-plugin -p flux-capabilities`, clippy `-D warnings` (incl. flux-cli/
      flux-app), fmt, `flux-codegate`, full workspace build + `plugins/` build (host-kit change).

## Progress
- **Done.** Landed the manifest `discovers`/`discover` fields, the `LoadedPlugin` return from
  `load_plugin_tools` (so the surface can register providers), and the L5
  `flux_capabilities::endpoint::{PluginRegistry, ProviderInvoker/HostProviderInvoker, EndpointBroker,
  EndpointBrokerHostCaps}`. Wired the broker into **both** the `flux run` (`build_agent`) and
  `flux app run` (`run_app`) surfaces: each plugin's caps are wrapped in `EndpointBrokerHostCaps` and
  registered as a `ProviderEntry`. Agent-facing `endpoint.*` ops are intentionally deferred to D-28.
- **Carried forward:** the broker will additionally implement `flux_plugin::ReferenceResolver` (for
  ref-based IO + cross-plugin credential materialization) in D-27.

## Notes
- Broker lives in `flux-capabilities` (L5, wrapping L4 `SystemHostCaps` like `DatasourceHostCaps`),
  invoked by the L6 surfaces that hold the loaded-plugin set; providers and consumers never address
  each other — the broker is the only intermediary. (Refines the story's original "broker in flux-plugin
  L4" note: it sits at L5 because it must hold live plugin hosts, exactly as `DatasourceHostCaps` does.)
- Prior art (shapes only): `fluxplane-endpoint/discovery_registry.go`.
- Blocks [D-27](D-27-reference-based-io.md), [D-28](D-28-kubernetes-endpoint-provider.md).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).

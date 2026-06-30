---
id: D-25
title: Endpoint reference model & registry (the references-only spine)
pillar: Core
status: done
design: docs/designs/endpoint-discovery.md
---

# Endpoint reference model & registry (the references-only spine)

## Goal
Make a host-managed **reference** the only currency a plugin operation handles for endpoints and
credentials. Introduce the `EndpointRef` weak-reference type (carrying a `credential_ref`, never a
secret), the host `EndpointRegistry` (owner/TTL/health), the host-only `Resolved` form, and a
`ReferenceResolver` whose first source is a **static env/config resolver** that binds references from
**host config** — replacing the per-plugin env coupling that exists today. No discovery yet; this is
the data-model + resolution spine the rest of the epic builds on. Serves the Core safety value: a
plugin op can no longer name an env var, hold a raw secret, or assemble a credential-bearing URL.

## Why
Today a plugin's own manifest hardcodes env-var **names** (`EndpointSpec.env: ["GITLAB_URL"]`,
`AuthMethod.env: ["GITLAB_TOKEN"]`, `crates/flux-plugin/src/lib.rs:200,148`) and the `endpoint` host
capability hands the plugin the env-resolved **URL string** (`:772–775`, `:506–522`). That is the last
place a plugin "deals with environment variables directly" and couples plugin code to host config.
flux already manages live resources by opaque handle (`conn_id`/`proc_id`/`blob_ref`, `:843–993`);
this story extends that discipline upstream to the endpoint/credential binding. See the
[epic design](../designs/endpoint-discovery.md) — *The reference invariant*.

## Acceptance
- [x] **`EndpointRef` weak-ref type** — `@endpoint/<id>` form, `url` (no embedded credentials),
      `product`, `protocol`, `source`, `credential_ref: Option<flux_secret::Ref>` (reuses the existing
      `Env`/`Plugin`/`Kubernetes` schemes) + `EndpointCandidate`/`EndpointRecord`. Round-trips (JSON +
      TOML). Tests `endpoint_ref_round_trips_and_carries_no_secret`, `record_toml_round_trips`.
- [x] **`EndpointRegistry`** — `put`/`resolve`/`list` + `replace_owned` keyed by id, each
      `EndpointRecord` carrying `owner`/`ttl_secs`/`discovered_at_secs`/`health`; `replace_owned`
      reconciles one owner's set without disturbing others, plus `save`/`load` to `~/.flux/endpoints.toml`
      (weak refs only). Tests `registry_put_resolve_replace_owned`, `registry_save_load_round_trips_weak_refs_only`.
- [x] **Static env/config resolver** — `StaticResolver` impls the new `flux_plugin::ReferenceResolver`,
      binding a *named* reference from a host-side binding map (not a plugin manifest) and materializing
      `env`-scheme credentials through the guarded `System`. Test `static_resolver_binds_from_host_config`.
- [x] **`ResolvedEndpoint` is host-only** — no `Serialize` impl (compile-time: can't reach the model)
      and a non-leaking `Debug` (never prints a header *value*). Test
      `resolved_endpoint_debug_does_not_leak_header_values`.
- [x] **Crate placement settled** — schema types in the L0 module `flux_secret::endpoint`
      (`credential_ref` stays `flux_secret::Ref`); the `ReferenceResolver` trait seam in `flux-plugin`
      (L4); the registry + resolver in `flux-capabilities` (L5, mirroring `DatasourceHostCaps`).
      `flux-codegate` layering lint green (no new inner→outer edge; no new crate).
- [x] Gate green: `cargo test -p flux-secret -p flux-plugin -p flux-capabilities`, clippy `-D warnings`,
      fmt, `flux-codegate`, full `cargo build --workspace`.

## Progress
- **Done.** Landed the L0 `flux_secret::endpoint` schema (`EndpointRef`/`EndpointCandidate`/
  `EndpointRecord`/`ResolvedEndpoint` + `SourceKind`), the `flux_plugin::ReferenceResolver` trait seam,
  and `flux_capabilities::endpoint::{EndpointRegistry, StaticResolver}` with `~/.flux/endpoints.toml`
  persistence. All tests + clippy + fmt + codegate + workspace build green.
- **Carried forward:** wiring `ReferenceResolver` into `SystemHostCaps` (a `with_resolver` field) is in
  D-27 (where ref-based IO consumes it); the op-handler-level "no env/URL on the op surface"
  enforcement is in D-29.

## Notes
- Reuse: `flux_secret::Ref` (incl. `Kubernetes`), `Material` (unserializable value), `Redactor`; the
  opaque-handle precedent (`conn_id`/`proc_id`/`blob_ref`).
- Reverses the explicit D-10/D-12 deferral of a `.dex`-style endpoint registry.
- Blocks [D-26](D-26-endpoint-discovery-broker.md) and [D-27](D-27-reference-based-io.md).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).

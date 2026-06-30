---
id: D-25
title: Endpoint reference model & registry (the references-only spine)
pillar: Core
status: ready
priority: 1
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
- [ ] **`EndpointRef` weak-ref type** — canonical `@endpoint/<id>`, `url` (no embedded credentials),
      `product`, `protocol`, `source`, and `credential_ref: Option<flux_secret::Ref>` (reuses the
      existing `Env`/`Plugin`/`Kubernetes` schemes). Round-trips. Test `endpoint::ref_round_trips`.
- [ ] **`EndpointRegistry`** — `put`/`resolve`/`list` + `replace_owned` keyed by id, each
      `EndpointRecord` carrying `owner`, `discovered_at`, `ttl`, optional health; `replace_owned`
      reconciles one owner's set without disturbing others. Test
      `endpoint::registry_put_resolve_replace_owned`.
- [ ] **Static env/config resolver** — a `ReferenceResolver` materializes a reference from **host
      binding config**, not from any plugin manifest. Test `endpoint::static_resolver_binds_from_host_config`
      asserts resolution succeeds with the binding in host config and the plugin manifest naming no env key.
- [ ] **`Resolved` is host-only** — the runtime form that carries injected credential material has no
      model-visible serializer; a failing-first test `endpoint::resolved_never_serializes_to_model`
      proves a `Resolved` cannot be serialized into agent/model-visible output (mirrors
      `flux_secret::Material`'s unserializable value).
- [ ] **Crate placement settled** — reference schema types in an L0 module (extend `flux-secret` or a
      sibling L0 crate; `credential_ref` stays `flux_secret::Ref`); registry seam in `flux-plugin` (L4).
      `flux-codegate` layering lint green (no new inner→outer edge).
- [ ] Gate green: `cargo test -p flux-secret -p flux-plugin` (+ the schema crate), clippy `-D warnings`,
      fmt, `flux-codegate`.

## Progress
- (not started — leads the endpoint-discovery epic; design-first doc landed at
  [endpoint-discovery.md](../designs/endpoint-discovery.md).)

## Notes
- Reuse: `flux_secret::Ref` (incl. `Kubernetes`), `Material` (unserializable value), `Redactor`; the
  opaque-handle precedent (`conn_id`/`proc_id`/`blob_ref`).
- Reverses the explicit D-10/D-12 deferral of a `.dex`-style endpoint registry.
- Blocks [D-26](D-26-endpoint-discovery-broker.md) and [D-27](D-27-reference-based-io.md).
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).

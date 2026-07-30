---
id: D-221
title: "Connector-backed storage — one registry, safe object and credential facades"
pillar: Core
status: backlog
design: docs/designs/connector-backed-storage-facade.md
epic: connector-backed-storage-facade
note: "EPIC — account-scoped named stores with per-capability defaults; Babelforce S3 for objects, Vault/1Password for credentials, and connector declarations driving both without exposing secrets"
---

# Connector-backed storage — one registry, safe object and credential facades

## Goal

Let a standalone or hosted Flux runtime bind many named storage services behind one account-scoped
registry, use a customer-safe object facade for local/S3 data, and use a structurally secret-safe
credential facade for local/Vault/1Password data. flux-connectors declares portable backend roles;
Flux remains the only runtime and safety envelope.

## Acceptance

- [ ] `StoreRegistry` resolves named profiles and separate object/credential defaults for an
      authenticated account; backend type is filterable metadata, never an ambiguous access
      selector.
- [ ] `ObjectStoreFacade` derives tenant prefixes from immutable host identity, accepts only
      validated relative paths, and returns bounded opaque blob handles rather than arbitrary bytes.
- [ ] The hosted platform can supply a hidden Babelforce-managed S3 object default, while that
      binding is structurally ineligible for credential storage.
- [ ] `CredentialStore` supports general secret records plus OAuth refresh/expiry metadata, and one
      injected/default seam is used consistently by CLI, SDK, providers, plugins, doctor, and the
      connector pack without breaking existing file/Vault records.
- [ ] flux-connectors declares `object_storage` and `secret_storage` roles and emits host-only store
      adapter descriptors separately from operator management Tools.
- [ ] HashiCorp Vault KV-v2 and 1Password Connect are first-class secret-storage providers;
      AWS S3 claims object storage after its SigV4/XML prerequisites land; local adapters remain in
      Flux.
- [ ] Credential and management paths prove a secret sentinel appears in no model/tool result,
      view, error, progress, approval, event, blob, catalogue, or generated artifact.
- [ ] Operator management operations are absent from model registries and still traverse immutable
      identity, authorization, approval, audit, redaction, and guarded IO.
- [ ] Backend errors fail closed without local fallback; bootstrap accepts only trusted
      env/file/workload-identity references and cannot recurse through the store registry.
- [ ] Local, native-S3, connector-S3, Vault, and 1Password adapters pass shared capability-specific
      conformance suites; cross-account and cross-tenant isolation have failing-first tests.
- [ ] The full Flux and flux-connectors gates are green, generated artefacts are fixed points, and
      public configuration/security documentation describes defaults, BYOS, migration, and failure
      behavior without exposing platform internals or secret values.

## Progress

- 2026-07-30: joined design written in
  [connector-backed-storage-facade.md](../designs/connector-backed-storage-facade.md); implementation
  stories remain to be filed in Flux, flux-connectors, and the hosted platform along the delivery
  slices recorded there.

## Notes

- This epic supersedes the narrower framing “choose a different OAuth token backend.” The current
  `CredentialStore` remains a compatibility input, but the product boundary is an account-scoped
  registry with separate object and credential facades.
- Related landed work: D-83 (file/Vault credential backends), D-126 (`flux auth set`), D-130 (Vault
  Kubernetes auth), and flux-connectors C-90/C-116/C-136 (credential addressing, the connector-pack
  port, and credential diversion).
- No implementation story may put ordinary object bytes and credential values behind one untyped
  read method. That would erase the design's safety boundary.

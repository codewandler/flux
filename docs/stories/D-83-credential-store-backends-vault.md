---
id: D-83
title: Backend-abstracted credential store (file + Vault)
pillar: Core
status: done
design: docs/designs/plugin-oauth.md
epic: plugin-oauth
note: "CredentialStore trait; file backend (dev/CLI) + host-injectable Vault backend (deployment); generalize save_stored keying to plugin+purpose[+account]. Unblocks per-customer OAuth tokens → Vault (Integrations)."
---

# Backend-abstracted credential store (file + Vault)

## Goal
Let flux persist plugin/provider tokens through a **pluggable backend** — a local file for dev/CLI, and
**Vault** when deployed — so credentials never sit in a file on a pod.

## Acceptance
- [ ] A `CredentialStore` trait; the file backend (`~/.flux/credentials.toml`, 0600) is the default
      (generalize `save_stored`/`store_path`/`TokenSource`, `crates/flux-credentials/src/lib.rs`, to key
      by `plugin+purpose[+account]` instead of the two provider consts).
- [ ] A **Vault** backend implementation, selectable/configurable; the store backend is
      **host-injectable** (a host app supplies its own, the way it supplies custom `HostCapabilities`).
- [ ] Provider logins (`claude`/`codex`) keep working through the file backend (no regression).
- [ ] Failing-first test: tokens round-trip through the trait; a mock backend proves injection.

## Progress
- 2026-07-08 **DONE.** Added an async `CredentialStore` trait (flux-credentials) with two backends:
  `FileCredentialStore` (the existing 0600 `~/.flux/credentials.toml`, the default — so `claude`/
  `codex` provider logins and the CLI keep working unchanged) and `VaultCredentialStore` (Vault KV-v2
  at `<addr>/v1/<mount>/data/<prefix>/<key>` with `X-Vault-Token`, a `from_env` constructor, `:`
  key-separators → path segments). `resolve_stored_bearer` now takes an injectable `&dyn
  CredentialStore`; flux-plugin's `SystemHostCaps` gained a `cred_store` field + a
  `with_credential_store` builder (host-injectable like the resolver/secret-sink), defaulting to the
  file backend. Keying is `plugin:<name>:<purpose>`. Tests:
  `credential_store_trait_round_trips_and_is_injectable` (mock backend proves injection) +
  `vault_credential_store_round_trips_via_kv_v2` (KV-v2 shape via a loopback stub).

## Notes
- Design: [plugin-oauth.md](../designs/plugin-oauth.md). **Unblocks** a downstream consumer's platform
  Integrations layer (per-customer OAuth tokens → Vault) and its OAuth-wrapping plugin deployment.

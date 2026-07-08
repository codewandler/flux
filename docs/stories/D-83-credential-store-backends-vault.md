---
id: D-83
title: Backend-abstracted credential store (file + Vault)
pillar: Core
status: backlog
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
- Proposed. Independent of D-80..D-82 but shares the epic; the store is where D-81/D-82 persist tokens.

## Notes
- Design: [plugin-oauth.md](../designs/plugin-oauth.md). **Unblocks** a downstream consumer's platform
  Integrations layer (per-customer OAuth tokens → Vault) and its OAuth-wrapping plugin deployment.

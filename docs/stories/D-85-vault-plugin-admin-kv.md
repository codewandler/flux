---
id: D-85
title: Vault plugin: grouped admin diagnostics + KV-v2 secrets
pillar: Agent
status: done
design: docs/designs/secret-management-plugins.md
epic: secret-management-plugins
note: "Native Vault plugin over host HTTP: `vault.admin` diagnostics and `vault.kv` KV-v2 ops; metadata records only, never secret values."
---

# Vault plugin: grouped admin diagnostics + KV-v2 secrets

## Goal
Ship a native Vault plugin that gives agents safe, grouped access to Vault diagnostics and KV-v2
workflows without bypassing host-managed HTTP/auth.

## Acceptance
- [x] `plugins/vault` declares endpoint `vault.endpoint` from `VAULT_ADDR`, token auth from
      `VAULT_TOKEN`, optional namespace config from `VAULT_NAMESPACE`, and private-host intent.
- [x] Admin read ops are grouped as `vault.admin`: health, auth list, mount list, policy list/read,
      token lookup-self.
- [x] KV-v2 ops are grouped as `vault.kv`: list/read/write/patch/metadata/version delete/undelete/destroy.
- [x] KV metadata/list ops contribute only key metadata; `vault.kv.read` does not contribute secret values.
- [x] Failing-first tests cover manifest groups, namespace header forwarding, KV metadata contribution,
      and no contribution from explicit secret reads.

## Progress
- Done in this session. `flux-plugin-vault` ships grouped admin/KV ops with metadata-only datasource
  contribution tests.

## Notes
- Admin write operations such as policy mutation, mount mutation, token creation, seal, or unseal are
  out of scope for v1.

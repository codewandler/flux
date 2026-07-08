# Design: secret-management plugins

**Status:** in progress · **Epic:** `secret-management-plugins` · **Stories:** D-84..D-86

## Why

flux has a mature native plugin pack for DevOps systems, but not for the secret-management systems
operators reach for during incident response and integration setup. Vault and 1Password need the same
plugin rules as every other integration: no direct network, no local env reads, no vendor SDK owning
IO, and no indexing of secret values.

## Shape

- **D-84 — plugin operation groups.** Plugin manifests carry group definitions and per-op group tags;
  the host projects them into `ToolSpec.group` and merges plugin groups into the runtime group list.
  Shipped plugin groups are force-on (`surface_when = []`) so grouping organizes installed ops without
  hiding them.
- **D-85 — Vault.** A native `flux-plugin-vault` speaks to `VAULT_ADDR` through host HTTP, with
  `VAULT_TOKEN` injected as `X-Vault-Token`. Admin diagnostics live under `vault.admin`; KV-v2 ops
  live under `vault.kv`.
- **D-86 — 1Password Connect.** A native `flux-plugin-onepassword` speaks to `OP_CONNECT_HOST` through
  host HTTP, with `OP_CONNECT_TOKEN` injected as Bearer. Server, vault, item, and file ops are grouped
  separately.

## Safety

Both plugins commonly target private endpoints, so their manifests declare private hosts but still
depend on the operator's `.flux/config.toml` private-net grant. Datasource records include only
metadata: Vault KV key names/metadata and 1Password vault/item/file metadata. Secret values and file
bytes are returned only by explicit read/download ops and are never contributed to the datasource index.

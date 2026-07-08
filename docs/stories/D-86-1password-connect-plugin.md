---
id: D-86
title: 1Password Connect plugin: grouped vault/item/file operations
pillar: Agent
status: done
design: docs/designs/secret-management-plugins.md
epic: secret-management-plugins
note: "Native 1Password Connect plugin over host HTTP: grouped server/vault/item/file ops; file bytes via blob refs; metadata records only."
---

# 1Password Connect plugin: grouped vault/item/file operations

## Goal
Ship a native 1Password Connect plugin that exposes vault, item, and file operations through host
HTTP and keeps item/file content out of datasource records.

## Acceptance
- [x] `plugins/onepassword` declares endpoint `onepassword.endpoint` from `OP_CONNECT_HOST`, bearer
      auth from `OP_CONNECT_TOKEN`, blob support, and private-host intent.
- [x] Ops are grouped as `onepassword.server`, `onepassword.vaults`, `onepassword.items`, and
      `onepassword.files`.
- [x] Item/file listing contributes metadata only; explicit item show may return fields but does not
      contribute them.
- [x] File content downloads use `http_bytes_ref` and return a host `blob_ref`.
- [x] Failing-first tests cover manifest groups, metadata-only contribution, explicit-show no-contribute,
      and file content blob storage.

## Progress
- Done in this session. `flux-plugin-onepassword` ships grouped Connect ops with metadata-only
  datasource and blob-download tests.

## Notes
- The `op` CLI backend is out of scope for v1; this plugin is Connect REST API only.

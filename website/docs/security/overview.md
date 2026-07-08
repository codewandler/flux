---
title: Security overview
description: How flux keeps you safe — the runtime envelope, credentials, the plugin sandbox, plugin trust, and inbound server auth, in one place.
---

# Security overview

flux runs real tools, plugins, and network calls on your behalf. This section explains the guarantees
that hold by construction, the trust boundaries you still own, and the pages to read for each part of
the model.

There are three directions of trust to keep separate in your head:

- **Local** — the agent reaching your filesystem, processes, and network. Governed by the one
  [safety envelope](../agent/safety.md): every operation lowers onto a single
  authorization → approval → guarded-IO chain. Nothing has a side door.
- **Outbound** — flux authenticating to the services it calls (LLM providers, plugin APIs).
  Governed by [Credentials & secrets](./credentials.md): tokens the host stores and injects, and
  a redactor that keeps their values out of the model's sight.
- **Inbound** — callers authenticating to a flux **server** you expose. Governed by
  [Server authentication & tenancy](./server-auth.md): bearer-token auth, OIDC introspection, and
  per-tenant isolation.

## The pillars

| Pillar | What it protects | Page |
|---|---|---|
| The envelope | The agent can't touch fs/process/network except through one gated chain | [Safety & approvals](../agent/safety.md) |
| Credentials & secrets | Provider and plugin tokens; secret values never reach the model | [Credentials & secrets](./credentials.md) |
| Plugin capability sandbox | What a plugin's code can reach *through flux* | [Plugin capability sandbox](./plugin-sandbox.md) |
| Plugin trust & signing | *Which* plugin code runs — supply-chain integrity | [Plugin trust & signing](./plugin-trust.md) |
| Server auth & tenancy | Who may drive a networked flux, and tenant isolation | [Server authentication & tenancy](./server-auth.md) |

## An honest posture

Two things are easy to assume and worth stating plainly up front, because the deep pages depend on
you knowing them:

- **Stored credentials are plaintext on disk, protected by file permissions — not encrypted.** The
  default store is `~/.flux/credentials.toml`, written `0600` (owner-only) via an atomic write. That
  is the whole at-rest protection under the default backend. Encryption/externalization is an opt-in
  choice (a Vault-backed store) — see [Credentials & secrets](./credentials.md).
- **Plugins are capability-confined and integrity-pinned — not OS-sandboxed.** A plugin binary is
  trusted, pinned code, launched with a cleared environment and able to reach only what its manifest
  declared *through flux*. flux does not run it in an OS sandbox. Review plugins the way you review
  dependencies — see [Plugin trust & signing](./plugin-trust.md).

## Where things live

- `~/.flux/credentials.toml` — provider and plugin OAuth tokens (`0600`).
- `~/.flux/plugins/bin/<name>/<version>/…` — installed, hash-pinned plugin binaries.
- `.flux/config.toml` — your permission rules and per-plugin network grants. See
  [Configuration](../reference/config.md).

Everything below is enforced in the runtime, not asked of you as convention.

## Related docs

- [Safety & approvals](../agent/safety.md) — local filesystem/process/network access.
- [Credentials and secrets](./credentials.md) — outbound tokens and redaction.
- [Server authentication & tenancy](./server-auth.md) — inbound callers and realm isolation.

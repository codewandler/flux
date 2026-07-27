---
title: Security overview
description: How flux keeps you safe — the runtime envelope, credentials, the plugin sandbox, plugin trust, and inbound server auth, in one place.
---

# Security overview

flux runs real tools, plugins, and network calls on your behalf. This section explains the guarantees
that hold by construction, the trust boundaries you still own, and the pages to read for each part of
the model.

There are three directions of trust to keep separate in your head:

> **New here, or not an engineer?** Read [Security in plain terms](./plain-terms.md) first — the
> same guarantees without the jargon — then come back for the mechanics.

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
| OS process sandbox | What a spawned process's raw syscalls can reach (opt-in) | [OS process sandboxing](./os-sandbox.md) |
| Plugin trust & signing | *Which* plugin code runs — supply-chain integrity | [Plugin trust & signing](./plugin-trust.md) |
| Server auth & tenancy | Who may drive a networked flux, and tenant isolation | [Server authentication & tenancy](./server-auth.md) |

## Also enforced by the runtime

Beyond the six pillars, the runtime holds a set of smaller guarantees by construction rather than
by convention. Each is covered by a test:

- **Egress / SSRF guard** — every URL flux fetches (a built-in op, a plugin callback, a browser
  fetch) is resolved and blocked when it points at a private, loopback, link-local, unique-local,
  CGNAT, or IPv4-mapped address, or an internal hostname, unless the caller holds a scoped
  private-network grant. After vetting, the connection is **pinned to the exact validated
  addresses**, closing the DNS-rebinding gap between the guard and the connect. This is the one
  guard — there is no second, hand-rolled URL check. See [Safety & approvals](../agent/safety.md).
- **Session integrity** — a turn always ends with a valid conversation, even on cancel, compaction,
  or an iteration cap: no empty assistant turn, no orphaned tool call, no two user messages in a
  row. Providers reject malformed histories, so flux never produces one.
- **Immutable caller identity** — within a live turn, *who* is acting is fixed at the start and
  can't be swapped mid-turn by the model or a sub-agent.
- **Audit trail** — flux records an append-only event log of tool calls, approvals, and destructive
  markers, so an action can be traced after the fact.

## An honest posture

Two things are easy to assume and worth stating plainly up front, because the deep pages depend on
you knowing them:

- **Stored credentials are plaintext on disk, protected by file permissions — not encrypted.** The
  default store is `~/.flux/credentials.toml`, written `0600` (owner-only) via an atomic write. That
  is the whole at-rest protection under the default backend. Encryption/externalization is an opt-in
  choice (a Vault-backed store) — see [Credentials & secrets](./credentials.md).
- **Plugins are capability-confined and integrity-pinned — not OS-sandboxed by default.** A plugin
  binary is trusted, pinned code, launched with a cleared environment and able to reach only what
  its manifest declared *through flux*. flux does not run it in an OS sandbox unless you opt in via
  `[sandbox]` (bubblewrap on Linux, Seatbelt on macOS) — see
  [OS process sandboxing](./os-sandbox.md). Review plugins the way you review dependencies — see
  [Plugin trust & signing](./plugin-trust.md).

## Where things live

- `~/.flux/credentials.toml` — provider and plugin OAuth tokens (`0600`).
- `~/.flux/plugins/bin/<name>/<version>/…` — installed, hash-pinned plugin binaries.
- `.flux/config.toml` — your permission rules and per-plugin network grants. See
  [Configuration](../reference/config.md).

Everything below is enforced in the runtime, not asked of you as convention.

## Related docs

- [Security in plain terms](./plain-terms.md) — the same guarantees for non-developers.
- [Safety & approvals](../agent/safety.md) — local filesystem/process/network access.
- [Credentials and secrets](./credentials.md) — outbound tokens and redaction.
- [OS process sandboxing](./os-sandbox.md) — opt-in confinement of spawned processes' raw syscalls.
- [Server authentication & tenancy](./server-auth.md) — inbound callers and realm isolation.

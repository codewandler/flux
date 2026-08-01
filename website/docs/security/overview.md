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
  [safety envelope](../agent/safety.md): every model-requested operation lowers onto a single
  authorization → approval → guarded-IO chain. Trusted native plugin code remains a separate OS
  trust boundary, described below.
- **Outbound** — flux authenticating to the services it calls (LLM providers, plugin APIs).
  Governed by [Credentials & secrets](./credentials.md): tokens the host stores and resolves, the
  different ways they reach trusted IO code, and a redactor that keeps their values out of the
  model's sight.
- **Inbound** — callers authenticating to a flux **server** you expose. Governed by
  [Server authentication & tenancy](./server-auth.md): bearer-token auth, OIDC introspection, and
  per-tenant isolation.

## The pillars

| Pillar | What it protects | Page |
|---|---|---|
| The envelope | The agent can't touch fs/process/network except through one gated chain | [Safety & approvals](../agent/safety.md) |
| Credentials & secrets | Provider and plugin tokens; secret values never reach the model | [Credentials & secrets](./credentials.md) |
| Plugin capability sandbox | What a plugin's code can reach *through host callbacks* | [Plugin capability sandbox](./plugin-sandbox.md) |
| OS process sandbox | What a spawned process's raw syscalls can reach (interactive opt-in; selected unattended CLI forms fail closed) | [OS process sandboxing](./os-sandbox.md) |
| Plugin trust & signing | *Which* plugin code runs — supply-chain integrity | [Plugin trust & signing](./plugin-trust.md) |
| Server auth & tenancy | Who may drive a networked flux, and tenant isolation | [Server authentication & tenancy](./server-auth.md) |

## Also enforced by the runtime

Beyond the six pillars, the runtime holds a set of smaller guarantees by construction rather than
by convention. Each is covered by a test:

- **Egress / SSRF guard** — native HTTP/fetch/crawl operations, plugin-host HTTP/OAuth calls, and
  fleet worker calls use the shared DNS-aware guard. It blocks private, loopback, link-local,
  unique-local, CGNAT, IPv4-mapped, and internal destinations unless the caller holds a scoped
  private-network grant; those adapters also bind their clients to the exact addresses the guard
  vetted. Three outer adapters do not yet provide that full boundary: browser requests are
  URL-checked before Chrome continues but Chrome resolves the host again; A2A push delivery is
  rechecked before sending but its pooled client resolves again; and `flux a2a <URL>` currently
  treats the operator-supplied URL as trusted and does not apply the shared guard. Do not pass an
  untrusted URL to that command. See [Safety & approvals](../agent/safety.md).
- **Session integrity** — a turn always ends with a valid conversation, even on cancel, compaction,
  or an iteration cap: no empty assistant turn, no orphaned tool call, no two user messages in a
  row. Providers reject malformed histories, so flux never produces one.
- **Immutable caller identity** — within a live turn, *who* is acting is fixed at the start and
  can't be swapped mid-turn by the model or a sub-agent.
- **Audit trail** — flux records an append-only event log of tool calls, approvals, and destructive
  markers that cross the runtime. Raw syscalls from an unsandboxed native plugin are outside that
  host-mediated trail.

## An honest posture

Three things are easy to assume and worth stating plainly up front, because the deep pages depend on
you knowing them:

- **Stored credentials are plaintext on disk, protected by file permissions — not encrypted.** The
  default store is `~/.flux/credentials.toml`, written `0600` (owner-only) via an atomic write. That
  is the whole at-rest protection under the default backend. A Vault-backed store is available to
  host applications through the credential-store API; setting Vault environment variables does not
  switch the stock CLI or server away from the file store. See
  [Credentials & secrets](./credentials.md).
- **Plugin host callbacks are capability-confined; native code remains trusted.** A plugin process
  starts with a cleared environment and can reach only what its manifest declared *through flux*.
  The binary is not OS-sandboxed by default in interactive use, so direct syscalls can bypass the
  callback protocol. Enable `[sandbox]` (bubblewrap on Linux, Seatbelt on macOS) for that additional
  boundary. The CLI selects fail-closed confinement for its recognized auto-approved forms and for
  `flux app run --serve`. An unflagged `flux app run <program>` that serves only program-declared
  channels uses the posture resolved from CLI configuration and environment. Direct SDK/server
  embedders must inject a sandbox or export its environment settings. In both cases, no selected
  posture means sandboxing is off and process networking is open. See
  [OS process sandboxing](./os-sandbox.md). Review plugins the way you review dependencies — see
  [Plugin trust & signing](./plugin-trust.md).
- **The managed config tier is an operator control, not a defense against your own machine.** A
  system-owned config file (`/etc/flux/config.toml` or `$FLUX_MANAGED_CONFIG`) can pin security-
  relevant settings — the authorization floor, egress grants, the tool blocklist, sandbox
  confinement — so a project or user config can only make them *more* restrictive, never relax
  them. Its authority is entirely filesystem permissions on that one file: it stops an ordinary
  developer from casually loosening an audited baseline, not a user who owns the machine and can
  edit the managed file or the `flux` binary itself. See
  [Managed configuration tier](../reference/config.md#managed-configuration-tier-operator-floor).

## Where things live

- `~/.flux/credentials.toml` — provider and plugin tokens (`0600`, plaintext).
- `~/.flux/plugins/bin/<name>/<version>/…` — installed, hash-pinned plugin binaries.
- `.flux/config.toml` — your permission rules and per-plugin network grants. See
  [Configuration](../reference/config.md).

## Related docs

- [Security in plain terms](./plain-terms.md) — the same guarantees for non-developers.
- [Safety & approvals](../agent/safety.md) — local filesystem/process/network access.
- [Credentials and secrets](./credentials.md) — outbound tokens and redaction.
- [OS process sandboxing](./os-sandbox.md) — interactive defaults, the CLI's fail-closed forms, and
  embedder responsibilities for spawned processes' raw syscalls.
- [Server authentication & tenancy](./server-auth.md) — inbound callers and realm isolation.

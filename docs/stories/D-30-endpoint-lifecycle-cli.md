---
id: D-30
title: Endpoint lifecycle — refresh runner, operator CLI & audit
pillar: Core
status: done
design: docs/designs/endpoint-discovery.md
note: "the epic's final step: `EndpointBroker::refresh` re-discovers + reconciles each owner's set via `replace_owned` (stale dropped, other owners untouched) driven on-demand by `EndpointRunner::tick` (no always-on ticker — it would contend with the agent's plugin-host locks); a `flux endpoint` CLI (`list`/`show`/`resolve`/`import`) renders weak refs + health + the credential *location*, never a value (pinned by `cli::endpoint_list_redacts`); the agent `endpoint.import` op persists a weak ref to `~/.flux/endpoints.toml`; and a new `EndpointDiscovered` audit event fires per provider on `discover`/`refresh` (count only — no URL, no secret). The **endpoint-discovery epic core (D-25→D-30 + D-20) is complete**; D-31 (host-terminated raw-socket auth) and D-32 (retire the `host.endpoint` URL-handback) are filed as backlog hardenings."
---

# Endpoint lifecycle — refresh runner, operator CLI & audit

## Goal
Make discovered endpoints a living, inspectable, auditable part of the system: a periodic **refresh
runner** that re-discovers and reconciles owned endpoints (`replace_owned`, owner/TTL), an operator
**`flux endpoint`** CLI (`list` / `show` / `resolve`) that renders weak references + health but **never
secrets**, and **audit events** on discovery and resolution.

## Why
Discovery is not a once-per-lifetime event — clusters change, endpoints come and go, credentials
rotate. fluxplane's `Runner` (`~/projects/fluxplane/fluxplane-endpoint/runner.go`) refreshes providers
on an interval and reconciles the registry; operators need to see what was discovered and what a
reference binds to without ever exposing a secret. See the
[epic design](../designs/endpoint-discovery.md) — *Discovery protocol* (registry) and *Security model*.

## Acceptance
- [x] **Refresh runner** — re-runs provider discovery on an interval and reconciles each provider's set
      via `replace_owned`, leaving other owners' entries untouched; stale (expired TTL) entries drop.
      Test `endpoint::refresh_reconciles_owned` (broker; `EndpointRunner::tick` drives one cycle — there
      is no always-on ticker, see Notes).
- [x] **`flux endpoint list` / `show`** — render the registry's weak refs + health; a credential value
      or env binding is **never** printed. Failing-first test `cli::endpoint_list_redacts`.
- [x] **`flux endpoint resolve <ref>`** — operator-only; reports what a reference *would* bind to
      (source, host, credential-ref location) without printing secret material.
- [x] **`flux endpoint import <id>`** (+ agent `endpoint.import` op) — persists a weak ref to
      `~/.flux/endpoints.toml`; never a secret.
- [x] **Audit** — discovery (`EndpointDiscovered`: product/provider/count, no URL/secret) and
      resolution (`CrossPluginResolve`) emit `flux-events` records; reads are account-scoped where
      applicable (cf. D-02).
- [x] Gate green: `cargo test --workspace` (+ `-p flux-cli -p flux-plugin`), clippy `-D warnings`,
      fmt, `flux-codegate`.

## Progress
- Done. `EndpointBroker::refresh` re-runs the fan-out per product and reconciles each owner's set via
  `EndpointRegistry::replace_owned` (stale dropped, fresh inserted, other owners untouched); the
  `EndpointRunner` wraps a broker + products + interval and exposes `tick` for a future lock-aware
  scheduler (no always-on ticker — it would contend with the agent's plugin-host locks). The discovery
  audit (`CrossPluginAudit::record_discovery` → `EventKind::EndpointDiscovered`) fires per provider on
  both `discover` and `refresh`. The `flux endpoint` CLI (`list`/`show`/`resolve`/`import`) renders weak
  refs + health + the credential *location*, never a value (pinned by `endpoint_list_redacts`); the
  agent-facing `endpoint.import` op persists a weak ref to `~/.flux/endpoints.toml`. The epic
  (D-25→D-30 + D-20) is complete; the two remaining hardenings are filed as [D-31](D-31-host-terminated-rawsocket-auth.md)
  and [D-32](D-32-retire-url-handback.md).

## Notes
- CLI styling follows [D-19](D-19-plugin-lifecycle-cli.md) (the `flux plugin` lifecycle surface).
- Prior art (shapes only): `fluxplane-endpoint/runner.go`.
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).

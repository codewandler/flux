---
id: D-30
title: Endpoint lifecycle — refresh runner, operator CLI & audit
pillar: Core
status: backlog
priority:
design: docs/designs/endpoint-discovery.md
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
- [ ] **Refresh runner** — re-runs provider discovery on an interval and reconciles each provider's set
      via `replace_owned`, leaving other owners' entries untouched; stale (expired TTL) entries drop.
      Test `endpoint::runner_reconciles_owned`.
- [ ] **`flux endpoint list` / `show`** — render the registry's weak refs + health; a credential value
      or env binding is **never** printed. Failing-first test `cli::endpoint_list_redacts`.
- [ ] **`flux endpoint resolve <ref>`** — operator-only; reports what a reference *would* bind to
      (source, host, credential-ref location) without printing secret material.
- [ ] **Audit** — discovery and resolution emit `flux-events` records (consumer/provider, endpoint,
      grant); reads are account-scoped where applicable (cf. D-02).
- [ ] Gate green: `cargo test -p flux-cli -p flux-plugin` (+ schema crate), clippy `-D warnings`, fmt,
      `flux-codegate`.

## Progress
- (not started — needs [D-25](D-25-endpoint-reference-model.md) and
  [D-28](D-28-kubernetes-endpoint-provider.md).)

## Notes
- CLI styling follows [D-19](D-19-plugin-lifecycle-cli.md) (the `flux plugin` lifecycle surface).
- Prior art (shapes only): `fluxplane-endpoint/runner.go`.
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).

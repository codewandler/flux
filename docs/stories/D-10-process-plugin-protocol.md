---
id: D-10
title: Process-plugin protocol redesign — a clean, unified plugin wire protocol
pillar: Core
status: done
theme: downstream-managed-services
design: docs/designs/process-plugin-protocol.md
note: enriched the plugin manifest (auth-by-purpose, datasource declarations, endpoints) + host capabilities (HTTP method/headers/body + bearer injection, secret-by-purpose, endpoint, datasource-record contribution) over the existing unified frame; `DatasourceHostCaps` L5 bridge (commits `f389bc7`/`7db537a`)
---

# Process-plugin protocol redesign — a clean, unified plugin wire protocol

## Goal
Redesign `flux-plugin`'s subprocess wire protocol, manifest, and binding SDK so a plugin can do everything
the integration pack (**D-08**) needs — call operations, contribute and query **datasource records**
(feeding **D-07**), and request host capabilities (HTTP with secret-by-purpose injection, process, env,
blob, conn) — over **one clean, self-consistent frame**. Informed by fluxplane's evolved protocol but
**not a verbatim port**: we keep its proven *capability surface* and drop the cruft it accreted (dual
v1/v2 modes, three overlapping command families, per-call grant negotiation). Clean cutover of the current
`flux.plugin.v1` — no dual protocol.

## Why (downstream: Slack-channel assistants)
flux's plugin runtime today (`flux.plugin.v1`) is host-complete but **cannot let a plugin contribute
datasource records** to the knowledge layer, has **no auth-by-purpose** (secrets are raw env keys), and
**no endpoint** concept — all of which D-08's Slack/GitLab/Jira/… plugins need to feed D-07 and reach
authenticated APIs. fluxplane solved these but its protocol grew organically; this is the chance to do it
right once, before eight plugins are written against it. **D-08 is blocked on this.**

## flux gap
`flux-plugin` v1: `Frame{protocol,id,kind,command,payload,ok,result,error}`, manifest
`{name,version,operations,capabilities{process,secrets,http}}`, host caps `process.run`/`secret`/`http.do`,
op `{name,description,input_schema,effects,risk}`. Missing: datasource record/search/get/lookup commands,
auth-by-purpose + endpoints in the manifest, the richer op semantics (access/idempotency), and a
plugin-binding SDK beyond the raw `PluginHandler`.

## Acceptance
- [ ] **One unified frame (evolve the existing transport, don't rewrite it).** flux's `call_with_host`
      already writes one `Request` and demuxes plugin→host `Request` callbacks from the op `Response`
      (`crates/flux-plugin/src/lib.rs:504-530`) — keep that. **Drop fluxplane's explicit `target` field**
      (direction already implies plugin-vs-host). A small `command` vocabulary (`op:<name>` / `ds:<verb>` /
      `host:<cap>`) so plugin ops, datasource commands, and host callbacks ride the *same* frame (no
      separate `operations.*`/`datasources.*`/`host.capability.*` families, no v1/v2 mode flag).
      Failing-first test: a fixture round-trips an `op:` call, a `ds:records` contribution, and a
      `host:secret` fetch over the one frame.
- [ ] **Manifest the host introspects** — ops (name + input JSON Schema + op semantics that **reuse
      flux-runtime's `Effect`/permission-subject/`Risk`** + an idempotency hint + `secret_purposes` — **not**
      a ported fluxplane `Access` enum), **datasource declarations** (entity + capabilities + entity schema
      + relations + fallback, consuming the `flux-datasource` types from D-07), **auth methods** (by-purpose,
      env aliases + secret/sensitive fields), and **endpoints** — all in one manifest fetched once (no
      `*.list` round-trips).
- [ ] **Host capabilities** the plugin calls: `http` (with **secret-by-purpose** auto-injection, e.g.
      `Authorization: Bearer <resolved>`), `process`, `env`, `blob`, `conn`; **deny-by-default**, authorized
      from the manifest's declared capabilities. Failing-first test: an ungranted secret/http call is denied.
- [ ] **A Rust plugin-binding SDK** (the analog of fluxplane's `pluginbinding`): typed operation + datasource
      registration, manifest builders, a `serve()` loop, and a host-call client — so a plugin is "declare
      ops/datasources + implement each against the vendor API". `EntitySchema` derived via
      `flux-datasource-derive`.
- [ ] **Clean cutover:** `flux.plugin.v1` is removed; the `echo`/`caps` fixtures + the CLI call sites
      (`load_plugin_tools`, `discover`) move to the new protocol. Op `access/effects` map to flux-runtime
      effects + permission subjects at tool projection. Full gate green; `flux-plugin` stays L4.

## Progress
- Ready. **Depends on D-07** (the `flux-datasource` L0 schema crate, which the datasource commands +
  manifest declarations reference). Blocks **D-08**.

## Notes
- This is the redesign the user flagged: fluxplane's protocol "evolved over time" and carries legacy we
  should not copy. The design doc ([process-plugin-protocol.md](../designs/process-plugin-protocol.md))
  records the specific cruft to drop and the target frame. The detailed wire spec is finalized in the
  design step at the top of implementation (plan Phase 2).
- Reuse: `flux-plugin`'s existing `PluginHost`/`serve` transport scaffolding + `SystemHostCaps` (extend,
  don't rewrite the process plumbing); `flux-secret::Ref` + `SecretResolver` for secret-by-purpose; the
  `flux-datasource` record/declaration/lookup types from D-07.
- Prior art (shapes, not code): `fluxplane-plugin/protocol`, `manifest`, `pluginbinding`. Skip its
  fluxplane-specific bits (`.dex`, MySQL/NATS-backed host).

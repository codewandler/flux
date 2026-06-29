# Design: process-plugin protocol redesign

**Status:** proposed (story [D-10](../stories/D-10-process-plugin-protocol.md)) · **Layer:** L4
(`flux-plugin`) · **Owner:** Timo

## Why

flux's plugin runtime (`flux.plugin.v1`) is host-complete but predates the needs of the integration pack
([D-08](integration-plugins.md)): a plugin **cannot contribute datasource records** ([D-07](datasource-rag.md)),
there is **no auth-by-purpose** (secrets are raw env keys), and **no endpoint** concept for authenticated
APIs. fluxplane solved all three — but its wire protocol grew organically and carries legacy. Before eight
plugins are written against flux's protocol, we redesign it **once, cleanly**, taking fluxplane's capability
surface but not its accreted shape. (The user's steer: *"the protocol for the Go version has evolved over
time — maybe there are parts we can solve more elegantly by changing the process-plugin protocol in a smart
way."*)

## What fluxplane accreted (the cruft to drop)

Learned from `~/projects/fluxplane/fluxplane-plugin`:
- **Dual protocol modes** — a v1 legacy framing *and* a v2 framed mode coexist (`protocol` version string +
  branching). We ship one frame, versioned by the manifest, no mode flag.
- **Three overlapping command families** — `operations.*`, `datasources.*`, and `host.capability.*` are
  bespoke message groups with their own envelopes/lifecycles. We unify them onto **one** request/response
  frame distinguished by direction + a `command` prefix (`op:`/`ds:`/`host:`) — **and drop fluxplane's
  explicit `target` field**, since direction already says who handles a `Request`.
- **Per-call grant negotiation / list round-trips** — `operations.list`, `datasources.list` and ad-hoc
  capability checks happen over the wire at call time. We make the **manifest the single source of truth**,
  fetched once and introspected by the host (capabilities authorized from it, no per-call negotiation).
- **Redundant scoping knobs** — endpoint-ref resolution, fallback modes, and instance plumbing are spread
  across many fields. We keep the *concepts we need* (endpoints, datasource fallback) but as plain manifest
  data, and defer the `.dex`-style registry entirely.

## Target shape (refined in the implementation's design step, plan Phase 2)

### One frame (evolve the existing transport — don't rewrite it)
flux's `PluginHost::call_with_host` (`crates/flux-plugin/src/lib.rs:504-530`) **already** writes a single
`Request` and loops, servicing plugin→host `Request` callbacks inline and awaiting the op's `Response` by
`kind`. So the framing mechanics stay; the redesign is a command-vocabulary + manifest cleanup.
```
Frame {
  id: String,                 // correlation id
  kind: Request | Response | Event,
  command: String,            // "op:slack.message.send" | "ds:search" | "host:http" | …
  payload: Value,             // typed per command
  // responses carry exactly one of:
  ok: bool, result: Value, error: Option<Error>,
}
```
**No `target` field** — *direction* already implies it: a host-initiated `Request` is handled by the
plugin; a plugin-initiated `Request` (mid op) is a host-capability callback. A plugin op call, a datasource
`search`/`get`/`lookup`/`records` contribution, and a host capability request are all just a `command` over
this one frame — same correlation/multiplexing for all three.

### One manifest (fetched once, host-introspected)
- **operations**: `{ name, description, input_schema, effects[], risk, idempotency, secret_purposes[] }`
  — **reuse flux-runtime's `Effect`/permission-subject/`Risk`** vocabulary (add only the idempotency hint +
  `secret_purposes`); do **not** port fluxplane's parallel `Access` enum. `effects` → permission subjects
  at tool projection.
- **datasources**: `flux-datasource` `Declaration`s (entity + capabilities + `EntitySchema` + relations +
  fallback) — the records a plugin contributes/serves into D-07's index.
- **auth**: methods **by purpose** (`bot_token`, `api_token`, …) with env aliases + secret/sensitive
  fields; the host resolves a purpose → `flux-secret` material and can inject it (e.g. bearer) into a host
  HTTP call.
- **endpoints**: named, env-resolved base URLs (GitLab/Jira) — declared, resolved by the host.
- **capabilities**: the host-capability classes the plugin may use (`http`, `process`, `env`, `blob`,
  `conn`) — **deny-by-default**; the host authorizes calls against this declaration, no wire negotiation.

### Host capabilities (host-side, deny-by-default)
`http` (with **secret-by-purpose injection**), `process`, `env`, `blob`, `conn`. The trait stays in
`flux-plugin`; the concrete `SystemHostCaps` is extended (process plumbing reused). The **datasource**
host commands (`ds:records`/`ds:search`/`ds:get`) are serviced by an L5 impl
([`DatasourceHostCaps` in flux-capabilities](integration-plugins.md)) because the index is L5 — flux-plugin
defines only the trait + protocol.

### Plugin-binding SDK (Rust)
The analog of fluxplane's `pluginbinding`: typed `operation(spec, handler)` + `datasource(spec, handler)`
registration, manifest builders, a `serve()` loop over the one frame, and a host-call client. `EntitySchema`
derived from a record struct via `flux-datasource-derive`. A new plugin becomes "declare ops/datasources +
implement each against the vendor API."

## Cutover
`flux.plugin.v1` is **removed** (no dual mode). The `echo`/`caps` fixtures and the CLI call sites
(`load_plugin_tools`, `discover` in `crates/flux-cli/src/main.rs`) move to the new protocol. `flux-plugin`
stays **L4**; it gains a dep on the L0 `flux-datasource` crate (D-07) for the datasource/manifest types.

## Testing (hermetic)
- A fixture round-trips an op call, a `ds:records` contribution, and a host `secret`-by-purpose fetch over
  the single frame (no network).
- Capability-deny-by-default: an ungranted `http`/`secret` call is refused.
- Manifest → tool projection: an op's `access`/`effects` produce the expected permission subjects.

## Non-goals (v1)
- A `.dex`-style endpoint+grant+index registry (manifest data + config only).
- In-process plugins (everything is a subprocess — the flux model).
- `context.build` / `evidence.observe` plugin commands (flux already has `flux-evidence`; map on demand).

## Reuse, don't reimplement
- `flux-plugin`'s `PluginHost`/`serve` transport + `SystemHostCaps` process plumbing (extend).
- `flux-secret::Ref` + `SecretResolver` (secret-by-purpose). The `flux-datasource` types (D-07).
- Prior art (shapes only): `fluxplane-plugin/{protocol,manifest,pluginbinding}`.

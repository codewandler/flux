---
id: D-116
title: "Static endpoint wiring — `flux endpoint add` + config bindings that resolve"
pillar: Core
status: done
design: docs/designs/datasource-discoverability.md
epic: datasource-discoverability
note: "wiring a known service without k8s discovery today means hand-writing `flux endpoint import --from-json '<EndpointRef>'`, and even then only discovered @endpoint/* refs resolve — StaticResolver is constructed with an EMPTY bindings map ('No host config endpoint bindings are wired yet', main.rs:2185-2190)"
---

# Static endpoint wiring — `flux endpoint add` + config bindings that resolve

## Goal
An operator wires a known service (the canonical case: a Postgres database) in one command and the
agent can use it in the same or any later session: `flux endpoint add` writes a weak `EndpointRef`
to `~/.flux/endpoints.toml`, and statically-registered refs resolve at IO time like discovered ones
— closing the "wire it with an endpoint, then the agent can start using it" loop without a
Kubernetes provider in the picture.

## Acceptance
- [ ] Small in-story design pass first: the `flux endpoint add` flag surface
      (`<id> --product --url [--protocol] [--credential-ref <scheme:...>] [--label k=v]...`) and
      the host-config bindings TOML shape under `[endpoint]` — recorded here before implementation.
- [ ] Failing-first test: `flux endpoint add` persists a weak ref (never a secret; URL must be
      credential-free — reject inline `user:pass@` with a pointer to `--credential-ref`) and
      `flux endpoint list`/`show` render it.
- [ ] Failing-first test: a statically-registered/config-bound ref resolves through the
      `ReferenceResolver` chain at connect time (today `StaticResolver` gets an empty map and only
      discovered `@endpoint/*` refs resolve).
- [ ] End-to-end proof (gated like the pg tests, e.g. `TEST_POSTGRES_URL`): added Postgres endpoint
      → `endpoint.list` shows it → `sql.query {endpoint_ref}` connects via host-terminated SCRAM
      with the password sourced from the credential ref — never entering the plugin.
- [ ] `flux endpoint --help` + the D-117 website page document the new subcommand.

## Design (recorded 2026-07-10, before implementation)

Two operator surfaces, one resolver. The registry (`~/.flux/endpoints.toml`) is the runtime index;
the `StaticResolver` — today built with an empty map (`main.rs:2476`, `main.rs:6889`) — is fed from
the registry's config-bound (`source == Config`) records at session startup.

**1. `flux endpoint add` — the imperative surface (writes `~/.flux/endpoints.toml`).**

```
flux endpoint add <id> --url <url> \
    [--product <p>] [--protocol <proto>] [--credential-ref <ref>] [--label <k=v>]...
```

- `<id>` — the named reference id (a bare name, e.g. `pg-prod`). Reject an `@endpoint/…` id; that
  prefix is reserved for *discovered* refs.
- `--url` — bare `scheme://host[:port][/path]`, **credential-free**. Reject an inline `user[:pass]@`
  authority with a pointer to `--credential-ref` (the failing-first test).
- `--product` — product class (`postgres`, …); optional (drives group surfacing / display).
- `--protocol` — wire-protocol hint (`postgres`, `http`, …); optional.
- `--credential-ref` — the credential **location**, in `flux_secret::Ref::parse` form
  (`env/PGPASSWORD`, `kubernetes/<ns>/<name>/<key>`, `plugin/<p>/<i>/<slot>`); optional
  (unauthenticated when omitted). Never a value.
- `--label k=v` — repeatable non-secret labels.

Persists an `EndpointRecord::config` (owner `config`, `source == Config`) via
`EndpointRegistry::{put,save}` — a weak ref only. Idempotent overwrite by id.

**2. Host-config `[[endpoint.static]]` — the declarative surface (config alternative).**

```toml
[[endpoint.static]]
id = "pg-prod"
url = "postgres://db.example:5432/app"
product = "postgres"                 # optional
protocol = "postgres"                # optional
credential_ref = "env/PGPASSWORD"    # optional
labels = { region = "eu" }           # optional
```

Lives under the config `[endpoint]` table (alongside `cross_plugin_credentials`). `flux-config`
holds these as plain strings (a new `StaticEndpoint` struct — **no `flux-secret` dep**, keeping the
config crate a leaf and flux-capabilities config-surface-free per `broker.rs:221`); user+project
merge by concatenation with project entries overriding by id. At session startup each is converted
(shared validator, below) into a config-bound `EndpointRef` and `put` into the in-memory registry —
so it surfaces the endpoint group (D-115 `is_empty` check), lists, and resolves identically to an
`endpoint add` record. Not re-persisted to `endpoints.toml` on its own; if an in-session
`endpoint.import` snapshots the registry it rides along (the existing union-snapshot contract,
`registry.import`), which is acceptable for weak refs.

**3. Resolution.** New `EndpointRegistry::config_bindings() -> HashMap<String, EndpointRef>`
(filter `source == Config`, key by id). Both startup paths build
`StaticResolver::new(system, registry.config_bindings())` after `load()` + the config-static merge.
The broker already routes a named (non-`@endpoint/`) ref to the static resolver
(`broker.rs:735`) and materializes an `env`-scheme credential into a bearer header
(`StaticResolver::materialize`); Postgres auth is host-terminated SCRAM via the sql plugin's
`host.conn_authenticate` (D-31), which takes the `ResolvedEndpoint` + credential ref.

**4. Shared validator** `endpoint_ref_from_parts(id, url, product, protocol, credential_ref, labels)`
in `flux-cli` — rejects `@endpoint/` ids, credential-bearing URLs, and unparseable credential refs.
Used by both `endpoint add` and the config-static merge so the two surfaces validate identically.

**Test seams.** `run_endpoint` is refactored to `run_endpoint_in(path, action)` (mirrors
`run_plugin_in`) so the add/list/show tests use a temp store instead of `$HOME`. The resolution test
lives in flux-capabilities (config-bound record → `config_bindings()` → broker chain → resolved URL
+ materialized credential). The `TEST_POSTGRES_URL` e2e resolves an added Postgres ref through the
broker and asserts the bound URL + host-side credential materialization (the sql-plugin SCRAM leg
itself is D-31's already-tested contract).

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).
- 2026-07-10 design pass recorded (above); implementing in worktree `feat/d116-static-endpoint-wiring`.
- 2026-07-10 **DONE** (worktree `feat/d116-static-endpoint-wiring`, off main@`bc13310`). Shipped:
  `flux endpoint add` (validated weak-ref persist; refactored `run_endpoint` → testable
  `run_endpoint_in`); `[[endpoint.static]]` config surface (`flux_config::StaticEndpoint` +
  user/project merge-by-id); both startup paths now build the `StaticResolver` from
  `EndpointRegistry::config_bindings()` after a `merge_static_endpoints` pass (was an empty map).
  Tests: flux-config parse+merge (2), flux-capabilities `config_bindings` + broker-chain resolution
  (2), flux-cli add/reject/validator/config-merge (4) + a `TEST_POSTGRES_URL`-gated e2e resolving the
  sql plugin's default `sql.endpoint` ref through the broker (URL bind + host-side credential
  materialize). Live CLI smoke passed (add/list/resolve; store holds a location, never a value).
  Gate green: fmt, `clippy -D warnings`, `cargo test --workspace`, flux-codegate. WHATS-NEW +
  CHANGELOG updated (additive → patch bump next cut). **`flux endpoint --help` done; the website
  concept page + CLI reference is D-117's scope (paired follow-up).** Not committed (awaiting review).

## Notes
- Import-only today: `EndpointAction` (`crates/flux-cli/src/main.rs:657-681`), handler
  `run_endpoint` (main.rs:6190).
- Resolver seam: `StaticResolver` (`crates/flux-capabilities/src/endpoint/mod.rs:178`), empty-map
  construction + TODO comment at `crates/flux-cli/src/main.rs:2185-2190`.
- Reference invariant (design `docs/designs/endpoint-discovery.md`): weak refs only, credential is
  a *location* (`flux_secret::Ref` schemes env/plugin/kubernetes), host injects at IO time.
- Depends on nothing; pairs naturally with D-115 (the added endpoint should surface the ops).

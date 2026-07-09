---
id: D-116
title: "Static endpoint wiring — `flux endpoint add` + config bindings that resolve"
pillar: Core
status: ready
priority: 22
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

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).

## Notes
- Import-only today: `EndpointAction` (`crates/flux-cli/src/main.rs:657-681`), handler
  `run_endpoint` (main.rs:6190).
- Resolver seam: `StaticResolver` (`crates/flux-capabilities/src/endpoint/mod.rs:178`), empty-map
  construction + TODO comment at `crates/flux-cli/src/main.rs:2185-2190`.
- Reference invariant (design `docs/designs/endpoint-discovery.md`): weak refs only, credential is
  a *location* (`flux_secret::Ref` schemes env/plugin/kubernetes), host injects at IO time.
- Depends on nothing; pairs naturally with D-115 (the added endpoint should surface the ops).

---
id: A-126
title: Fleet discovery — agents as a product of the endpoint broker that already exists
pillar: Agent
status: ready
priority: 13
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-orchestrate, flux-capabilities]
note: "C-243's ExternalRuntime exists but is not CLI-wired; project endpoint-broker agent refs into that shipped worker vocabulary rather than inventing flux-fleet/AgentAddress"
---

# Fleet discovery — agents as a product of the endpoint broker that already exists

## Goal
Answer "which agents exist?" — a question nothing in flux can answer today — without building a
second discovery mechanism. The endpoint broker already fans a query out to provider plugins and
returns weak refs carrying `labels` and `credential_ref` and never a secret
(`crates/flux-capabilities/src/endpoint/broker.rs`, `discover` at `:426`). Agents become a
`product` on it.

## Acceptance
- [ ] Static roster: `[[endpoint.static]]` entries in `~/.flux/endpoints.toml`
      (`crates/flux-config/src/lib.rs:125`) with `product = "agent"` and an A2A URL resolve to
      externally managed fleet members. No new config file, config section, or runtime-selecting URI.
- [ ] Dynamic: `endpoint.discover { product: "agent" }` reaches provider plugins, so a kubernetes
      provider can enumerate live pods as agents.
- [ ] `fleet.list` op — a projection over the broker filtered to agents, returning stable worker
      reference, endpoint, runtime kind, liveness and labels. It wires C-243's `ExternalRuntime`
      into the CLI. Failing-first: static and provider-discovered agents appear in one answer with
      their sources distinguishable.
- [ ] Failing-first test: **filtering by label selects a cluster** — two agents differing only in a
      `cluster` label are separable, which is the whole multi-cluster story and needs no cluster
      object.
- [ ] Failing-first test: a discovered agent's `credential_ref` is **never** resolved to a value in
      `fleet.list` output; the weak-ref invariant the broker already enforces survives the agent
      projection.
- [ ] A discovered agent's advertised endpoint is **re-guarded** before use:
      `A2aClient::adopt_endpoint` (`crates/flux-a2a/src/client.rs:115`) adopts an arbitrary
      cross-origin URL a card advertises, which for a *discovered* fleet is a redirection surface.

## Progress

- 2026-08-02: respecified against C-243's opaque worker id and `ExternalRuntime`; promoted to ready
  without a `flux-fleet` crate or `AgentAddress` dependency.

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "Discovery".
- Depends on C-243, which is done.
- Note for review: the A2A card route leaks existence pre-auth by design (spec §13.1 constant 404);
  discovery must not make that worse by enumerating agents to an unauthenticated caller.

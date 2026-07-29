---
id: A-126
title: Fleet discovery — agents as a product of the endpoint broker that already exists
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet, flux-capabilities]
note: "no second discovery mechanism — the broker's provider fan-out means a new pod becomes visible with no config edit"
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
      (`crates/flux-config/src/lib.rs:125`) with `product = "agent"` and an `AgentAddress` in `url`
      resolve to fleet members. No new config file and no new config section.
- [ ] Dynamic: `endpoint.discover { product: "agent" }` reaches provider plugins, so a kubernetes
      provider can enumerate live pods as agents.
- [ ] `fleet.list` op — a projection over the broker filtered to agents, returning address, status
      and labels. Failing-first test: it returns both static and provider-discovered agents in one
      answer, with the source of each distinguishable.
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
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "Discovery".
- Depends on A-120, A-121.
- Note for review: the A2A card route leaks existence pre-auth by design (spec §13.1 constant 404);
  discovery must not make that worse by enumerating agents to an unauthenticated caller.

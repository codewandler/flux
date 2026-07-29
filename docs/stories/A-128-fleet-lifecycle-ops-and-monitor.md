---
id: A-128
title: fleet.start/.stop/.status ops and the cluster-monitor journey
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet, flux-app]
note: "the epic's headline proof — start a discovered worker, dispatch to it, watch it through Ready/Busy/Exited, stop it, offline in CI"
---

# fleet.start/.stop/.status ops and the cluster-monitor journey

## Goal
Put the runtime port in the agent's hands and in the coordinator's sweep: model-facing
`fleet.start` / `fleet.stop` / `fleet.status` ops over `AgentRuntime`, and a monitor journey that
reconciles the fleet the way the [coordinator's sweep](../designs/fleet-coordinator.md) reconciles
the board. This is where "the coordinator can monitor and control the cluster" becomes true.

## Acceptance
- [ ] `fleet.start` / `fleet.stop` / `fleet.status` ops in `flux-fleet`, dispatching to the runtime
      resolved from the address's scheme.
- [ ] Accurate `effects`, `Risk`, `Idempotency` and **concrete `permission_subjects`** — the
      resolved program path / image ref / workload, never `*` and never empty. `fleet.start` on a
      `proc://` address is `bash`-class power and is gated as such (A-122).
- [ ] A monitor journey on a `schedule` channel: for each known agent, `fleet.status`; an `Exited`
      or `Unreachable` worker holding a `Claimed`/`InProgress` board item releases that item back to
      `Ready` so it is re-dispatched.
- [ ] **Failing-first test — the epic's headline proof, offline:** a coordinator discovers a worker
      (A-126), starts it on the `proc://flux` runtime, dispatches a board item, observes
      `Starting → Ready → Busy`, kills it, and the monitor journey moves the item back to `Ready`.
      No credentials, no network.
- [ ] Failing-first test: a worker whose task still reports `working` but whose runtime reports
      `Exited` is treated as **failed**. This is the concrete trap: an in-flight task at restart
      reports `working` forever (`crates/flux-server/src/a2a.rs:1195-1199`) and the TTL sweep that
      would clear it is lazy, running only at the next mint.
- [ ] Multi-cluster works by label filtering alone — the monitor journey over two label sets touches
      only its own agents.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md).
- Depends on A-121, A-122, A-126; the board interaction depends on A-113 and A-130.
- Deliberately not here: restart policy, autoscaling, load balancing. See the design's "What this
  does not attempt".

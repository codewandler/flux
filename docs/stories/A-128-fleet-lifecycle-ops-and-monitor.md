---
id: A-128
title: Fleet monitor journey over the shipped worker lifecycle
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-orchestrate, flux-app]
note: "C-243 shipped fleet.start/worker_status/stop; the remaining headline proof joins discovery, dispatch/task status and worker liveness in one offline monitor journey"
---

# fleet.start/.stop/.status ops and the cluster-monitor journey

## Goal
C-243 put `fleet.start`, `fleet.worker_status`, and `fleet.stop` over `AgentRuntime` in the agent's
hands. Finish the coordinator sweep that joins worker liveness to `fleet.dispatch` / `fleet.status`
task state and reconciles the fleet the way the
[coordinator's sweep](../designs/fleet-coordinator.md) reconciles the board. This is where "the
coordinator can monitor and control the cluster" becomes true.

## Acceptance
- [x] `fleet.start` / `fleet.worker_status` / `fleet.stop` dispatch through the shipped runtime.
      `fleet.status` remains the separate A2A task-status operation; the names must not be conflated.
- [x] The shipped process lifecycle ops declare accurate effects/risk/idempotency and concrete
      permission subjects through C-243's reviewed guarded-spawn contract.
- [ ] A monitor journey on a `schedule` channel: for each known agent, `fleet.status`; an `Exited`
      or `Unreachable` worker holding a `Claimed`/`InProgress` board item releases that item back to
      `Ready` so it is re-dispatched.
- [ ] **Failing-first test — the epic's headline proof, offline:** a coordinator discovers or starts
      a process worker (A-126/C-243), dispatches a board item, observes a live worker plus a working
      task, kills it, and the monitor journey observes `WorkerState::Dead` and moves the item back to
      `Ready`. No credentials or external network.
- [ ] Failing-first test: a worker whose task still reports `working` but whose runtime reports
      `Exited` is treated as **failed**. This is the concrete trap: an in-flight task at restart
      reports `working` forever (`crates/flux-server/src/a2a.rs:1195-1199`) and the TTL sweep that
      would clear it is lazy, running only at the next mint.
- [ ] Multi-cluster works by label filtering alone — the monitor journey over two label sets touches
      only its own agents.

## Progress

- 2026-08-02: narrowed to the remaining monitor journey. C-243 already shipped the three worker
  lifecycle operations under their exact public names.

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md).
- Depends on A-126; the lifecycle baseline is C-243 and the board interaction depends on A-113 and
  A-130 (all but A-126 are done).
- Deliberately not here: restart policy, autoscaling, load balancing. See the design's "What this
  does not attempt".

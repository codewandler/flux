---
id: A-119
title: "Agent fleet runtime — addressing, lifecycle and discovery for a fleet of agents (epic)"
pillar: Agent
status: in-progress
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
note: "EPIC — C-243 shipped the AgentRuntime port plus process/external workers; Docker/Kubernetes placement, address vocabulary and discovery remain"
---

# Agent fleet runtime — addressing, lifecycle and discovery for a fleet of agents (epic)

## Goal
Make a fleet of agents something flux can **start, stop, observe and find** — across local
processes, docker containers and kubernetes pods — so the [fleet
coordinator](../designs/fleet-coordinator.md) has workers to dispatch to instead of URLs a human
typed into a config file.

Two axes, deliberately not conflated: the **runtime** owns the process (external / proc / docker /
k8s), the **transport** owns the conversation (a2a over HTTP, ndjson over stdio). One URI carries
both — the scheme picks the runtime, the transport is defaulted per scheme and overridable with
`?proto=`.

## Acceptance
- [x] A design doc (`docs/designs/agent-fleet-runtime.md`) covering the address vocabulary, the
      `AgentRuntime` port and its four backends, the transport axis, discovery via the existing
      endpoint broker, the roles/fleet unification, and the safety envelope — each claim about the
      current tree pinned at `file:line`.
- [x] The epic is broken into implementation stories (A-120…A-128); each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: a coordinator starts a worker from an address it discovered, dispatches a
      board item to it, watches it reach `Ready` → `Busy` → `Exited`, and stops it — **offline**, on
      the `proc://flux` runtime, in CI.

## Progress
- 2026-07-29 — **design done**: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md).
  Grounded by two exhaustive sweeps of the tree. The two findings that shaped it:
  - **Lifecycle: nothing exists.** Every A2A-reachable agent is an `Arc<FlowEngine>` inside one
    foreground `flux` process a human started. `crates/flux-channels/src/host.rs:63-78` is the
    entire supervision story and a fatal channel error kills the process with no restart. No
    Dockerfile, no unit file, no manifest, no `--daemon`; flux never spawns `flux`. `GET /health`
    exists and nothing consumes it. D-63's multi-agent mount is implemented with **no production
    caller**.
  - **Discovery: nothing exists either — except for services.** The endpoint broker
    (`crates/flux-capabilities/src/endpoint/broker.rs`, D-25…D-32) already does exactly the
    fan-out we want — ask the host "which endpoints exist for product X", get back weak refs with
    `labels` and `credential_ref` and never a secret. Reusing it means the k8s plugin can enumerate
    live pods as agents with no new mechanism.
- 2026-08-02 — C-243 superseded A-120/A-121/A-122's proposed crate/address-first cut and shipped
  `AgentRuntime` in `flux-runtime`, `ProcessRuntime` + `ExternalRuntime` in `flux-orchestrate`, and
  the `fleet.start`/`fleet.worker_status`/`fleet.stop` operations. The epic is now in progress rather
  than backlog. A-124/A-125 are unblocked and promoted to ready; address/discovery work remains.

## Notes
- The 2026-07-29 design chose endpoint-broker discovery, an NDJSON/stdio path for foreign CLI
  agents, and an optional remote target on roles. C-243 later rejected the runtime-selecting URI and
  new-crate part of that design; the remaining stories build on its opaque worker id and
  `AgentRuntime` instead.
- Lifecycle stays split across L2 `flux-runtime` and L5 `flux-orchestrate`; there is no
  `flux-fleet` crate to add. The in-flight [fleet coordinator](fleet-coordinator.md) stories
  (A-112/A-113/A-116) stay where they are.
- Current order: {A-123, A-124, A-125, A-126} → A-127 → A-128.

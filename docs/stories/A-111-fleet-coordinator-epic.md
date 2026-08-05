---
id: A-111
title: "Remote fleet transports extend the native local coordinator later (epic)"
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
note: "Decision 0010 moved the product coordinator to C-239/A-117 and local Flux sub-agents; this epic now holds later remote/A2A extensions only"
---

# Remote fleet transports extend the native local coordinator later

## Goal

After the Decision 0010 local board/fleet product is proven, extend its stable worker/runtime ports to
remote agents without changing BoardRef identity, handoff evidence, rework, gating or CLI semantics.

## Acceptance

- [ ] The native local coordinator C-239/A-117 is complete and dogfooded before this epic is promoted.
- [ ] Remote workers provide verifiable filesystem isolation and exact artifact transfer; a remote
      branch name or model claim is never accepted as a local commit.
- [ ] A2A lifecycle, discovery, authentication, cancellation and retained-task behavior compose with
      the existing durable manifest and acknowledgement contract.
- [ ] Vendor board bindings remain the separate Decision 0006 connector/Exchange line.
- [ ] Offline and networked contract tests prove remote and local workers produce the same typed
      coordinator-visible handoff where their transport capabilities overlap.

## Progress

- 2026-08-05 — respecified by Decision 0010. Delivered WorkBoard/A2A primitives remain foundations;
  the former `coordinator.flux` product proof is superseded by the native CLI supervisor.

## Notes

- A-123…A-126 and the agent-fleet-runtime design hold the deferred transport/runtime work.

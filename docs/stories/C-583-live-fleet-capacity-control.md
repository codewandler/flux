---
id: C-583
title: "Scale Fleet worker capacity through one operation and CLI contract"
pillar: Core
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-cli, flux-runtime, flux-orchestrate, flux-tui]
depends_on: [C-570, C-571]
note: "one Fleet worker is one admitted agent; revisioned desired capacity scales assignment-bound workers up and drains down, while nested task children stay separate"
---

# Scale Fleet worker capacity through one operation and CLI contract

## Goal

Let the main coordinator or operator change live Fleet capacity without editing configuration or
restarting the supervisor, while preserving one admitted agent per worker and every assignment,
budget and ownership fence.

## Acceptance

- [ ] Failing first, a running Fleet exposes configured `max_workers` but has no revisioned operation
      or ergonomic CLI command to change desired live capacity, drain excess capacity or explain why
      fewer workers can run.
- [ ] One typed `fleet.scale` service is projected as an operation for the main coordinator,
      `flux fleet scale --workers N`, `flux fleet call scale --request ...`, schema, JSON/human output
      and later TUI controls; all paths share idempotency, optimistic revision and acknowledgement
      semantics.
- [ ] `desired_workers` is revisioned and constrained by configured `max_workers`, template instance
      caps, available dependency-satisfied assignments and C-571 reservations. Scaling up admits only
      explicitly selected or scheduled Board assignments; it never creates an idle nameless agent or
      a second coordinator.
- [ ] Scaling down is drain-by-default: the coordinator stops replacement/admission and converges at
      safe C-570 yield or terminal boundaries without discarding a worker's assignment, worktree,
      session, loop binding, handoff or usage settlement. Forced interruption remains the existing
      explicit targeted `cancel`, never a hidden side effect of `scale`.
- [ ] Status/events expose configured maximum, desired, admitted, active, draining, queued and
      budget-limited capacity with exact reasons. Restart/replay reconstructs the same target and
      convergence state without counting terminal agents as live.
- [ ] The Fleet main agent alone receives capacity authority by default. Story workers cannot scale,
      spawn Fleet members, mark themselves drained or widen their own template/budget through prose.
- [ ] Contracts and tests distinguish Fleet workers from nested `task` children: one worker is one
      admitted agent/session; a permitted task child receives no Fleet identity or independent writer
      worktree and is bounded by a separate snapshotted child-concurrency/depth limit that cannot
      widen during the worker's lifetime.
- [ ] A hermetic fixture scales from one to five independent ready assignments, drains back to two,
      survives restart mid-drain and proves one writer/worktree per story, no oversubscribed budget,
      no lost handoff and deterministic event/JSON output.

## Progress

- 2026-08-05 — contracted from operator feedback; implementation has not started.

## Notes

- `max_workers` remains the hard configured ceiling; `desired_workers` is the live control target.
- C-573 may later drive this same actuator from metrics within policy. It must not invent another
  concurrency control path.

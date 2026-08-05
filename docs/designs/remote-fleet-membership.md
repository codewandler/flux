# Design — Authenticated remote fleet membership and task workers

**Status:** proposed follow-up · **Stories:**
[C-554](../stories/C-554-remote-fleet-admission-and-leases.md),
[C-555](../stories/C-555-a2a-task-agent-backend.md)

## Why

An A2A endpoint being discoverable does not make it a trusted fleet worker. Remote membership needs
an explicit coordinator-owned admission protocol before the task backend may dispatch work.

## Admission

The main coordinator creates a bounded invitation. The remote agent answers with an authenticated
hello, stable identity, capabilities, task modes, fence posture and lease request. The coordinator
validates policy and records admission or refusal. Leases expire and renewal never changes identity
or widens capabilities implicitly.

## Execution

The A2A task-agent backend maps the generic lifecycle to remote task start, acknowledged steering,
status, cancellation and result receipts. Exact commit/artifact transport is independently verified
before a remote handoff can enter local integration. A network identity never substitutes for a
BoardRef, worker admission or exact evidence.

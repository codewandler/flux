---
id: A-52
title: Outbound Task fidelity — populate Task.history/historyLength and emit artifacts
pillar: Agent
status: backlog
priority:
epic: a2a-conformance
design: docs/designs/a2a-conformance.md
note: "Tier-2: Task.history/artifacts and TaskArtifactUpdateEvent are modeled and client-decodable but never produced by the server"
---

# Outbound Task fidelity

## Goal
Make the `Task` flux returns richer and spec-faithful: populate conversation `history` (honoring the
client's `historyLength`) and emit structured outputs as A2A artifacts, so clients that render task
history or consume artifacts get real data instead of empty arrays.

## Why (evidence)
- `crates/flux-a2a/src/types.rs:214-228` `Task` has `history`/`artifacts`; the server always builds via
  `Task::new` (`types.rs:238-240`, used at `crates/flux-server/src/a2a.rs:424`) which leaves both
  empty. Clients already read them (`types.rs:246-261` prefers artifacts).
- `TaskArtifactUpdateEvent` (`types.rs:298-312`) is client-decodable but the server never emits an
  artifact-update frame (only `TaskStatusUpdateEvent`, `a2a.rs:512-545`).
- `configuration.historyLength` is not modeled (`SendConfiguration` has only `blocking`,
  `types.rs:543-548`) and would be ignored.

## Acceptance
- [ ] The returned/streamed `Task` carries `history` from the engine's conversation projection,
      truncated to `configuration.historyLength` when the client sets it (add the field to
      `SendConfiguration`).
- [ ] Structured (non-text) agent outputs are emitted as `Task.artifacts` and, in streaming mode, as
      `TaskArtifactUpdateEvent` frames.
- [ ] Failing-first tests: a two-turn `contextId` conversation returns non-empty `history` bounded by
      `historyLength`; a turn producing a structured output yields an artifact (blocking) and an
      `artifact-update` frame (streaming).
- [ ] Docs: the Task lifecycle + Streaming events rows in the support matrix update.

## Progress
- (not started)

## Notes
- Still within the synchronous-turn model (no task retention needed). The `input-required`/async
  states are Tier-3 (A-53). Epic: [a2a-conformance](../designs/a2a-conformance.md).

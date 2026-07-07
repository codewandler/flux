---
id: A-52
title: Outbound Task fidelity — populate Task.history/historyLength and emit artifacts
pillar: Agent
status: done
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
- **Done**, in two wired pieces plus a documented scope boundary.
- **History (flux-server):** `SendConfiguration` gained `history_length: Option<u32>` (wire
  `historyLength`, serialized only when set). A blocking `message/send` now populates `Task.history`
  from the engine's conversation projection (`EventStore::conversation`) via a private `a2a_history`
  helper: each user/agent message → an A2A `Message` (system messages and text-less tool-only turns
  dropped), capped to the most-recent `historyLength` when the client sets it. Read via the shared
  `flux_a2a::server::history_length` extractor. Projection failure degrades to empty history rather
  than failing the (already-successful) turn.
- **Artifacts (reusable seam):** the `A2aTurn` runner's structured (non-text) reply parts
  (`A2aReply.extra_parts`) now become `Task.artifacts` (one grouped `Artifact` with a minted
  `artifactId`) instead of hitchhiking on `status.message` — the spec-faithful home (clients read
  artifacts first). A plain text answer yields `artifacts: []`. Added the reusable
  `flux_a2a::server::artifact_update_value` streaming frame shaper (mirrors `status_update_value`)
  for `TaskArtifactUpdateEvent`.
- **Scope boundary (documented in the epic design):** flux's engine emits only text — there is no
  structured-output channel in `AgentSink`. So artifacts are a capability of the reusable `A2aTurn`
  server seam; flux-server's built-in text agent produces none, so its tasks carry `history` but
  empty `artifacts` (correct, not a gap). Native flux-server artifact emission would first need an
  engine structured-output seam — out of scope, noted as a future follow-up.
- **Tests (failing-first):** `flux_a2a::server` unit tests `rich_replies_surface_data_parts_as_artifacts`
  (structured part → artifact; message text-only), `message_send_returns_a_completed_task_*` (text-only
  → `artifacts: []`), `artifact_update_value_shapes_a_frame`, `history_length_reads_configuration`;
  `flux-server` integration test `task_history_is_populated_and_bounded` (two-turn `contextId`
  conversation → history accumulates; `historyLength=2` caps to the most-recent two, newest kept).
- **Docs:** Task-lifecycle + Streaming-events rows (contributor + website) updated.

## Notes
- Still within the synchronous-turn model (no task retention needed). The `input-required`/async
  states are Tier-3 (A-53). Epic: [a2a-conformance](../designs/a2a-conformance.md).

---
id: D-147
title: Flow-driven sessions front door — Session::start_flow + suspended
pillar: Agent
status: ready
priority: 6
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 2 — the D-131 differentiator reaches the SDK; durable await across restarts"
---

# Flow-driven sessions front door — Session::start_flow + suspended

## Goal
Expose the flow-driven session primitive (D-131) through the SDK: `Session::start_flow(&DraftAst)`
runs an authored flow to its first top-level `await` and surfaces the flow's authored prompt;
`send()` resumes; suspension state is queryable — durable human-in-the-loop that survives a
process restart.

## Acceptance
- [ ] Failing-first: an authored flow with two `await`s — `start_flow` returns the authored
      prompt with `suspended: true`; `send("answer")` resumes to the next prompt; completing the
      flow flips `suspended: false`.
- [ ] A process restart between the two awaits (drop client, rebuild over `Storage::dir`)
      still resumes via `open_session` + `send`.
- [ ] `TurnOutput` gains `suspended: bool` and `#[non_exhaustive]` (MINOR; flag in CHANGELOG).
- [ ] `FlowClient`'s `finish_outcome` suspension error retargets its message at
      `Session::start_flow` (`crates/flux-sdk/src/flow.rs:637`).

## Progress
- (pending)

## Notes
- Engine seams: `start_flow_turn` (`crates/flux-flow/src/engine.rs:1095`), suspension-first
  resume branch (`:302-306`), `FlowStore::has_suspension`. Depends on D-142.

---
id: D-145
title: Owned event stream — AgentEvent + TurnStream with cancel/finish
pillar: Agent
status: ready
priority: 4
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 1 — async-iterator streaming over a spawned cancellable turn"
---

# Owned event stream — AgentEvent + TurnStream with cancel/finish

## Goal
The idiomatic streaming shape: `Session::stream(input) -> TurnStream`, a `futures::Stream` of
owned `AgentEvent`s (an enum mirroring `AgentSink` 1:1, `#[non_exhaustive]`) with `cancel()` and
`finish() -> TurnOutput`, so embedders consume turns without implementing a sink trait.

## Acceptance
- [ ] `AgentEvent` covers every `AgentSink` method (text/thinking/planning/plan deltas, tool
      call/result, observation, turn end) — a doc-comment ties each variant to its sink method.
- [ ] Failing-first: `Session::stream` yields `TextDelta` items live (slow two-chunk mock:
      first item arrives before the provider emits the second).
- [ ] `stream.cancel()` mid-tool-call ends the turn; exactly one persisted assistant message
      (valid `user → assistant` alternation via `history()`).
- [ ] `finish()` drains and returns the same `TurnOutput` a plain `send` would have.
- [ ] `tokio` promoted to a real dependency (spawn); channel sink forwards owned events.

## Progress
- (pending)

## Notes
- New `crates/flux-sdk/src/events.rs`; `Session::stream` spawns `run_turn_cancellable`
  (possible: `Session` holds `Arc<FlowEngine>`, `run_turn*` takes `&self`).
- Depends on D-142; composes with D-144's sink door (one implementation, two shapes).

---
id: D-145
title: Owned event stream — AgentEvent + TurnStream with cancel/finish
pillar: Agent
status: done
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
- 2026-07-11: implemented in new `src/events.rs` — `AgentEvent` (`#[non_exhaustive]`, one variant
  per `AgentSink` method), `TurnStream` (impls `futures::Stream` + `next`/`cancel`/`finish`),
  `ChannelSink` forwards owned events over an unbounded mpsc while collecting the output.
  `Session::stream` spawns `run_turn_cancellable` (the turn guard is acquired INSIDE the spawned
  task so the stream handle returns immediately yet turns still serialize). `futures` +
  `flux-evidence` promoted to real deps. Tests: gated-mock live-delivery (first delta observable
  while provider parked on a semaphore), cancel-mid-parked-tool leaves a valid alternation +
  closing assistant message.

## Notes
- New `crates/flux-sdk/src/events.rs`; `Session::stream` spawns `run_turn_cancellable`
  (possible: `Session` holds `Arc<FlowEngine>`, `run_turn*` takes `&self`).
- Depends on D-142; composes with D-144's sink door (one `events.rs`, two shapes: `TeeSink` for
  bring-your-own-sink, `ChannelSink` for the owned-event stream).

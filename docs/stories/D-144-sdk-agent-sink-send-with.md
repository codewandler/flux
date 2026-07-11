---
id: D-144
title: Streaming, bring-your-own-sink — AgentSink re-export + Session::send_with
pillar: Agent
status: ready
priority: 3
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 1 — the minimal streaming door: consumer sink + cancellation token"
---

# Streaming, bring-your-own-sink — AgentSink re-export + Session::send_with

## Goal
Let an embedder observe a live turn: re-export `flux_flow::AgentSink` and add
`Session::send_with(input, sink, cancel)` so text deltas, tool calls, and tool results stream to
consumer code as they happen (today's private `Collector` drops tool results entirely).

## Acceptance
- [ ] `flux_sdk::AgentSink` re-exported; `Session::send_with(&self, input, &mut dyn AgentSink,
      &CancellationToken) -> Result<TurnOutput>` drives `run_turn_cancellable`.
- [ ] Failing-first: a consumer sink receives `text_delta` before `send_with` returns, and
      receives `tool_result` for a dispatched op (the drop-tool_results bug class).
- [ ] Cancelling the token mid-turn ends the turn with a valid `user → assistant` session shape
      (AGENTS.md invariant; assert via `history()`).
- [ ] `CancellationToken` re-exported (tokio-util already a dep).

## Progress
- (pending)

## Notes
- `crates/flux-sdk/src/session.rs`; engine seam `crates/flux-flow/src/engine.rs:293`.
- Depends on D-142 (Session).

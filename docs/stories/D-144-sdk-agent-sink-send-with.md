---
id: D-144
title: Streaming, bring-your-own-sink — AgentSink re-export + Session::send_with
pillar: Agent
status: done
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
- [x] `flux_sdk::AgentSink` re-exported; `Session::send_with(&self, input, &mut dyn AgentSink,
      &CancellationToken) -> Result<TurnOutput>` drives `run_turn_cancellable` via a `TeeSink`.
- [x] Failing-first: a consumer sink receives `tool_result` for a dispatched op (the
      drop-tool_results bug class); live `text_delta` covered by the D-145 stream test.
- [x] Cancelling mid-turn ends the turn with a valid `user → assistant` session shape (asserted
      via `history()` role-alternation + closing-assistant check).
- [x] `CancellationToken` re-exported at the crate root.

## Progress
- 2026-07-11: implemented alongside D-145 in `src/events.rs` (`TeeSink` forwards to the consumer
  sink while collecting the `TurnOutput`) + `Session::send_with`. `AgentSink` + `CancellationToken`
  re-exported from the crate root. Test `send_with_streams_deltas_and_tool_results_to_a_consumer_sink`.

## Notes
- `crates/flux-sdk/src/session.rs` + `src/events.rs`; engine seam `crates/flux-flow/src/engine.rs:293`.
- Depends on D-142 (Session). Shipped in the same commit as D-145 (shared `events.rs`).

---
id: D-158
title: FlowClient streaming — execute_with_sink + execute_streamed
pillar: Agent
status: done
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 4 — flow runs stop being observability-blind (ExecSink drops everything but names)"
---

# FlowClient streaming — execute_with_sink + execute_streamed

## Goal
Flow executions become observable: `FlowClient::execute_with_sink(ast, sink)` streams every
dispatch to a consumer `AgentSink`; `execute_streamed(ast)` is the owned-`AgentEvent` variant
(same machinery as D-145).

## Acceptance
- [x] Failing-first: the sink receives `tool_result` for every dispatched op in an `execute`
      (today's `ExecSink` records only names, `crates/flux-sdk/src/flow.rs:613-622`).
- [x] `execute_streamed` yields events live during a slow mock op.
- [x] `execute`/`execute_with` behavior unchanged (existing tests green).

## Progress
- **Done (unreleased).** `FlowClient::execute_with_sink(ast, sink)` (`crates/flux-sdk/src/flow.rs`)
  runs `execute_flow` through a `TeeSink` (consumer + a `Collector` for op names), so the consumer
  gets full `tool_call`/`tool_result`/text/observation events (the runtime already emits them;
  `ExecSink` just dropped all but names). `execute_streamed(ast)` spawns the run over a `ChannelSink`
  and returns a new `FlowStream` (in `events.rs`, mirrors `TurnStream`: `futures::Stream` + `next()` +
  `finish() -> ExecutionResult`; no cancel token — a flow run has none). Both reuse `finish_outcome` +
  `cognition_usage`, so `usage`/`result`/`steps` match `execute`.
- `FlowClient.store` changed `FlowStore` → `Arc<FlowStore>` (FlowStore isn't `Clone`) so the spawned
  run shares it; every direct `&self.store` path deref-coerces unchanged. `FlowStream` re-exported at
  the crate root.
- Failing-first tests: `execute_with_sink_streams_tool_results` (a `SlowTool` op → the consumer sink
  records both `tool_call` and `tool_result`, which `execute`'s collector drops) and
  `execute_streamed_yields_events_then_finishes` (the stream delivers the op's ToolCall+ToolResult
  live, then `finish()` returns the same result). `execute`/`execute_with` tests unchanged/green.
- CHANGELOG + WHATS-NEW + website mirror updated. Gate green (workspace 2171; clippy all-features /
  fmt / codegate). **Only D-159 (datasource recipe doc) remains in the epic. Not committed/released.**

## Notes
- Depends on D-145 (AgentEvent). Seeding paths (`execute_with`) keep their fresh-store isolation.

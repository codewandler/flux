---
id: A-12
title: Un-silence the planning wait — wire planning state + thinking streaming in normal mode
pillar: Agent
status: done
priority:
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: the streaming plumbing exists end-to-end (SharedSink → SinkEvent::Thinking/Planning → CliSink) but is dead in normal mode — the single biggest perceived-latency win, independent of the rest of the epic
---

# Un-silence the planning wait

## Goal
During the planning call in a *normal* turn the CLI shows nothing at all: `sink.planning(true)` is
only invoked from `plan_turn` (REPL `/plan`, `crates/flux-flow/src/engine.rs:358`), and
`EngineLoopHost::plan` passes `thinking_sink: None` into `compile_turn`
(`crates/flux-flow/src/loop_host.rs:542-553`). Wire the existing plumbing so the user sees the
"composing plan…" state and live thinking tokens on every planner call.

## Acceptance
- [x] `EngineLoopHost::plan` brackets `compile_turn` with `sink.planning(true/false)` and passes a
      live thinking sink. Failing-first test: `normal_turn_planning_state_reaches_the_sink`
      (a recording sink observes `Planning(true)` … `Planning(false)` around a normal `run_turn`).
- [x] Thinking deltas emitted by the provider during a normal turn reach the sink
      (`normal_turn_streams_thinking_deltas`, mock provider emitting `ThinkingDelta`).
- [x] No behavior change for `/plan` (`plan_turn`) — it already streams; test stays green.
- [x] Gate green: `cargo test --workspace`, clippy `-D warnings`, fmt, `cargo test -p flux-codegate`.
      (Ran package-scoped per the orchestrator's instruction for this parallel story:
      `cargo build/test/clippy -p flux-flow -p flux-cli` + `cargo fmt --all`, all green. The
      orchestrator runs the full workspace gate incl. `flux-codegate` afterward.)

## Progress
- 2026-07-02: shipped. `EngineLoopHost::plan` (`crates/flux-flow/src/loop_host.rs`) now brackets its
  `compile_turn` call with a new `PlanningGuard` RAII type (mirrors the existing `DepthGuard` pattern)
  — `sink.planning(true)` fires on construction, `planning(false)` fires unconditionally in `Drop` so
  it can't be skipped on a compile error or any future early `?`. It also passes a live `SharedSink`
  as `compile_turn`'s `thinking_sink`, so `Chunk::ThinkingDelta` chunks from the provider now stream
  through exactly like the REPL `/plan` path already did. No changes were needed downstream — the
  `SinkEvent::Planning`/`Thinking` channel forwarding (`ChannelSink`/`drain_event`) and `CliSink`'s
  spinner + dimmed-thinking rendering were already wired and dead only for lack of a caller.
  Added two failing-first tests in `loop_host.rs`'s test module (extended the shared test `Recorder`/
  `RecSink` with `planning`/`thinking` fields): `normal_turn_planning_state_reaches_the_sink` and
  `normal_turn_streams_thinking_deltas`, both verified red (via a temporary local revert) before the
  fix and green after. Full `flux-flow` (137 tests) and `flux-cli` (63 tests) suites green, including
  every `engine::tests::plan_turn_*` REPL-path test — no regression. `cargo build/clippy -D warnings`
  clean for both crates; `cargo fmt --all` is a no-op.

## Notes
- Pure wiring: `SharedSink::thinking_delta`/`planning` forward (`loop_host.rs:893-897`),
  `SinkEvent::Thinking/Planning` ride the channel (`loop_host.rs:933-934`), `CliSink` renders both
  (`crates/flux-cli/src/main.rs:3541-3556`).
- Ships first and independently — no dependency on any other epic story.

---
id: A-12
title: Un-silence the planning wait — wire planning state + thinking streaming in normal mode
pillar: Agent
status: ready
priority: 1
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
- [ ] `EngineLoopHost::plan` brackets `compile_turn` with `sink.planning(true/false)` and passes a
      live thinking sink. Failing-first test: `normal_turn_planning_state_reaches_the_sink`
      (a recording sink observes `Planning(true)` … `Planning(false)` around a normal `run_turn`).
- [ ] Thinking deltas emitted by the provider during a normal turn reach the sink
      (`normal_turn_streams_thinking_deltas`, mock provider emitting `ThinkingDelta`).
- [ ] No behavior change for `/plan` (`plan_turn`) — it already streams; test stays green.
- [ ] Gate green: `cargo test --workspace`, clippy `-D warnings`, fmt, `cargo test -p flux-codegate`.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- Pure wiring: `SharedSink::thinking_delta`/`planning` forward (`loop_host.rs:893-897`),
  `SinkEvent::Thinking/Planning` ride the channel (`loop_host.rs:933-934`), `CliSink` renders both
  (`crates/flux-cli/src/main.rs:3541-3556`).
- Ships first and independently — no dependency on any other epic story.

---
id: C-531
title: "Pair tool sink events by dispatch id"
pillar: Core
status: ready
priority: 2
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-lang, flux-flow, flux-tui, flux-cli, flux-sdk, flux-server]
note: "C-528's concurrent batches interleave same-name call/result events; the TUI's name-based LIFO match cross-attaches them"
---

# Pair tool sink events by dispatch id

## Goal

A tool result, timing, or progress line can never attach to the wrong transcript card, regardless of
how many same-name calls run concurrently. The sink stream carries the pairing the durable log
already has.

## Acceptance

- [ ] `run_call` (`crates/flux-lang/src/runtime.rs`) mints a process-unique dispatch id per call and
      emits it on both the call and its result. A failing-first flux-flow test drives two admitted
      same-name `read` calls through `flush_parallel_native_calls` with a blocking tool releasing
      them out of order, and proves via a collecting sink that each result event carries the id of
      its own call.
- [ ] `FlowSink::{tool_call, tool_result}` and `AgentSink::{tool_call, tool_timing, tool_result}`
      carry the id; every overriding implementor is updated (flux-flow SinkBridge/loop_host/whatif,
      flux-cli CliSink/GoalSink/ReviewProgressSink/StreamJsonSink, flux-app, flux-server,
      flux-orchestrate, flux-sdk, flux-tui ChannelSink). Clean cutover — no compat shims.
- [ ] The TUI matches `finish_tool`/`time_tool` (and `progress_tool` where an id is present) on the
      id. A failing-first TestBackend test feeds `call(id1, read a)`, `call(id2, read b)`,
      `result(id1)` and asserts card *a* resolves while card *b* stays `◌ running` — today the LIFO
      name scan resolves *b*.
- [ ] stream-json emits the id as a new field (additive) with a release-note line.
- [ ] The whatif `RerunRecordingSink` FIFO pairing hazard is either fixed by the id or explicitly
      re-scoped in its comments.
- [ ] Breaking signature change on published crates is recorded as a workspace MINOR decision.
- [ ] Full repository gate green.

## Progress

- 2026-08-05 — filed from the tool-output review. At filing, C-528's
  `flush_parallel_native_calls` is canonical on origin and arriving via an in-progress merge; the
  cross-attachment becomes the common case the moment it lands.

## Notes

- Evidence: name-based LIFO matching at `crates/flux-tui/src/lib.rs:1857-1948` ("Ops dispatch
  sequentially" premise); concurrency at `crates/flux-flow/src/staged.rs:2890-2949`
  (`native_call_parallel_safe` admits idempotent read-only ops → same-name read/grep/glob batches);
  resume pairs by step id and is already correct (`lib.rs:2968-2971`).
- No provider `tool_use_id` reaches `run_call` (the staged executor wraps each call in a one-call
  AST and drops `call_id`); an `AtomicU64` minted at dispatch is sufficient — call and result
  bracket one await.
- The C-158 bash progress channel never passes `native_call_parallel_safe` (bash is
  `AccessKind::Process`), so name-matching for progress is sound today; plumb the id there only if
  trivial.
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-1.

---
id: A-79
title: Stream correlated sub-agent activity through the parent sink
pillar: Agent
status: done
priority: 1
epic: live-sub-agent-activity
design: docs/designs/live-sub-agent-activity.md
note: "forward planning, safe status/tool lifecycle and completion live; keep child prose/reasoning private"
---

# Stream correlated sub-agent activity through the parent sink

## Goal
Let a surface report what a delegated role is doing while it works, without duplicating the child reply
or exposing its private reasoning and tool results. Every forwarded event must identify the role and child
run so nested or concurrent same-named operations remain pairable.

## Acceptance
- [x] Failing-first parent-engine regression proves child planning, a display-status call, a read call and
      its outcome reach the parent sink before the child finishes; child thinking, prose and result content
      do not cross.
- [x] The L2 spawner contract carries an optional per-turn activity reporter without depending on the L3
      `AgentSink`; `FlowEngine` derives that reporter from its owned turn channel.
- [x] Every event carries role + child session + child-local call correlation. Nested child events relay
      unchanged, and repeated operation names in different children cannot mis-pair.
- [x] Existing child final-text collection, usage roll-up, audit correlation, block presentation,
      cancellation and wall-clock behavior remain intact; parent cancellation delivers one child
      failure completion before its activity channel closes.
- [x] Public API and architecture comments document the privacy/projection boundary; CHANGELOG and
      customer-facing release notes describe the visible improvement.
- [x] Full workspace build/test/clippy/fmt and `flux-codegate` are green.

## Progress
- 2026-07-14 started from a downstream served-chat regression: Flux 0.21's `LocalSpawner` terminates all
  child `AgentSink` callbacks in its private `TextCollector`.
- 2026-07-14 forwarding is implemented and the failing-first Flux regressions are green. A lexical
  executor bridge also carries the reporter through adapter tools that open a nested one-shot
  `FlowClient`; the downstream manager `message/stream` acceptance passes against this workspace.
- 2026-07-14 full `cargo test --workspace --no-fail-fast` and workspace clippy are green, including
  `flux-codegate`; targeted runtime, flow, and orchestrate regressions are green. The final workspace
  format check is blocked only by concurrent A-77 edits in `flux-cli`, `flux-config`, and `flux-flow`.
- 2026-07-14 adversarial review hardened the contract: keys and values are both secret-scrubbed,
  success/failure is emitted exactly once at the spawner boundary (including timeout/drop paths), child
  planning is asserted explicitly, and streamed nested `FlowClient` execution pins the lexical reporter
  before crossing `tokio::spawn`. Focused tests and warnings-denied clippy are green.
- 2026-07-14 closed for the 0.22.0 release. The previously recorded A-79 test/clippy/codegate pass plus
  A-77's complete post-merge repository gate provide the release evidence; the user explicitly requested
  that the release itself not rerun validation.
- 2026-07-14 post-release review fixed cancellation teardown ordering: the parent now drops an
  in-flight child collector before the final activity drain, with an end-to-end pending-`task`
  regression requiring the correlated failure completion.

## Notes
- The downstream acceptance boundary is ai-agent-platform E-40. Do not flatten child callbacks into the
  parent's unscoped tool callbacks: those lack role/session identity and mis-pair repeated op names.
- Existing sinks receive the reserved `subagent.activity` observation and default-ignore it until they
  deliberately project it. Adding `SpawnRequest::activity` is a public struct-shape change and ships with
  the repository's next breaking release; callers using `SpawnRequest::new` remain source-compatible.
- A-80 owns the adjacent pre-existing lifecycle gap exposed by the same adapter route: nested one-shot
  runtimes do not yet inherit the parent cancellation token or durable session lineage, and shared/cloned
  context slots still need scoped install/restore rather than overwrite-only turn state.

---
id: C-570
title: "Agents report typed progress and cooperatively yield upstream"
pillar: Core
status: ready
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-runtime, flux-flow, flux-orchestrate, flux-cli]
depends_on: [C-569]
note: "SpawnActivity is host telemetry; add durable child-authored reports, acknowledgement and a resumable safe-checkpoint yield"
priority: 10
---

# Report progress without claiming authority

## Goal

Let a long-running agent tell its parent or Fleet main what phase it reached, what needs attention and
where it can safely yield, using one bounded durable protocol rather than stdout or final prose.

## Acceptance

- [x] Failing first, a blocking native `task` child emits observations but cannot produce a durable,
      acknowledged “red test established” progress record or yield a resumable cursor. The fixed
      child reports while its parent continues to await the terminal result.
- [x] `AgentReport` has stable report/sequence, agent/session/parent/assignment/loop identities,
      phase/state, bounded redacted summary, optional completed/total units, evidence references,
      attention request and current budget projection. Persistence returns an acknowledgement/event
      id and is idempotent across retry/restart.
- [x] Reports cannot transition a Board item, mark work done, change Fleet membership, assert
      unverified evidence or widen capability/budget. Host projections derive operational state from
      admitted state plus verified receipts.
- [x] `candidate_ready` means implementation is frozen for C-572/C-587 review and reflection;
      `handoff_ready` is host-derived only after both mandatory receipts settle successfully. A
      worker cannot report itself past that barrier.
- [ ] Native `task` transports reports through the existing correlated child boundary without
      exposing thinking/tool-result content. Fleet and C-552 TaskAgentBackend map the same schema to
      their event streams; `SpawnActivity` remains compatible host telemetry.
- [ ] A loop may yield only at a declared safe checkpoint. Yield settles current usage and returns a
      typed partial terminal plus durable cursor; resume preserves exact assignment, session, loop
      binding, capabilities and budget. It is neither cancellation nor operator pause.
- [x] Oversized, secret-bearing, duplicate, out-of-order and wrong-assignment reports fail closed or
      are safely elided with bounded diagnostics. Progress/report delivery never backpressures an
      agent indefinitely.
- [ ] Hermetic tests cover nested children, five concurrent workers, report-before-review, attention,
      cooperative yield, acknowledged resume and parent cancellation.

## Progress

- 2026-08-08 — landed the report contract and the native `task` transport. `flux-core`'s
  `AgentReport`/`AgentReportLedger` own the record shape, the caps, idempotent acknowledgement and
  the `handoff_ready` barrier; `flux-runtime`'s `AgentReportReporter` stamps identity and sequence
  and redacts every model-authored string before a sink sees it; `flux-orchestrate`'s `TextCollector`
  relays a report across the correlated child boundary only for the session that authored it,
  eliding anything else behind a bounded `agent.report.refused` diagnostic. Cooperative yield exists
  as the pure `yield_at`/`AgentYieldCursor` contract with declared checkpoints and exact resume.
- 2026-08-08 — still open, and the reason the transport, yield and hermetic-coverage boxes stay
  unchecked: no authored loop emits through a typed stage yet (the reporter is the seam, nothing
  calls it from Flux-Lang), the engine has no `yielded` partial terminal that carries the cursor,
  Fleet and the C-552 `TaskAgentBackend` do not yet map the schema onto their event streams, and
  report-before-review plus parent-cancellation coverage is not written.

## Evidence

- Failing first: `cargo test -p codewandler-flux-orchestrate
  a_child_progress_report_crosses_the_correlated_boundary_redacted` panicked with `left: 3 right: 1`
  — the correlated boundary relayed a record naming another agent's session and one claiming the
  host-derived `handoff_ready` state straight through to the parent.
- After: that test plus `a_nested_child_report_reaches_the_root_with_its_own_identity` (nested child
  keeps its own session/depth/spawn id),
  `five_concurrent_workers_report_through_one_sink_without_cross_talk` and the six
  `crates/flux-core/tests/agent_report.rs` cases pass. Targeted runs: `cargo test -p
  codewandler-flux-core`, `-p codewandler-flux-runtime`, `-p codewandler-flux-orchestrate`; clippy
  with `-D warnings` clean on the two touched runtime crates.

## Notes

- A-140/A-141 own operator-initiated pause and in-flight effects. This story owns a worker choosing
  a checkpoint already declared by its loop.
- A-79's `SpawnActivity` is reused for correlation but not widened into an unauthenticated status
  mutation path.

---
id: A-94
title: Mid-turn steering — queue user guidance into a running turn
pillar: Agent
status: done
priority:
epic:
design:
note: "a running turn is take-it-or-Ctrl-C today; let the user type while the agent executes — the message queues and injects at the next planner consultation as a steering block, without cancelling in-flight ops or losing the turn; the multipass loop already re-consults, so the seam exists"
---

# Mid-turn steering — queue user guidance into a running turn

## Goal
Let the user talk to the agent while it runs: input typed during execution queues and injects at
the next planner consultation as a clearly-attributed steering block ("stop touching the tests,
focus on the parser"), without cancelling in-flight ops or losing the turn. Today the only options
are wait or Ctrl-C.

## Acceptance
- [x] A steering message submitted mid-turn is injected at the next planner consultation, visibly
  attributed as mid-turn user guidance, and persists in the session log — failing-first test
  driving a mock-provider multipass turn.
  (`staged::tests::steering_queued_mid_turn_is_injected_at_the_next_consultation_in_order`,
  `engine::tests::steering_reaches_the_planner_and_leaves_shape_and_execution_intact`)
- [x] In-flight ops are never cancelled or re-fired by steering; approvals pending at injection
  time are unaffected — behavior-lock test. (The engine test drives a captured batch through
  approval + execution with steering queued: exactly one write, injection only at the round head.)
- [x] TUI: the composer stays live during execution with a "queued" indicator; queued messages are
  editable/retractable until consumed.
  (`engine_drain_empties_the_strip_and_steered_messages_reach_the_transcript`,
  `an_edit_raced_by_engine_consumption_falls_back_to_a_fresh_submission`)
- [x] Plain-CLI: a documented equivalent (or an explicit statement that steering is TUI-only in v1).
  (Documented: steering is **TUI-only in v1** — the REPL's blocking readline has no live composer;
  stated in CHANGELOG + WHATS-NEW.)
- [x] Multiple queued messages inject in order at one consultation. (Covered by the staged test —
  two messages, one attributed block, submission order asserted.)

## Progress
- 2026-07-28 — implemented. `flux_flow::SteeringQueue` (id-based push/edit/retract/reorder/drain)
  shared surface↔engine via `FlowEngine::set_steering` → `EngineLoopHost` → `StagedContext`;
  `adaptive_explore` drains at every round head and injects one `<user-steering>` block (merged
  into a trailing user `tool_result` message so the conversation never grows a consecutive-user
  pair). Consumed steering is recorded as a durable, redacted `turn.steering` observation —
  deliberately not an `EventKind::Message`, preserving the strict user → assistant alternation.
  TUI: `ChatState.queue` re-backed by the shared `SteeringQueue` (edits/retracts are id-based so a
  concurrent engine drain invalidates instead of retargeting); `turn.steering` observations render
  as `↪ steering delivered:` transcript notices; unconsumed leftovers still start ordinary
  follow-up turns at `Finished`.

## Notes
- Seam: the agent loop's planner consultation point (multipass loop re-consults each pass) +
  `flux-tui` composer state.
- v1 scope: steering is TUI-only. The plain-CLI REPL blocks on readline for the whole turn, so
  there is no live composer to queue from; SDK embedders can attach their own queue via
  `FlowEngine::set_steering`.
- A-93 (typed session log) note: steering deliberately introduces no new `Message` shape — it
  rides the observation channel, so A-93's three illegal shapes remain the complete set.

---
id: D-131
title: Flow-driven session primitive — start an authored flow as the conversation driver
pillar: Agent
status: done
epic:
design: ../designs/flow-driven-session.md
note: "downstream ask (ai-agent-platform R-20, flows arc): start-flow-turn + authored-prompt surfacing on suspension + bounded model-segment delegation; the resume half already ships"
---

# Flow-driven session primitive — start an authored flow as the conversation driver

## Goal
Let a consumer run an **authored flow as the session's conversation driver**: turn 1 executes the flow
to its first top-level `await` and surfaces the **flow's authored prompt** as the assistant turn; each
later turn resumes the suspension deterministically. The resume half already exists —
`FlowEngine::run_turn_cancellable` checks `take_suspension(session_id)` first and bypasses the planner
with full turn/usage/event accounting (`resume_suspended`). What's missing is the **entry point** and
the prompt surfacing: nothing starts an authored flow as the driver (the one-shot SDK path errors on
suspension — "drive await flows through the engine instead"), and the suspension surface is a hardcoded
hint (`"(awaiting your input — reply to continue the flow)"`, engine.rs) rather than the flow's own
last-emitted text.

## Acceptance
- [x] A public engine entry point (`FlowEngine::start_flow_turn(session_id, flow, sink)`) executes an
      authored flow fresh, persists the suspension at the first top-level `await`, and returns the
      flow's authored prompt as the assistant turn. Failing-first: a two-`await` flow driven
      turn-by-turn produces the two authored prompts with **zero planner invocations**
      (`start_flow_turn_drives_a_two_await_flow_with_zero_planner_calls`).
- [x] Resume keeps the existing accounting but surfaces the **authored** prompt on re-suspension
      instead of the fixed hint (`suspension_prompt`; the hint remains only as the empty-emit
      fallback); completion returns the flow's result as the final turn.
- [x] **Bounded model-segment delegation:** a flow can hand a run of turns to the model loop under a
      capability scope + an explicit exit condition, then return control deterministically — the
      reflexive `ai_segment(goal, tools, max_rounds, until?)` op (NOT a new AST node, which would
      cascade into roundtrip/native-syntax/docs drift). Failing-first: the segment cannot call an op
      outside its scope (`ai_segment_cannot_call_an_op_outside_its_scope`); the `max_rounds` cap and
      the `until` predicate both return control to the flow
      (`ai_segment_is_bounded_by_max_rounds_and_returns_control`,
      `ai_segment_exits_early_when_until_symbol_is_bound`).
- [x] The approver/risk envelope (`RiskApprover`) applies inside flow-driven sessions exactly as in
      planner-driven ones (`flow_driven_session_applies_the_risk_approver`). Gate green.

## Progress
- 2026-07-10 — filed from the ai-agent-platform flows-arc design (their R-20 "deterministic
  conversation mode", design doc `docs/designs/flows.md` downstream). Upstream-first per the own-flux
  rule.
- 2026-07-11 — design [flow-driven-session.md](../designs/flow-driven-session.md) written after a full
  seam audit. Decomposed into **Phase A** (`start_flow_turn` entry point + authored-prompt surfacing;
  acceptance 1/2/4 — a mirror of `resume_suspended` + a hint→`outcome.result` swap) and **Phase B**
  (`ai_segment` bounded model-delegation as an engine-resolved suspension; acceptance 3). Six
  invariants pinned. Implementing Phase A first.
- 2026-07-11 — **Phase A DONE** (`crates/flux-flow/src/engine.rs`): `FlowEngine::start_flow_turn`
  fresh-drives an authored flow to its first top-level `await`, persists the suspension, and surfaces
  the flow's authored prompt; `suspension_prompt` helper + resume-path swap replace the hardcoded
  hint with `outcome.result` (hint kept as the empty-emit fallback). Acceptance **1, 2, 4** met.
  Three failing-first tests pass (`start_flow_turn_drives_a_two_await_flow_with_zero_planner_calls`,
  `flow_driven_suspension_falls_back_to_hint_on_empty_authored_prompt`,
  `flow_driven_session_applies_the_risk_approver`); crate 297/297, clippy + fmt clean. **Phase B**
  (`ai_segment`, acceptance 3) pending a design green-light on the exit-condition shape.
- 2026-07-11 — **Phase B DONE + story CLOSED.** `ai_segment` shipped as a **reflexive op** (third
  `LoopHost` method + `flux-tools/reflect.rs` tool), NOT a new AST node — a new `Node` kind would
  cascade into roundtrip totality, the native lexer/parser/printer, the emit_plan schema, and the
  node-reference/`website_in_sync` drift guard (an epic-sized surface). `EngineLoopHost::ai_segment`
  runs the bounded, cap-scoped loop (goal via the `feedback` channel; `advertised = tools` +
  `push_cap_scope(tools)`; exits on natural completion / `max_rounds` / the `until` symbol becoming
  bound). Both flow-driven paths now arm the loop host + drain plumbing so a segment's inner
  plan/run_plan stream and its planner spend folds into turn usage. All 4 acceptance items met; 6
  failing-first tests (3 Phase A + 3 Phase B); flux-flow 300/300, flux-runtime 67, flux-tools 130,
  codegate layering, clippy + fmt, full workspace build — all green. (Pre-existing + unrelated:
  `website_customer_changelog_is_in_sync` fails on repo-state whats-new drift; reproduced with my
  changes stashed.)

## Notes
- ~90% exists: suspension persistence (`run_top_level` → `Suspension`, `resume_flow`), the engine's
  suspension-first turn path, and the planner-path reification
  (`await_inside_a_plan_is_reified_not_a_turn_suspension`) all ship today. This is a promotion of the
  resume machinery into a first-class session mode, not new machinery.
- Sibling ask: D-132 (voice provider defers to flow suspensions). Nice-to-have from the same review:
  D-133 (`annotate_effects` helper).

---
id: D-131
title: Flow-driven session primitive — start an authored flow as the conversation driver
pillar: Agent
status: backlog
epic:
design:
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
- [ ] A public engine entry point (shape TBD — e.g. `start_flow_turn(session_id, flow, sink)`)
      executes an authored flow fresh, persists the suspension at the first top-level `await`, and
      returns the flow's authored prompt as the assistant turn. Failing-first: a two-`await` flow
      driven turn-by-turn produces the two authored prompts with **zero planner invocations**.
- [ ] Resume keeps the existing accounting but surfaces the **authored** prompt on re-suspension
      instead of the fixed hint; completion returns the flow's result as the final turn.
- [ ] **Bounded model-segment delegation:** a flow can hand a run of turns to the model loop under a
      capability scope + an explicit exit condition, then resume deterministically (the downstream
      "ai_segment" node — a deterministic skeleton with visibly-bounded non-deterministic segments).
      Failing-first: the segment cannot call an op outside its scope; the exit condition returns
      control to the flow.
- [ ] The approver/risk envelope (`RiskApprover`) applies inside flow-driven sessions exactly as in
      planner-driven ones. Gate green.

## Progress
- 2026-07-10 — filed from the ai-agent-platform flows-arc design (their R-20 "deterministic
  conversation mode", design doc `docs/designs/flows.md` downstream). Upstream-first per the own-flux
  rule.

## Notes
- ~90% exists: suspension persistence (`run_top_level` → `Suspension`, `resume_flow`), the engine's
  suspension-first turn path, and the planner-path reification
  (`await_inside_a_plan_is_reified_not_a_turn_suspension`) all ship today. This is a promotion of the
  resume machinery into a first-class session mode, not new machinery.
- Sibling ask: D-132 (voice provider defers to flow suspensions). Nice-to-have from the same review:
  D-133 (`annotate_effects` helper).

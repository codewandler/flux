---
id: A-31
title: "Exhausted planner budget must report the last rejection — decode-Err and not-callable branches skip last_reject"
pillar: Agent
status: done
epic: parse-resilience
design: docs/designs/parse-resilience.md
note: "s_360 showed the bare 'planner did not produce a plan within 8 steps' because the decode-failure branch is the ONE rejection path that never records last_reject — the informative variant already exists and was unreachable for this class"
---

# Planner rejection surfacing: every repair feedback is a candidate last_reject

## Goal
`compile_turn` keeps `last_reject` so that when the step budget runs out the user sees *"planner
did not produce a valid plan … the last plan was rejected: <cause>"*. But two repair paths send
feedback to the model without recording it: the `emit_plan` decode-`Err` branch (invalid AST JSON /
invalid Flux-Lang source / missing `source`) and the not-callable branch (hallucinated tool names).
A turn that fails exclusively through those paths — exactly what s_360 did — reports the bare
"planner did not produce a plan within 8 steps" with zero diagnostic value. Record `last_reject` in
both branches so the exhausted-budget error always carries the actual last rejection the model saw.

## Acceptance
- [x] Failing-first test: a mock provider that calls `emit_plan` with an undecodable `ast` (e.g. a
      number) on every step → the turn error contains the "invalid AST JSON" cause. Today it is the
      bare within-N-steps message.
      → `json_arm_garbage_string_ast_surfaces_the_decode_error` (string case) +
      `failed_consultation_still_returns_accumulated_usage` (number case).
- [x] Same for a provider that only calls a nonexistent tool every step → the error carries the
      "`<name>` is not callable" feedback.
      → `exhausted_budget_reports_the_not_callable_feedback`.
- [x] `ask_user` handling is unchanged (a user answer is not a rejection and must never appear as
      one) — the branch was not touched.
- [x] Existing rejection paths (hidden ops, gather violations, analyzer diagnostics, duplicate
      emit_plan) still surface exactly as before (all existing tests unchanged+green; the combined
      error now reads "the last **attempt** was rejected" since the cause may not be a plan).

## Progress
- 2026-07-03 filed from the s_360 diagnosis — the opaque error cost a full forensic session
  (temp instrumentation + paid repro) that an accurate message would have made unnecessary.
- 2026-07-03 **done**: `last_reject` recorded in the decode-`Err` and not-callable branches
  (`compile.rs`), failing-first tests, full gate green.

## Notes
- Site: `crates/flux-flow/src/compile.rs` — decode-`Err` arm (~line 710) and the `other =>`
  not-callable arm (~line 725) of the tool_use loop.
- Epic: [parse-resilience](../designs/parse-resilience.md).

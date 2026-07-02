---
id: C-17
title: Compile-path plan gates — close the text-fallback hidden-op bypass
pillar: Core
status: done
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: a plan emitted as prose JSON (not emit_plan) skips hidden_ops_in entirely — a registered-but-unsurfaced op (e.g. bash with the shell group off) executes; verified on the current tree incl. uncommitted WIP
---

# Compile-path plan gates — close the text-fallback hidden-op bypass

## Goal
Every path that turns model output into an executable plan applies the same gates. Today only the
`emit_plan` tool-call branch runs `hidden_ops_in` (compile.rs:366); the plain-text fallback
(compile.rs:311-321) does not (F1), the engine executes accepted-with-diagnostics plans blind
(engine.rs:380-390, F2), and duplicate `emit_plan` calls silently last-win (F3).

## Acceptance
- [x] **Failing-first:** a text-fallback plan calling a registered-but-hidden op is NOT returned as
      `TurnOutput::Plan` (mirrors the existing tool-call-path test at compile.rs:1284-1302).
- [x] Engine turn loop surfaces `Compiled.diagnostics` and repairs instead of executing a plan
      accepted with unknown-op diagnostics (test).
- [x] Two `emit_plan` calls in one assistant message → clean rejection, not last-wins (test).
- [x] The `unsafe` sink reborrow (compile.rs:284-291) replaced with a safe per-iteration reborrow.
- [x] Uncommitted compile.rs WIP preserved; full gate green.

## Progress
- [x] **F1** — the plain-text plan fallback in `compile_turn` now runs the same `hidden_ops_in`
      gate as the `emit_plan` branch (shared `hidden_ops_rejection` feedback, riding as a user
      message since there is no tool_use id); a hidden-op text plan is repair feedback, never a
      `TurnOutput::Plan`, and never accepted even on the final step.
      Tests: `hidden_op_text_fallback_plan_is_rejected_and_repaired`,
      `hidden_op_text_fallback_is_rejected_even_on_the_final_step` (compile.rs).
- [x] **F2** — `compile_turn` no longer "accepts with diagnostics" on the last step: an analyzer
      failure is always repair feedback, and budget exhaustion rejects the turn with the last
      rejection's text — so no executing caller (loop host, `/plan`+`/run`, `--plan`) can receive a
      diagnostics-carrying plan; the `Compiled` doc promise ("surfaced rather than executed") is now
      true, with only the one-shot `compile` (compile-only surface) still returning diagnostics.
      `plan_turn` additionally enforces the gate itself as a backstop (surfaces the diagnostic text,
      records the assistant message, hands back nothing for `/run`).
      Tests: `unknown_op_plan_is_never_accepted_with_diagnostics` (compile.rs),
      `plan_turn_rejects_a_diagnostics_plan_instead_of_handing_it_to_run` (engine.rs).
- [x] **F3** — multiple `emit_plan` calls in one assistant message are rejected with clean repair
      feedback ("a turn takes exactly ONE plan"), every tool_use answered, none accepted — no more
      silent last-wins. Test: `duplicate_emit_plan_calls_are_rejected_not_last_wins` (compile.rs).
- [x] **F4** — the `unsafe` raw-pointer sink reborrow replaced with a safe `as_deref_mut()`
      per-iteration reborrow; the real lifetime cycle was `stream_blocks`'s signature unifying the
      reference and trait-object lifetimes (`&'a mut (dyn AgentSink + 'a)` + `&mut` invariance) —
      now decoupled (`&'a mut (dyn AgentSink + 'b)`). `compile.rs` carries `#![deny(unsafe_code)]`
      so the gate module stays unsafe-free. Guarded by compilation.
- [x] All five tests written failing-first (verified red on the pre-fix tree), then green.
- [x] Uncommitted engine.rs WIP (C-14 evidence flush, staged) preserved — built on top.
- Note: `loop_host.rs` (out of this story's file scope) drops `Compiled.diagnostics` on the floor;
  it is safe because F2 guarantees `compile_turn` never emits a diagnostics-carrying plan.

## Notes
- Review finding F1–F4 in docs/designs/flux-lang-v1-hardening.md.

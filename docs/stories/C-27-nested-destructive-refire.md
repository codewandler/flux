---
id: C-27
title: "Re-fire the undisclosed-destructive gate on the current plan's disclosure, not any ancestor scope"
pillar: Core
status: done
priority: 1
epic: review-hardening
design: docs/designs/review-hardening.md
note: "destructive_scope is a bare AtomicU32 depth counter, so a nested run_plan approved destructive:false inherits the outer plan's destructive disclosure (counter stays >0) — a runtime-assembled `rm -rf` invisible to the nested plan's static risk preview then dispatches with no re-fire and no prompt: a silent approval-gate bypass"
---

# Re-fire the undisclosed-destructive gate on the current plan's disclosure, not any ancestor scope

## Goal
Close a silent approval-gate bypass in the C-12 undisclosed-destructive re-fire gate — flux's core
security invariant is that an undisclosed destructive op must re-fire per-op approval. `destructive_scope`
is a single `AtomicU32` depth counter on the `Executor` (`crates/flux-runtime/src/lib.rs:806`);
`enter_approved_scope(destructive_disclosed)` bumps it **only** when the flag is true (`:888-893`), and
dispatch gates on `intents.is_destructive() && destructive_scope.load(SeqCst) == 0` (`:1151-1152`). A
nested `run_plan` approved with `destructive:false` does not increment the counter, so it stays `>0`
inherited from the outer scope. Because static plan risk keys on literal args only while dispatch computes
destructiveness from runtime params (`crates/flux-flow/src/runtime.rs:349,386`), a `$symbol`-assembled
`rm -rf` shows no destructive badge in the nested plan yet dispatches with **no re-fire and no prompt**.
The gate must key on the *current* plan's own disclosure, not on whether any ancestor scope disclosed.

## Acceptance
- [x] Failing-first test (extends `undisclosed_destructive_op_refires_approval_inside_approved_scope`,
      `crates/flux-runtime/src/lib.rs:1637`): open an outer `enter_approved_scope(true)`, then a nested
      `enter_approved_scope(false)`, then `dispatch("rm", <runtime destructive intent>)`; assert the
      approver **is** asked and a denying approver blocks the op. Today the outer counter (=1) suppresses
      the re-fire and the op runs silently.
- [x] Fix: track approved-scope disclosure per plan (a stack of scope flags/identities), or evaluate
      `undisclosed_destructive` against the innermost scope's flag — not a bare shared depth counter.
- [x] No regression: depth-1 disclosed/undisclosed cases and the sequential (non-nested) `run_plan` loop
      behave exactly as before.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded **SECURITY-CRITICAL** (Opus). Mechanism confirmed
  exactly. Reachable via reflexive `run_plan` under a destructive-disclosed outer scope (the scope guard
  is held across the inner `execute_flow`, `crates/flux-flow/src/loop_host.rs:1224-1259`). Does **not**
  cross sub-agent boundaries — those get a fresh `Executor`, so the counter is per-executor, not process-wide.
- 2026-07-03 fixed: `destructive_scope` on `Executor` (`crates/flux-runtime/src/lib.rs`) is now a
  `Mutex<Vec<bool>>` — one disclosure-flag frame per open approved-plan scope, pushed by
  `enter_approved_scope` (always, even `false`) and popped by `PlanScopeGuard::drop`. The
  `undisclosed_destructive` gate in `dispatch` now reads only the top-of-stack (innermost) frame
  instead of a bare `AtomicU32` depth counter, so a nested scope's own disclosure can never be
  masked by an ancestor's. New failing-first test
  `undisclosed_destructive_op_refires_approval_inside_nested_disclosed_scope` (written first, confirmed
  failing against the old counter, now passing) proves an outer `enter_approved_scope(true)` +
  inner `enter_approved_scope(false)` still re-fires the per-op gate and a denying approver blocks it.
  All pre-existing depth-1 (`undisclosed_destructive_op_refires_approval_inside_approved_scope`,
  `disclosed_destructive_plan_runs_without_per_op_reprompt`) and sequential/non-nested tests pass
  unchanged. Gate: `cargo test -p flux-runtime -p flux-flow` (235 passed across both crates' unit +
  integration suites), `cargo clippy -p flux-runtime
  -p flux-flow --all-targets -- -D warnings` (clean), `cargo fmt -p flux-runtime -p flux-flow` (clean).

## Notes
- Evidence: `crates/flux-runtime/src/lib.rs:806,888-898,906-909,1151-1153`;
  `crates/flux-flow/src/loop_host.rs:1224-1259` (scope held across reflexive `run_plan`);
  `crates/flux-flow/src/runtime.rs:339-458` (static-vs-runtime destructive asymmetry, "invisible here,
  which is why dispatch re-fires the gate").
- Residual of [C-12](C-12-plan-approval-intents.md). Design: [review-hardening](../designs/review-hardening.md).

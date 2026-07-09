---
id: C-12
title: Thread plan intents into plan approval — close the sub-agent destructive bypass
pillar: Core
status: done
priority:
note: plan approval now sees the plan's real aggregate IntentSet — SubAgentApprover denies destructive plans on the emit_plan path, and an undisclosed destructive op re-fires the approval gate even inside an approved scope
---

# Thread plan intents into plan approval — close the sub-agent destructive bypass

## Goal
Make the plan-approval gate see what the plan actually does, so the sub-agent destructive backstop
fires on the path sub-agents actually use. Verified 2026-07-02 (harness claims review):
`Approver::request_plan`'s default impl forwards an **empty** `IntentSet::default()`
(`crates/flux-runtime/src/lib.rs:612-616`); once a plan is approved, the approved scope suppresses
**all** per-op gates (`in_approved_scope()`, `lib.rs:1060`) — including the destructive force. A
sub-agent's only tool is `emit_plan`, so its whole life runs through `run_plan` → `approve_plan`
(`crates/flux-flow/src/loop_host.rs:530`), and `SubAgentApprover`'s destructive-deny
(`crates/flux-orchestrate/src/lib.rs:43-59`) approves a destructive plan **blind**. The existing
test (`sub_agent_refuses_destructive_command`, orchestrate `:1329`) covers only a direct `ToolUse`
fixture — a path the pure-DAG compiler steers away from. This breaks README:170: "Sub-agents …
cannot approve destructive operations themselves."

## Acceptance
- [x] **Failing-first:** `sub_agent_denies_destructive_plan_from_emit_plan` (flux-orchestrate) — a
      mock provider emits `emit_plan` whose AST calls the existing `FakeDestructive` tool; the
      destructive op must NOT execute (fails today: it runs).
- [x] `request_plan` takes a `PlanApprovalRequest { summary, ops: Vec<String>, destructive,
      mutating, intents: IntentSet }` (breaking trait change, clean cutover — all in-tree
      implementors updated); the default impl forwards the REAL aggregate intents.
- [x] `PlanRisk` (flux-flow `runtime.rs:281`) accumulates per-call-node `tool.intents(...)` into an
      aggregate `intents: IntentSet`; `PlanRisk::approval_request()` builds the request.
- [x] `SubAgentApprover` explicitly denies when `plan.destructive || plan.intents.is_destructive()`.
- [x] **Dynamic-arg hole closed:** a `destructive_scope` disclosure bit rides beside `plan_scope`;
      a destructive op that was NOT disclosed at plan-approval time re-fires the approval gate even
      inside an approved scope (interactive prompts; `--yes` allows; sub-agents deny). Failing-first:
      `undisclosed_destructive_op_refires_approval_inside_approved_scope` (flux-runtime). REPL `/run`
      computes `plan_risk_with_composites` on the reviewed AST and passes disclosure.
- [x] Regression: `disclosed_destructive_plan_runs_without_per_op_reprompt` — no interactive
      double-prompt when the destructive op WAS visible in the approved plan.
- [x] `AllowApprover` (`--yes`, server) keeps allowing destructive plans at top level (human
      opt-in); doc-comment states it must never be installed for sub-agents.
- [x] Full gate green; CHANGELOG entry (note the trust_all/undisclosed-destructive behavior change).

## Progress
- Filed 2026-07-02 from the harness claims review (P1 of the round).
- Done 2026-07-02. `PlanApprovalRequest` carries summary/ops/destructive/mutating + the aggregate
  `IntentSet` folded by `plan_risk`/`accumulate_risk`; `PlanRisk::approval_request()` builds it.
  `SubAgentApprover::request_plan` denies `destructive || intents.is_destructive()` — proven on the
  real emit_plan path by the new orchestrate test (marker file never written). `destructive_scope`
  disclosure bit added beside `plan_scope` (`PlanScopeGuard` pairs the decrement): an undisclosed
  destructive op re-fires the approval gate inside an approved scope; disclosed ones don't re-prompt.
  This deliberately also holds under trust_all ("always"). REPL `/run` computes
  `plan_risk_with_composites` on the reviewed AST and enters the scope with disclosure. 3 new tests;
  full gate green.

## Notes
- Design decisions in `~/.claude/plans/wiggly-tumbling-salamander.md` §C-12.
- Files: crates/flux-runtime/src/lib.rs (trait, request type, scopes, dispatch gate),
  crates/flux-flow/src/runtime.rs + loop_host.rs, crates/flux-orchestrate/src/lib.rs,
  crates/flux-cli/src/main.rs (StdinApprover, `/run`).
- `plan_risk` sees only literal args (`literal_input`, flux-flow runtime.rs:561) — that blindness is
  exactly why the undisclosed-destructive re-fire is needed.

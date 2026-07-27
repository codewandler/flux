---
id: D-177
title: Tune policy mode — authorize-only split
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 3b — deferred; the hardest surgery; depends on D-176"
---

# Tune policy mode — authorize-only split

## Goal
Let `Session::what_if().policy(perms)` re-authorize a recorded run under a different permission
policy against the frozen world, so a stricter policy's DENY surfaces as a real divergence — the
"would the tightened policy have blocked the destructive action?" gate.

## Acceptance
- [x] `WhatIf::policy(perms)` rebuilds the executor from the new `Permissions` and re-runs the target
      turn with the `Frozen` scope (via `VariantOverrides::permissions`, which replaces the spec's
      `allow`/`deny` **wholesale** — merging would make a stricter policy untestable — and flips the
      `FrozenTape`'s `with_reauthorize` flag D-175 landed inert).
- [x] Failing-first: under a stricter policy, an op the original ran is DENIED — the denial surfaces
      as the op's real recorded outcome (`is_error`, the envelope's own refusal wording) and halts
      the plan as `FailureKind::Denied`, not masked by the taped output; under an equal policy the
      run stays hermetic, serves from tape, re-runs nothing live, and diffs identical.
      (`a_stricter_policy_denies_an_op_the_recording_ran`,
      `an_equal_policy_stays_hermetic_and_serves_from_tape`.)
- [x] The authorize decision is a pure function of `(op, subjects, Permissions)` with **no** execution
      side effect, and does **not** open a bypass in the one non-bypassable envelope (adversarial
      tests in `flux-runtime`: `authorize_decides_without_any_execution_side_effect`,
      `an_allow_verdict_is_not_a_bypass_of_the_real_envelope`,
      `authorize_and_dispatch_report_the_same_refusal`, `authorize_denies_an_unknown_op`).

## Progress
- **Done** (2026-07-28).
  - `flux-runtime`: `Executor::authorize(name, &params) -> AuthorizeVerdict`
    (`Allow`/`ApprovalRequired`/`Deny(reason)`, `#[non_exhaustive]`). Implemented by **extracting**
    the deterministic gates out of `dispatch_outcome` into a private `gate(...)` — capability-scope
    floor, filesystem subject normalization, authority contract, mandatory policy floor, permission
    rules — which BOTH surfaces now call. One implementation, so they cannot drift; drift here would
    be a safety bug, and a test pins that the two report the same refusal *message*, not just the
    same verdict.
  - **`authorize` is deliberately synchronous**, which makes "no execution side effect" structural
    rather than a promise: `Tool::execute` and `Approver::request` are both `async` and therefore
    unreachable from it. It also records no evidence (a `GateAudit::DecisionOnly` mode suppresses the
    live path's `cap_scope_denied` observation), adds no permission rule, and bumps no cache
    generation.
  - **Excluded from `authorize`, on purpose**: the pre-tool hooks (a hook may rewrite `params`, and
    running hooks for a hypothetical call would itself be a side effect) and the approval gate. Both
    still run on every real dispatch. The capability-scope floor is factored into its own
    `cap_scope_gate` because the live path must check it BEFORE the hooks; checking it twice there is
    a pure read and a denial returns before the second check.
  - `flux-flow`: the `Frozen` arm of `ExecutorHost::dispatch` consults `frozen.reauthorize()` before
    serving — on a refusal it records the denial via `note_policy_denial` and returns the real
    refusal as the op outcome, so the taped answer can never mask it. `FrozenTape::is_hermetic`
    already counted `policy_denials`, so `Counterfactual::hermetic()` goes `false` for free.
  - Known limitation (pre-existing, not introduced here): `RerunRecordingSink` reconstructs a cell
    from `AgentSink::tool_result`, which carries `is_error` but not the envelope's structural
    `denied` flag — so a policy-denied cell records `denied: false` (with the refusal as its content
    and `is_error: true`). Widening `AgentSink` for it wasn't worth the churn; the denial is
    unambiguous in the trace via `PlanHalted{kind: Denied}`, which is what the test asserts on.
  - Gate green in both workspaces (build/test/clippy `-D warnings`/fmt) plus `flux-codegate` and
    `codewandler-flux-sdk` on the default and `test-kit` feature configurations.

## Notes
- The hard part: `Executor::dispatch_outcome` currently bundles authorize + execute. This story adds
  an authorize-*only* entry so policy mode can decide DENY/ALLOW without executing, then serve the
  frozen tape on ALLOW. Isolate and test heavily — this touches the safety-critical envelope.
- Split out of D-176 deliberately so the rest of Tune ships without blocking on it.

---
id: C-182
title: Plan-approval sheet must list the ops it is approving
pillar: Core
status: done
epic: turn-latency-visibility
design: docs/designs/turn-latency-visibility.md
note: "TUI ChannelApprover implements only Approver::request, so whole-plan approval falls through to the default request_plan and shows `3 op(s) · low` with no op names"
---

# Plan-approval sheet must list the ops it is approving

## Goal
Approving a whole plan in the TUI shows `approve run plan?` over a single line reading
`3 op(s) · low · mutating`. The user is asked to authorize three operations without being told which
three, or against what. `PlanApprovalRequest` already carries `ops: Vec<String>` and typed
`requirements`; the plain CLI renders both (`plan_prompt`, `flux-cli/src/session.rs:1184`). The TUI
discards them because `ChannelApprover` never implements `request_plan`, so the default
(`flux-runtime/src/lib.rs:1736`) collapses the batch to one string.

## Acceptance
- [x] `ChannelApprover` implements `Approver::request_plan`; the sheet lists the op names and the
      concrete statically-visible targets (typed `requirements`, `Operation`-kind entries skipped as
      duplicates of the ops line, plus `IntentTarget::Process` commands) — failing-first TestBackend
      test asserting the op names appear.
- [x] A destructive plan renders a warn-styled `⚠ contains a destructive operation` row, and the risk
      summary is shown in risk color on the header.
- [x] Long target lists reuse the existing subject windowing + `↑/↓ +N more` marker rather than
      growing the sheet past the half-screen cap.
- [x] Per-tool (non-plan) approvals render exactly as today.
- [x] Display-only: no change to approval semantics, receipt binding, or dispatch's per-op re-check —
      an undisclosed destructive op still re-fires the per-op gate inside an approved scope.

## Progress
- Implemented 2026-07-28. `ChannelApprover::request_plan` + `controller::plan_detail_lines`;
  `ApprovalView` now nests an `ApprovalRequest { tool, subjects, summary, destructive }` (a
  breaking change for embedders, rides the next MINOR). The destructive row renders above the
  scrollable list; the risk summary rides the header via the shared `plan::risk_style`.
- Tests: `the_approver_raises_a_plan_request_with_its_ops_not_a_bare_count`,
  `the_plan_approval_sheet_lists_its_ops_and_targets`,
  `plan_detail_lines_skip_operation_and_wildcard_requirements`,
  `a_destructive_plan_warns_on_its_own_row` (flux-tui).
- Verified failing-first: with the `request_plan` override removed the approver test fails on the
  default's bare `N op(s)` collapse.

## Notes
- The batch content is also on the `action_batch.proposed` observation, deliberately unrendered by
  both surfaces (it would garble the CLI prompt line); the approval request is the right source.
- Shares the sheet with C-113 (deny-with-reason) and C-154 (risk color) — check those before
  restyling the header.

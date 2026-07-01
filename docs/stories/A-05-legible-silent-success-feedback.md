---
id: A-05
title: Legible silent-success feedback — stop the loop re-running ops that already succeeded
pillar: Agent
status: done
note: FIXED — transcript renders `✓ ok (no output)` for empty views (all ops), bash emits `[exit 0] (no output)`, and run_plan skips a byte-identical plan re-emitted after a successful run; live rm task: 1 execution, 2.1s, $0.0137 (was 2 executions, 13.5s, $0.14)
---

# Legible silent-success feedback — stop the loop re-running ops that already succeeded

## Goal
Make a successful zero-output op legible in the loop feedback so the model doesn't re-run it.
Observed live (2026-07-01, session s_311): `flux run --yes "delete the file victim2.txt"` →
plan 1 `bash("rm victim2.txt")` succeeds (✓, empty stdout) → plan 2 emits the **byte-identical**
command (fails: file already gone) → plan 3 checks `path_exists` and finally answers. Three planner
calls, 13.5s and $0.14 for a one-op task; the same first plan also ran `rm` **twice** in the
uninstrumented variant.

Root cause: `BashTool::execute` returns `content: ""` for a silent success (stdout/stderr empty,
`[exit N]` appended only when `N != 0` — `crates/flux-tools/src/lib.rs`), and the flow transcript
renders a bare call as `[{op}]\n{view}` (`crates/flux-lang/src/runtime.rs:1142,1160`) — so the
loop-carried `$feedback` for "rm succeeded" is `[bash]` followed by a blank line. The model cannot
distinguish that from "nothing happened" and retries. (Same failure family as the fixed
empty-parallel-branch bug: empty evidence spins the loop.)

## Acceptance
- [x] Failing-first: the transcript renderer marks every empty view `✓ ok (no output)` — asserted
      at the renderer level (`flux-flow::runtime::execute_flow_transcript_marks_silent_successes`,
      via `flux-lang::runtime::legible_view`), so it covers every op, not just `bash`.
- [x] `BashTool` appends `[exit 0] (no output)` when a successful command produced no output.
- [x] Loop-guard extension: `run_plan` fingerprints the plan AST; a byte-identical plan re-emitted
      right after a SUCCESSFUL run is **not re-executed** (stronger than the planned nudge — a
      non-idempotent op must not run twice) and the model is told the previous run succeeded
      (`loop_host::tests::run_plan_skips_an_identical_plan_after_success`). A failed or suspended
      plan stays retryable; if the model insists, the informational transcript repeats and the
      existing stall guard escalates/stops.
- [x] Live re-check: `rm` scenario → one plan, command executed once (`✓ exit 0` in the step
      display), grounded summary; 2.1s / $0.0137 (was 3 calls, double execution, 13.5s / $0.14).

## Progress
- **DONE (2026-07-02).** `flux-lang::runtime::legible_view` (empty view → `✓ ok (no output)`) on
  both transcript entry shapes; `BashTool` `[exit 0] (no output)`; `LoopGuard.last_plan_hash/ok` +
  the skip in `run_plan` (recorded on success/failure, suspension counts as not-ok). 3 new tests;
  full gate green; live-verified.

## Notes
- Found during the 2026-07-01 harness e2e review.
- Interacts with A-04: with shell gated off the exact repro changes, but the class (any silent
  success — `write` to an existing path, an op returning empty list, …) stays.
- Cheapest correct fix is in the transcript renderer (one place, all ops); the BashTool marker is
  belt-and-braces.

---
id: A-62
title: Accurate validation diagnostic headers (stop mislabeling failures as "unknown operations")
pillar: Agent
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-010: unrelated validation failures are printed under the header `diagnostics - the plan references unknown operations` even when the failing bullet is correct and about something else — the header misleads the reader (and the planner reading it back)"
---

# Accurate validation diagnostic headers

## Goal
Several unrelated validation failures are grouped under a single misleading header —
`diagnostics - the plan references unknown operations` — even when the individual bullet is correct
and has nothing to do with unknown operations. The header should describe the actual failure class,
because both the human reader *and* the planner (which reads diagnostics back to repair) are misled
by it.

## Why (evidence)
- Beta F-010: "Several unrelated validation failures appeared under `diagnostics - the plan
  references unknown operations`, even when the bullet itself was correct."

## Acceptance
- [ ] Validation diagnostics are grouped/headed by their real failure class (unknown op vs. bad
      arg vs. type/shape vs. composability, etc.), so the header matches the bullets under it.
- [ ] The "unknown operations" header appears only when an operation is genuinely unknown.
- [ ] Failing-first test: a plan that fails validation for a non-unknown-op reason produces a
      diagnostic whose header is not "references unknown operations".
- [ ] Planner-facing feedback (the repaired-plan loop) carries the corrected, specific header.

## Progress
- 2026-07-08 **DONE.** The misleading header was hard-coded in the CLI's `print_diagnostics` (and the
  matching "not running" refusal). Added `diagnostics_all_unknown_op(diags)` and gated both strings on
  it: "the plan references unknown operations" only when *every* diagnostic is an unknown-op error
  (message shape `unknown operation: …`), else "the plan failed validation". The planner-facing repair
  path (`join_diags`) already used the specific per-message text — no header there — so the fix is
  scoped to the CLI. Test: `diagnostics_header_matches_the_failure_class`.

## Notes
- Ground against the validation/diagnostics assembly in the compile/validate path (where the header
  string is chosen for the diagnostics block).
- Epic: [beta-hardening](../designs/beta-hardening.md).

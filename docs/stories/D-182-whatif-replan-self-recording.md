---
id: D-182
title: Record served cells on the what-if re-plan path so its diffs are never vacuous
pillar: Agent
status: ready
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 2
note: "review finding (2026-07-28): re-plan path drives run_turn_pinned with a bare NullSink — a fully tape-served re-plan diffs as total divergence"
---

# Record served cells on the what-if re-plan path so its diffs are never vacuous

## Goal
`WhatIf::run`'s **re-plan** path (`.model()`/`.system_prompt()` variants) drives
`run_turn_pinned(&dst, …, &mut sink)` with a plain `NullSink` (`crates/flux-sdk/src/whatif.rs`).
Under a `Frozen` scope a served dispatch returns from `ExecutorHost::dispatch` before the tail
record, so no `OpRecorded` cell is written to the destination session. `Scenario::check` faces the
identical gap and fixes it explicitly with `flux_flow::whatif::RerunRecordingSink`; the re-plan path
must do the same.

Confirmed failure: a re-plan whose new plan is identical and fully tape-served yields
`hermetic() == true` while `diff()` (via the `cell_rows` fallback) sees zero cells on the dst side
and reports every statement as `Plan { b_stmt: None }` — a fake total divergence,
`first_divergence()` at node 0, and `what_if_over` over-counting `changed`. Both existing re-plan
tests only exercise genuinely-diverging plans, so the vacuous case is unpinned. The same shape
affects `.off_tape(Live)` substitution runs (`FrozenTape::record_tail` records misses, never hits).

## Acceptance
- [ ] The re-plan path wraps its sink in `RerunRecordingSink` (or equivalent) so served dispatches
      are recorded into the destination session, exactly as `Scenario::check` does.
- [ ] Failing-first test: a `.system_prompt()` variant engineered to produce the identical plan,
      fully tape-served → `diff().identical == true` (not total divergence), `hermetic() == true`.
- [ ] `.off_tape(Live)` runs record served hits too, not only live-miss tails; test pins a mixed
      served+live run's diff completeness.
- [ ] While in the file: `RerunRecordingSink` hardcodes `denied: false` (the `ToolResult` bridge
      drops the structural flag), so a D-177 reauthorize denial is re-recorded as a plain retryable
      error — carry the denial through or document the classification loss at the recording site.

## Progress
- (not started)

## Notes
- `crates/flux-sdk/src/test.rs` (`Scenario::check`) contains the reference wiring and a comment
  explaining the "every statement vanished" failure mode — the fix is reuse, not invention.
- The substitution-only path (`rerun_pinned`) already self-records under Halt and is sound; only the
  re-plan and Live-bridge paths are affected.

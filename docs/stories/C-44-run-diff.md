---
id: C-44
title: Run-diff + `flux diff` — align two runs, show where the plan or the world diverged
pillar: Core
status: done
design: docs/designs/time-machine.md
epic: time-machine
note: "Time Machine Phase 3 SHIPPED 2026-07-07 — `flux diff <A> <B>` classifies plan-changed vs same-plan-different-world per aligned statement, humanized via plan_source; exit 1 on divergence"
---

# Run-diff + `flux diff`

## Goal
Align two recorded runs and show exactly where they diverged — whether the **plan** changed or the
**same plan hit a different world**. The comparison surface that makes replay/fork actionable
(regression: did behavior change? counterfactual: how did the fork differ from the original?).

## Acceptance
- [ ] `run_diff(a, b) -> RunDiff` pure read-model in `crates/flux-events/src/projection.rs` (L2; may
      depend on flux-lang L0 for `stmt_hash16`): align statements by top-level `node` index, classify
      by `stmt_hash16` (`runtime.rs:101`) for **plan** divergence and by aligned `OpRecorded.content`
      (walked in `seq` order within each node) for **output** divergence. `DiffRow` = `Same | Plan |
      Output | OnlyA | OnlyB`.
- [ ] `flux diff <A> <B> [-o json]` (non-agent subcommand), resolving both via `resolve_run`.
      Renders through `CliSink` reusing `style_marked_plan`'s green/red/dim vocabulary: `Same` dim,
      `Plan` two-column, `Output` unified content diff.
- [ ] Failing-first test: two traces of the same plan where one op's `OpRecorded.content` differs →
      exactly one `DiffRow::Output` at the right node; a differing `stmt_hash16` → `DiffRow::Plan`.
- [ ] Full gate green; layering intact (no inner→outer edge — diff is L2, read-only).

## Progress
- 2026-07-07 DONE. `flux_events::projection::{stmt_rows, run_diff, StmtRow, DiffRow, RunDiff}`
  (pure L2 fold — no flux-lang runtime call needed: the trace rows already carry `stmt_hash16` in
  `StatementCompleted.stmt`, and cells attribute to statements by trace interleaving) +
  `flux diff <A> <B> [--json]` (non-agent subcommand).
  - Cell-count mismatch on an equal statement (e.g. a halt cutting it short) classifies as an
    `Output` divergence with the first unpaired cell — never silently `Same`.
  - CLI re-humanizes statement hashes by parsing each session's stored `plan_source` and hashing
    its top-level nodes (formatted one statement at a time); unknown hashes render `<hash>`.
  - Failing-first test `run_diff_classifies_plan_vs_output_divergence`: one differing recorded
    content → exactly one `Output` row at the right node; differing `stmt_hash16` → `Plan` row;
    self-diff → `identical`. Live smoke: `flux diff s_450 s_454` (a run vs its fork) prints the
    humanized plan divergence and exits 1; self-diff exits 0.
  - `children_of` landed with A-45; a tree-aware diff (parent + children pairing) is a natural
    follow-up if wanted. Full gate green.

## Notes
- No model, no store mutation — a pure fold over two `run_trace`s, so it belongs in the L2 events
  projection layer, not the L3 engine.

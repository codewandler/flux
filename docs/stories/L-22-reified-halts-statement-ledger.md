---
id: L-22
title: Reified halts + statement ledger + prefix fast-forward (resumable runtime mode)
pillar: Language
status: ready
priority: 3
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: the runtime core of patch-and-continue — failure returns Ok(FlowOutcome{failure}) with the prefix transcript, completed statements ledger as append-only events, an edited re-emission fast-forwards the hash-matching prefix with value rehydration; prereq L-21 denial fatality SHIPPED (b84204d)
---

# Reified halts + statement ledger + prefix fast-forward

## Goal
Give the runtime a memory of what already ran. In a new **resumable mode** (opt-in entry point;
composites/nested bodies keep strict `Err` + structural fatality, F14), a failing top-level
statement is reified — `Ok(FlowOutcome { failure: Some(PlanHalt{node, stmt_hash16, op, kind,
message, plan}) })` with the prefix transcript intact — and every completed top-level statement is
ledgered as an append-only `StatementCompleted` event. A later run given a `ResumeLedger`
fast-forwards the longest content-hash-matching completed prefix, rehydrating each skipped
statement's recorded value (design: Part 2 normative semantics).

## Acceptance
- [ ] `FailureKind` (Denied/ConfirmDenied/AssertFailed/Runtime, `is_fatal()` mirroring
      `FlowError::is_fatal`), `PlanHalt`, `ResumeLedger`/`LedgerEntry`, `FlowOutcome.failure`, and
      `execute_flow_resumable(..)` land in `crates/flux-lang/src/runtime.rs`; `FlowError` itself
      unchanged. Failing-first: `halted_plan_reifies_failure_with_prefix_transcript`.
- [ ] `stmt_hash16` = sha256(canonical JSON of the statement)[..16]; hashing is key-order/formatting
      insensitive (`stmt_hash_is_canonical_across_reemission`).
- [ ] New additive `RunEvent` variants `StatementCompleted{plan,node,stmt,value,skipped}`,
      `PlanHalted{plan,node,stmt,op,kind,error}`, `PlanResumed{plan,prior,skipped}`
      (`crates/flux-lang/src/ast.rs`); append-only philosophy holds (latch consumption = appending
      `PlanResumed`).
- [ ] Fast-forward: matching prefix skips with value rehydration
      (`resumed_plan_skips_hash_matched_prefix_and_rehydrates_values` — `last`/`last_value` come
      from the recorded ValueId); first divergence executes
      (`divergent_prefix_statement_reruns`); skipped statements re-ledger with `skipped: true` so a
      second halt yields a self-contained ledger.
- [ ] Interplay invariants hold with tests: strict mode byte-identical behavior (composite fatality
      preserved); a failing `saga` statement compensates and never ledgers as completed; `once`
      unaffected; ledger fast-forward runs after the checkpoint cursor (max of both).
- [ ] Fatality classification matches `is_fatal` (`FlowError::Denied` → `Denied`, etc. — leans on
      L-21's structural denial, shipped b84204d).
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- MemStore-level tests belong here; the flux-flow store fold + loop policy is A-16.
- Halt reification point: the per-statement `exec_body(...)?` in `run_top_level`
  (`runtime.rs:813-822`) — the failing index is already in hand as the loop variable.
- Granularity: a failed top-level `each`/`repeat`/`parallel` re-runs wholly (out of scope to go
  finer; consistent with checkpoint/await).

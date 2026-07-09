---
id: L-22
title: Reified halts + statement ledger + prefix fast-forward (resumable runtime mode)
pillar: Language
status: done
priority:
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
- [x] `FailureKind` (Denied/ConfirmDenied/AssertFailed/Runtime, `is_fatal()` mirroring
      `FlowError::is_fatal`), `PlanHalt`, `ResumeLedger`/`LedgerEntry`, `FlowOutcome.failure`, and
      `execute_flow_resumable(..)` land in `crates/flux-lang/src/runtime.rs`; `FlowError` itself
      unchanged. Failing-first: `halted_plan_reifies_failure_with_prefix_transcript`.
- [x] `stmt_hash16` = sha256(canonical JSON of the statement)[..16]; hashing is key-order/formatting
      insensitive (`stmt_hash_is_canonical_across_reemission`).
- [x] New additive `RunEvent` variants `StatementCompleted{plan,node,stmt,value,skipped}`,
      `PlanHalted{plan,node,stmt,op,kind,error}`, `PlanResumed{plan,prior,skipped}`
      (`crates/flux-lang/src/ast.rs`); append-only philosophy holds (latch consumption = appending
      `PlanResumed`).
- [x] Fast-forward: matching prefix skips with value rehydration
      (`resumed_plan_skips_hash_matched_prefix_and_rehydrates_values` — `last`/`last_value` come
      from the recorded ValueId); first divergence executes
      (`divergent_prefix_statement_reruns`); skipped statements re-ledger with `skipped: true` so a
      second halt yields a self-contained ledger.
- [x] Interplay invariants hold with tests: strict mode byte-identical behavior (composite fatality
      preserved); a failing `saga` statement compensates and never ledgers as completed; `once`
      unaffected; ledger fast-forward runs after the checkpoint cursor (max of both).
- [x] Fatality classification matches `is_fatal` (`FlowError::Denied` → `Denied`, etc. — leans on
      L-21's structural denial, shipped b84204d).
- [x] Gate green.

## Progress
- 2026-07-02: Implemented end-to-end in `crates/flux-lang` only (flux-flow host wiring stays A-16).
  `FailureKind` lives in `ast.rs` beside `RunEvent` (keeps `ast.rs` dependency-free; its `classify`
  is an inherent `impl` in `runtime.rs`, the one module that already depends on `FlowError`).
  `PlanHalt`/`LedgerEntry`/`ResumeLedger`/`FlowOutcome.failure`/`execute_flow_resumable` +
  `run_top_level_resumable` land in `runtime.rs`, reusing `flow_key`/`sha256_hex` for the new
  `stmt_hash16`. Ledger fast-forward indexes `LedgerEntry` by top-level `NodeId` (not vector
  position) so `checkpoint`/`await` gaps in the ledger don't corrupt the walk; composes with the
  checkpoint cursor as `ledger_end.max(checkpoint_next)`. `ResumeLedger::fold(events)` (pure, over
  `&[RunEvent]`) implements the "last `PlanHalted` with no later `PlanResumed`" latch fold so tests
  exercise the real mechanism, not a hand-rolled ledger. Added `render_statement` to `render.rs`
  (thin wrapper over the existing private `head`) for A-16/A-17's ✓/✗/· marker rendering. 10 new
  tests incl. the 3 named failing-first tests, a hash-stability test, saga/once/checkpoint interplay
  tests, and a strict-vs-resumable parity test; spot-verified two of them (rehydration, fatality
  classification) by breaking the implementation and confirming they fail for the right reason
  before restoring it. Gate: `cargo build/test/clippy -D warnings` on `-p flux-lang` all green (196
  lib tests + roundtrip/skill_in_sync/text_roundtrip/doctest), `cargo fmt -p flux-lang` clean.
  flux-flow itself does not currently build (a concurrent session's in-flight A-12/A-13 work on
  `compile.rs`/`loop_host.rs`, unrelated to this story's `FlowOutcome`/`RunEvent` additions — no
  downstream crate constructs `FlowOutcome` by struct literal, so nothing there needed a mechanical
  fix for this change).

## Notes
- MemStore-level tests belong here; the flux-flow store fold + loop policy is A-16.
- Halt reification point: the per-statement `exec_body(...)?` in `run_top_level`
  (`runtime.rs:813-822`) — the failing index is already in hand as the loop variable.
- Granularity: a failed top-level `each`/`repeat`/`parallel` re-runs wholly (out of scope to go
  finer; consistent with checkpoint/await).

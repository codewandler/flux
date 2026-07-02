---
id: A-16
title: Loop-host resume policy + structured feedback contract (latch fold, denial guard, suffix-scoped approval)
pillar: Agent
status: done
priority: 4
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: run_plan consumes the halt latch (fold over events.db, no new table), refuses hash-identical re-emission of denied statements, scopes approval to the suffix that will actually run, and feeds ✓/✗/· markers + machine-readable failure back to the planner
---

# Loop-host resume policy + feedback contract

## Goal
Wire L-22's primitive into the loop host (design: Part 2, `run_plan` order of operations): fold the
open halt latch from the event log, apply the denial re-emission guard, scope risk/approval to the
suffix that will actually run, execute resumable with the ledger, and return the structured
feedback contract (`failure{node, stmt, op, kind, fatal, message, completed[]}` + the ✓/✗/·-marked
transcript) so the model can repair surgically.

## Acceptance
- [x] `FlowStore::open_halted_plan(session)` — fold over run-events (last `PlanHalted` with no later
      `PlanResumed`, plus that plan's `StatementCompleted` ledger), pattern of `once_lookup`
      (`crates/flux-flow/src/state.rs:83-149`); no new SQLite table. Cross-process by construction.
- [x] `run_plan` halt arm: structured transcript (prefix outputs + `[plan halted at step N of M]` +
      rendered ✓/✗/· plan + kind-specific guidance, placed before the `cap_loop_feedback` tail) +
      machine-readable `failure` on the op Outcome. Failing-first:
      `run_plan_feeds_structured_halt_and_prefix_transcript`.
- [x] Resume: `second_run_plan_consumes_latch_and_skips_completed_prefix` (latch is one-shot —
      `PlanResumed` appended even at zero skips: `unrelated_next_plan_consumes_latch_with_zero_skips`).
- [x] Denial guard: `denied_statement_reemission_is_refused_not_redispatched` — a halt of kind
      denied/confirm_denied + a new plan containing the same `stmt_hash` returns an informational
      transcript without executing; a different approach flows normally.
- [x] Approval scoping: `plan_risk_with_composites` sees only the to-run suffix (user not
      re-prompted for completed writes); the `flow.plan` observation carries ✓-done/•-to-run
      markers. Divergence before the failed index adds the "will RE-RUN (incl. side effects)"
      transcript warning.
- [x] Guards: `LoopGuard` failure key `halt:{op}:{stmt}:{kind}` (same statement failing the same
      way escalates at existing STALL thresholds); silent-success guard untouched (byte-identical
      re-emission after a halt runs = retry of just the failed statement).
- [x] Cross-turn: fresh-turn `plan()` injects one ephemeral `[resume context]` message when a latch
      is open.
- [x] Gate green.

## Progress
- 2026-07-02: Implemented end-to-end in `flux-flow` (`state.rs`, `runtime.rs`, `loop_host.rs`):
  `FlowStore::open_halted_plan` (delegates the ledger half to `ResumeLedger::fold`, reconstructs the
  halt event itself via the same `once_lookup`-style rev/find_map); a resumable composites-aware
  wrapper `execute_flow_resumable_with_composites` used only by `run_plan`; the full `run_plan` order
  of operations (fold latch → denial re-emission guard → in-host prospective skip-prefix →
  suffix-scoped `plan_risk_with_composites` + ✓/·-marked `flow.plan` observation → execute resumable
  → structured halt feedback contract with a machine-readable `failure` object); a fresh-turn
  `[resume context]` ephemeral injection in `plan()`; the `LoopGuard` structured `halt:{op}:{stmt}:{kind}`
  key reuses the existing stall/escalate/stop machinery (`guard_transcript_with_key`).
- 2026-07-02: While writing `unrelated_next_plan_consumes_latch_with_zero_skips`, found and fixed a
  real bug in L-22's shipped `flux_lang::runtime::ResumeLedger::fold` — the latch-closing check
  compared `PlanResumed::plan` (the NEW plan's key) against the open latch instead of
  `PlanResumed::prior` (the OLD plan's key it actually closes), so the latch would only ever clear on
  a byte-identical resume — the common edited/corrected-plan case would leave it open forever. Fixed
  with a one-line change plus a regression test in `flux-lang` (`fold_clears_the_latch_after_a_non_identical_resume`),
  confirmed failing before the fix. Also made `flux_lang::runtime::stmt_hash16` `pub` (was private) so
  the host can compute the same statement-identity hash for the denial guard and the in-host
  prospective skip-prefix preview, without re-deriving or duplicating the hashing scheme.
- Gate: `cargo build/test/clippy -D warnings` green for `-p flux-lang` and `-p flux-flow`
  (197 + 165 tests respectively, including 12 new A-16 tests + 1 new flux-lang regression test);
  `cargo fmt -p flux-flow -p flux-lang` applied; full-workspace `cargo build --workspace` green.

## Notes
- Depends on L-22. The `Err` arm of `run_plan` (`loop_host.rs:742-757`) remains for infrastructure
  errors only.
- **A-16 review decision (design open question #4 — should a High-risk prefix-edit re-run escalate
  to a confirm?):** No new confirmation surface. Kept suffix-scoped plan-level approval as the ONE
  mechanism: `run_plan` always recomputes `plan_risk_with_composites` over the actual to-run suffix
  before every resumed execution, so if a prefix edit (or the divergence itself) pulls a High-risk or
  destructive op into that suffix, the existing plan-level `approve_plan` prompt already fires again
  on THAT risk — there is no code path where a High-risk re-run can execute silently pre-approved.
  Per-op dispatch gating is the second, always-on backstop (a destructive op re-escalates even inside
  an approved scope). Rationale for not building a dedicated "High-risk re-run" confirm: (1) cheap —
  it reuses a mechanism this story already had to build (suffix-scoped risk) instead of a new UI
  surface; (2) conservative — the divergence-before-failure "will RE-RUN (including their side
  effects)" transcript warning plus the recomputed risk together disclose the exact same information a
  bespoke confirm would, just through the existing approval + feedback channels; (3) consistent with
  the design's own residual note ("effectful re-runs on prefix edits remain possible by design"). If a
  High-risk re-run experience proves too easy to click through in practice, add a distinct
  confirmation surface as a follow-up story — not blocking A-16.

---
id: D-181
title: Scope the resurrect ledger and crash tail to the interrupted turn, not the session
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 1
note: "review finding (2026-07-28): session-wide fold can fast-forward on a PREVIOUS turn's progress or serve a previous turn's cells"
---

# Scope the resurrect ledger and crash tail to the interrupted turn, not the session

## Goal
Make Resurrect's exactly-once claim hold per *turn*. Today `resurrect`/`interrupted` fold the
whole-session trace: the `ResumeLedger` gathers `StatementCompleted` rows by content-addressed
`flow_key` only (`ResumeLedger::from_interrupted`, `crates/flux-lang/src/runtime.rs`), and
`crash_tail_cells` (`crates/flux-flow/src/resurrect.rs`) takes every `OpRecorded` after the last
statement boundary anywhere in the session. Events already carry `turn_id` and the store already has
`load_turn`, so turn-scoping was available and should be used.

Confirmed failure scenarios (2026-07-28 review):
1. **Identical-plan restart with zero progress**: turn 1 runs plan P to completion; turn 2
   re-accepts the identical plan (same `flow_key`) and crashes before its first
   `StatementCompleted`. The ledger returns turn 1's full completed set; resurrect fast-forwards
   everything, executes none of turn 2's side effects, and closes the turn `"resurrected"` with
   `diverged: None`. (The existing test `from_interrupted_keeps_only_the_last_execution_on_restart`
   covers only restarts *with* progress.)
2. **Mixed sessions**: a completed natively-dispatched turn records cells but no statement boundary,
   so its cells land in the crashed turn's `ResumeTape` and can be *served* — a matching
   `(op, input_hash)` gets a previous turn's output and its real side effect never fires. The
   leftover-cell latch fires only after the turn already closed on the wrong service.
3. **Purely native sessions**: no statement boundary at all → empty crash tail → every completed op
   of the crashed turn re-fires live. Exactly-once silently degrades to at-least-once for the whole
   turn on precisely the sessions the SDK produces.

## Acceptance
- [x] The resume ledger and the crash-tail cell slice are built from the interrupted **turn's**
      events only (`turn_id`-scoped), not the whole session trace.
- [x] Failing-first test for scenario 1: identical plan re-accepted in a later turn, crash before
      any progress → resurrect re-runs the turn from statement 0 (nothing fast-forwarded from the
      earlier turn).
- [x] Failing-first test for scenario 2: authored turn → completed native turn → crashed authored
      turn; the native turn's cells are never served into the resurrected turn.
- [x] Scenario 3 documented honestly: state what a purely native crashed turn can and cannot
      resurrect, and make `interrupted()`/`ResurrectReport` say so instead of silently re-firing.
- [x] The related content-vs-turn keying in `rerun_pinned`'s turn filter and `replay_turns_prefix`
      (`crates/flux-flow/src/whatif.rs` — `flow_key` set membership lets a repeated identical plan
      leak the target turn's execution into the prefix) is fixed with the same turn attribution, or
      split into its own follow-up with a test pinning the leak.

## Progress
- 2026-07-28: Implemented. `RunEvent`s turned out to carry no `turn_id` at write time anywhere in
  the live pipeline (`FlowStore::append_event` → `EventStore::record_run_event` never calls
  `NewEvent::in_turn`; wiring that through the write path lives in `flux-flow/src/state.rs` and
  `flux-events`, outside this story's file ownership). Fixed on the READ side instead: added
  `crate::cassette::turn_run_trace`/`turns_run_trace` (`crates/flux-flow/src/cassette.rs`), which
  positionally windows `EventStore::load_stream` between one (or several) turns' own
  `TurnStarted.global_seq` boundaries and reuses the existing `flux_events::run_trace` projection —
  achieving the identical `turn_id`-scoped partition as a pure read-side fix, no write-path or
  cross-crate change. `resurrect.rs`'s `interrupted`/`resurrect` and `whatif.rs`'s
  `selected_execution_keys`/`replay_turns_prefix` now derive their traces from this instead of
  `EventStore::run_trace(session)` (whole session) or `flow_key` set-membership filtering.
  `ResumeLedger::from_interrupted` (`flux-lang/src/runtime.rs`) needed no logic change — it was
  already correctly turn-local once given a turn-scoped input slice; only its doc comments were
  extended to state the turn-scoping contract on the input it expects.
  Added `ResurrectReport::unanchored_cells: usize` (additive field on the already `#[non_exhaustive]`
  struct — verified `cargo check -p codewandler-flux-sdk --tests` still compiles unchanged) for
  scenario 3's honest reporting, folded into `diverged` too.
  6 new failing-first tests (regression-pinned by temporarily reverting the fix in place, confirming
  each fails for the intended reason, then restoring it): 3 in `resurrect.rs`
  (`identical_plan_reaccepted_in_a_later_turn_resurrects_from_statement_zero`,
  `a_completed_native_turns_cells_are_never_served_into_a_later_crashed_turn`,
  `a_purely_native_crash_tail_reports_its_unanchored_cells_honestly_instead_of_silently_refiring`)
  and 2 in `whatif.rs` (`rerun_pinned_turn_filter_does_not_leak_a_repeated_identical_plans_other_turn`,
  `replay_turns_prefix_does_not_leak_the_target_turns_identical_plan`).
  Also disabled the `Executor`'s read-only op cache (`.with_op_cache(false)`) in
  `resurrect.rs`'s shared `counting_tool_executor` test helper — it's ON by default and was
  silently memoizing the new tests' 3 back-to-back identical zero-arg `counted()` dispatches,
  an orthogonal optimization unrelated to Resurrect's own exactly-once bookkeeping.
  Gate: `cargo test -p codewandler-flux-flow -p codewandler-flux-lang` (184 + 361 + integration
  suites, all green), `cargo clippy -p codewandler-flux-flow -p codewandler-flux-lang --all-targets
  -- -D warnings` (clean), `cargo fmt --check -p codewandler-flux-flow -p codewandler-flux-lang`
  (clean). A whole-workspace `cargo fmt --all -- --check` shows one pre-existing diff in
  `crates/flux-sdk/src/test.rs`, outside this story's file ownership (a concurrent session's
  in-progress work) and untouched here.

## Notes
- Same root cause across all findings: content-addressed plan identity (`flow_key`) with no turn
  attribution. Prefer threading `turn_id` through the fold over inventing a second identity.
- Keep the honest at-least-once window for an op interrupted mid-dispatch (no cell) — that part of
  the design is deliberate and documented; this story is only about cross-turn contamination.

---
id: D-184
title: Close the Lab's honesty gaps — is_clean vs live calls, golden-update reports, silent no-op substitutions
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 3
note: "review finding (2026-07-28): three spots report cleaner than reality — the opposite of the Lab's stated honesty contract"
---

# Close the Lab's honesty gaps — is_clean vs live calls, golden-update reports, silent no-op substitutions

## Goal
Three confirmed spots where the Lab reports cleaner than reality (`crates/flux-sdk/src/test.rs`,
`crates/flux-sdk/src/whatif.rs`), each the opposite of the design's honesty contract:

1. **`Report::is_clean()` ignores `model_live`.** A `check()` re-drive that missed the golden
   cassette and fell through to the real provider (real spend) but reproduced the plan/world reports
   `is_clean() == true`. The field doc calls `is_clean()` "the whole-report pass/fail a CI guard
   reads" — such a guard silently makes live model calls every run.
2. **`check()` under `FLUX_GOLDEN=update` fabricates its `Report`** (hardcoded `model_live: 1`,
   empty identical diff, `plan_changed`/`left_world` false), so `is_clean()` is unconditionally
   true. `FLUX_GOLDEN=update` accidentally exported in CI converts every `check()` gate into a
   silently-passing live re-record.
3. **`substitute_at` on a node with no dispatch is a silent no-op** (`node_to_cell_index → None`):
   a typo'd node id yields "identical, hermetic, no change" instead of an error.

## Acceptance
- [x] `is_clean()` is false (or a distinct, impossible-to-miss verdict exists) when
      `model_live > 0`; test pins that a live fall-through fails a CI-style guard.
- [x] `FLUX_GOLDEN=update` mode returns a `Report` reflecting what actually happened (or a distinct
      updated-baseline outcome that `is_clean()`-style guards reject); test pins that the fabricated
      always-clean report is gone.
- [x] `substitute_at` with a node id that maps to no recorded dispatch returns an error naming the
      node, never a silent identical run; test pins it. — fixed in D-182 pass (same file,
      `crates/flux-sdk/src/whatif.rs`).
- [x] Nit swept in the same pass: `authorize` classifies an unknown tool as `Deny` while
      `dispatch_outcome` reports the same refusal with `denied: false` — align the classification or
      document the drift where both are defined (`crates/flux-runtime/src/lib.rs`).

## Progress
- 2026-07-28: `Report::is_clean()` (`crates/flux-sdk/src/test.rs`) now hard-fails on
  `model_live > 0`, in addition to `plan_changed`/`left_world` — `!plan_changed && !left_world &&
  model_live == 0`. Doc comments on `Report` and `is_clean()` updated. Pinning: three new unit tests
  in a `#[cfg(test)] mod tests` at the bottom of `test.rs` construct `Report` directly and vary one
  field at a time (`is_clean_hard_fails_on_any_live_model_fall_through`,
  `is_clean_still_fails_on_plan_or_world_drift_alone`, `a_ci_style_guard_rejects_a_live_fall_through`
  — the last one models an actual CI guard fn reading only the bool). Extended the existing
  `check_counts_every_model_call_the_golden_does_not_cover` integration test
  (`crates/flux-sdk/tests/agent_test_kit.rs`) with an explicit `!report.is_clean()` assertion.
  `tests/agent_golden.rs`'s `editing_the_system_prompt_surfaces_a_plan_divergence` (which already
  asserts `!report.is_clean()` with `model_live>0`) still passes unchanged.
- 2026-07-28: `Scenario::check`'s `FLUX_GOLDEN=update` branch (`crates/flux-sdk/src/test.rs`) no
  longer fabricates a `Report`. `Scenario::record` was split into a private
  `record_with_call_count` that also returns the recording turn's real live-call count; `check` now
  captures the outgoing golden's trace/text before `record` overwrites the fixture, re-records, then
  computes a real `run_diff` between the previous and new golden and reports the actual
  `model_live` count. `left_world` is unconditionally `true` for this branch (a re-baseline is a
  live operation by definition, never a pinned re-drive), so the report is never clean regardless of
  what the diff says — this also covers the case where the re-recording happens to make zero
  observable live calls. New pinning test
  `flux_golden_update_check_never_reports_clean` (`crates/flux-sdk/tests/agent_test_kit.rs`) records
  a golden, re-baselines it under `FLUX_GOLDEN=update` with a counting provider, and asserts
  `!report.is_clean()` and `report.model_live == live_calls` (the real count, not a hardcoded `1`).
- 2026-07-28: `substitute_at`'s silent-no-op fix is explicitly OUT of this pass's scope —
  `crates/flux-sdk/src/whatif.rs` is owned by a concurrent agent (D-182). Left unticked above.
- 2026-07-28: fixed in the D-182 pass (same file, same session as the note above): `build_frozen`
  (`crates/flux-sdk/src/whatif.rs`) now returns `Result<FrozenTape>` and errors, naming the node,
  when `node_to_cell_index` finds no recorded dispatch for a `.substitute_at(node, _)` target. Test
  `substitute_at_a_dead_node_errors_instead_of_silently_no_opping`
  (`crates/flux-sdk/tests/whatif.rs`). Checkbox above ticked; see `docs/stories/D-182-whatif-replan-
  self-recording.md` for the full pass's Progress notes.
- 2026-07-28: unknown-tool classification drift (`crates/flux-runtime/src/lib.rs`) — aligned rather
  than documented. `dispatch_outcome`'s unknown-tool branch now passes `denied: true` to
  `finish_dispatch` (was `false`), matching `authorize`'s `Deny(...)` for the same case.
  Justification found in the existing code, not invented: `ast::Node::Retry`'s own doc comment
  already names "policy denial, unknown op" as the two cases that must never be retried, and
  `flux_lang::runtime::call_failure` keys off `CallOutcome::denied` to choose between the fatal
  `FlowError::Denied` (never retried) and the transient `FlowError::Runtime` (retried) — so the old
  `denied: false` meant a typo'd op name silently burned `retry`/`loop` attempts on a call that could
  never succeed. Updated `DispatchOutcome::denied`'s doc comment to name this case. Extended the
  existing `authorize_denies_an_unknown_op` test (`crates/flux-runtime/src/lib.rs`, near the
  `authorize_and_dispatch_report_the_same_refusal` refusal-parity test) to also call
  `dispatch_outcome` and assert `outcome.denied` plus wording parity with `authorize`'s verdict.
- Gate (package-scoped, since concurrent agents are mid-flight elsewhere in the workspace):
  `cargo test -p codewandler-flux-sdk --features test-kit` (all green, `agent_test_kit.rs` 12/12,
  `agent_golden.rs` 4/4, lib 61/61 incl. the 3 new `is_clean` unit tests), `cargo test -p
  codewandler-flux-sdk` (default features, 58/58 lib + integration suites, test-kit files correctly
  not compiled), `cargo test -p codewandler-flux-runtime` (95/95, incl. the extended parity test),
  `cargo clippy -p codewandler-flux-sdk --features test-kit --all-targets -- -D warnings` and `cargo
  clippy -p codewandler-flux-runtime --all-targets -- -D warnings` (both clean), `cargo fmt --all --
  --check` (clean). Two transient workspace-wide compile breaks from concurrent agents' in-flight
  edits (`flux-events` duplicate trait method, `flux-flow` `ResurrectReport` missing field) were hit
  and waited out — not touched, not caused by this story's changes.

## Notes
- All three are small, local changes; the work is deciding the reporting shape (hard-fail vs
  distinct verdict) once, consistently, and writing the pinning tests.

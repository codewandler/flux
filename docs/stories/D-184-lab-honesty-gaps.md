---
id: D-184
title: Close the Lab's honesty gaps — is_clean vs live calls, golden-update reports, silent no-op substitutions
pillar: Agent
status: ready
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
- [ ] `is_clean()` is false (or a distinct, impossible-to-miss verdict exists) when
      `model_live > 0`; test pins that a live fall-through fails a CI-style guard.
- [ ] `FLUX_GOLDEN=update` mode returns a `Report` reflecting what actually happened (or a distinct
      updated-baseline outcome that `is_clean()`-style guards reject); test pins that the fabricated
      always-clean report is gone.
- [ ] `substitute_at` with a node id that maps to no recorded dispatch returns an error naming the
      node, never a silent identical run; test pins it.
- [ ] Nit swept in the same pass: `authorize` classifies an unknown tool as `Deny` while
      `dispatch_outcome` reports the same refusal with `denied: false` — align the classification or
      document the drift where both are defined (`crates/flux-runtime/src/lib.rs`).

## Progress
- (not started)

## Notes
- All three are small, local changes; the work is deciding the reporting shape (hard-fail vs
  distinct verdict) once, consistently, and writing the pinning tests.

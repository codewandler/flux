---
id: C-15
title: Per-turn efficiency metrics + canonical usage attribution keys
pillar: Core
status: done
priority:
note: TurnSummary now folds per-turn calls + call_usage, efficiency_summary()/flux usage report calls/turn + iters/turn + cache-read share + uncached-in/out per turn, and usage keys are stamped canonically at write time (canonical_model_spec) with an unambiguous read-side merge for legacy variants
---

# Per-turn efficiency metrics + canonical usage keys

## Goal
Make harness efficiency measurable (the Improve pillar needs a trend line for tokens-per-task) and
stop splitting usage totals across key spellings. Verified 2026-07-02: `CallUsage` per planner
call and `TurnEnded.usage` + iterations exist (`engine.rs:270-283,578-603`), but there is no
rollup of calls-per-turn or cache-hit share — and attribution keys are written inconsistently
across paths (C-11 Notes), so `flux usage` splits the same backend into multiple rows.

## Acceptance
- [x] **Failing-first:** `turns_fold_per_turn_call_counts_and_cache_usage` (flux-events) —
      `TurnSummary` gains `calls: u32` + folded per-call usage via the existing `sum_usage`.
- [x] **Failing-first:** `efficiency_summary_reports_calls_per_turn_and_cache_read_share` — pure
      `efficiency_summary(events) -> EfficiencyReport { turns, avg_calls_per_turn,
      avg_iterations_per_turn, cache_read_share, avg_uncached_input_per_turn,
      avg_output_per_turn }` (`cache_read_share = cache_read / (input + cache_read +
      cache_creation)`), with EventStore wrappers mirroring `cost_summary`. No new event kind.
- [x] `flux usage` renders one efficiency line per section (e.g. `12 turns · 2.3 calls/turn ·
      3.1 iter/turn · cache-read 78% · uncached-in 4.1k/turn · out 1.9k/turn`).
- [x] **Failing-first:** `canonical_model_spec_prefixes_bare_ids_and_preserves_specs` (flux-core)
      — `canonical_model_spec(provider, model)` in `flux_core::pricing` (keep specs containing
      `/`; else `provider/model`); stamped at write time: loop_host (:430), engine `TurnStarted`
      (:171-174), orchestrate sub-agent calls (:312, :726). Bedrock becomes `aws/…` (`rates_for`
      already strips provider+region).
- [x] **Failing-first:** `cost_summary_merges_bare_and_prefixed_model_keys` — projection-side,
      read-only migration: group by normalized merge key (provider-stripped,
      bedrock-region-stripped, alias-resolved); the append-only log is never rewritten. CHANGELOG
      caveat: an identical id genuinely served by two providers would merge.
- [x] Existing C-06 attribution tests updated to canonical keys (the intended cutover edge). Full
      gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P9 of the round).
- Done 2026-07-02. `TurnSummary` gains `calls` + `call_usage` (CallUsage fold); new
  `EfficiencySummary` (raw sums, merge-by-addition across streams) + `efficiency_summary()` fold,
  store wrappers `efficiency`/`efficiency_all`, and one dimmed line per `flux usage` section.
  `flux_core::pricing::canonical_model_spec(provider, model)` stamps `CallUsage` at all three write
  sites (loop-host planner + completion render via `provider.name()`, engine `begin_turn`,
  orchestrate child spawn) — aliases resolved, Bedrock regional prefixes stripped, unknown/mock
  providers left bare. Read side: `merge_legacy_keys` in `cost_summary` collapses same-provider
  canonical variants and folds a bare key into its prefixed sibling only when exactly ONE candidate
  provider exists (ambiguous bare keys stay separate — providers bill differently). 4 new tests.

## Notes
- Strictly after A-06: the completion-render call must land in `loop_host.calls` before the
  calls/turn + cache-share definitions freeze.
- Closes the attribution-key normalization noted in C-11's Notes.

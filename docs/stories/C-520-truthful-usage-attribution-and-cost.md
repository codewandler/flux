---
id: C-520
title: "Project truthful usage attribution and cost"
pillar: Core
status: backlog
epic: usage-observatory
note: "Keep provider, model, timestamp and price provenance explicit; never double-count Flux call and turn totals"
---

# Project truthful usage attribution and cost

## Goal

Turn C-519's source records into comparable usage facts without overstating what Flux knows. Provider,
model, time, token tiers, and cost provenance remain explicit, and Flux-native call plus legacy turn
data obey one non-duplicating selection rule.

## Acceptance

- [ ] A failing-first test named `call_usage_wins_without_doubling_turn_usage` uses one Flux fixture
      containing both `CallUsage` and `TurnEnded.usage` and proves per-call data is canonical while the
      turn total contributes only as the existing uncovered-turn legacy fallback.
- [ ] Normalization preserves source harness, session, raw model, canonical model, proven provider,
      timestamp precision, every `Usage` tier, calls, and cost provenance. Unknown provider, timestamp,
      usage, or price remains a typed unknown/unpriced state.
- [ ] A failing-first test named `routed_model_prefix_does_not_invent_provider` proves no provider is
      inferred solely from a model-name prefix; evidence-based attribution and `unknown` are the only
      outcomes.
- [ ] Independent calls sum fresh input, output, cache write, cache read, reasoning, audio, and other
      present tiers field-by-field rather than using live-context `Usage::accumulate`. The test
      `independent_calls_preserve_usage_tiers` fails if cache or context semantics collapse tiers.
- [ ] Cost is computed per call before aggregation and preserves reported-cost precedence plus mixed
      reported, table-estimated/subscription-equivalent, and unpriced coverage. The failing-first test
      `mixed_cost_provenance_survives_aggregation` proves unknown cost never renders or serializes as
      `$0` and historical estimates retain their pricing basis.
- [ ] C-519's `flux usage` parity remains green after the projection is adopted.

## Progress

- (not started)

## Notes

- Depends on [C-519](C-519-shared-cross-harness-usage-timeline.md).
- Reuse the selection and pricing semantics cited by [C-518](C-518-usage-observatory-epic.md) in
  `crates/flux-events/src/projection.rs`; do not create a competing accounting system.

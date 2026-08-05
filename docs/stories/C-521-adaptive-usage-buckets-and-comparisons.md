---
id: C-521
title: "Build adaptive usage buckets and comparisons"
pillar: Core
status: backlog
epic: usage-observatory
note: "Derive stable plot-width buckets, cumulative totals, hierarchy groups and previous-period deltas from the truthful timeline"
---

# Build adaptive usage buckets and comparisons

## Goal

Provide the deterministic analysis projection used by every observatory panel: adaptive timeline
buckets, cumulative totals, filtering, grouping, drill-down, sorting, and equal-previous-period
comparison over C-520's truthful usage facts.

## Acceptance

- [ ] A failing-first test named `adaptive_buckets_fit_plot_width_without_losing_totals` proves bucket
      count is bounded by the requested plot width while field-by-field token, call, session, and cost
      totals exactly match the selected source range.
- [ ] The projection supports 4h, 1d, 7d, and arbitrary valid custom ranges with explicit inclusive/
      exclusive boundary semantics. The same fixture, range, width, and filter always produce the same
      buckets and cumulative series.
- [ ] One filter set drives grouping and drill-down by harness, provider, model, and
      harness → provider → model; unknown provider/model and unpriced records remain visible groups
      rather than being dropped.
- [ ] A failing-first test named `previous_period_is_equal_length_and_adjacent` proves comparison uses
      the immediately preceding equal-length range, including stable zero-baseline and no-prior-data
      behavior without divide-by-zero, infinity, or fabricated percentages.
- [ ] Calls, distinct sessions, all token tiers, and reported/estimated/unpriced cost coverage can be
      selected and sorted without changing their underlying totals. Cumulative values are monotonic for
      additive metrics and end at the exact range total.
- [ ] Projection work is bounded by the selected buckets/groups after initial range ingestion; consumers
      do not need to scan the full history for every redraw.

## Progress

- (not started)

## Notes

- Depends on [C-520](C-520-truthful-usage-attribution-and-cost.md).
- This story owns data projection only. Static widgets belong to C-522 and replay state to C-523.

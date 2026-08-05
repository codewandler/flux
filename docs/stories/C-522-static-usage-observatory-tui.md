---
id: C-522
title: "Ship the static Usage Observatory TUI"
pillar: Core
status: backlog
epic: usage-observatory
note: "Make the observatory complete and responsive before motion: KPIs, route flow, timelines, comparisons, filters and inspection"
---

# Ship the static Usage Observatory TUI

## Goal

Ship a polished historical Usage Observatory that remains fully useful with animation disabled. One
cursor and filter state coordinate KPI totals, route flow, token/cost timelines, comparisons, and
metadata-only inspection across deliberate wide, medium, and compact layouts.

## Acceptance

- [ ] A failing-first state test named `observatory_panels_share_cursor_and_filters` proves KPI totals,
      route flow, timelines, table, and inspector all derive from the same selected range, cursor,
      grouping, metric, sort, and filter state.
- [ ] Operators can select 4h, 1d, 7d, and custom windows; group and drill through harness, provider,
      model, and harness → provider → model; sort/filter; inspect a focused bucket or usage item; and
      compare with the immediately preceding equal-length period.
- [ ] The static activity-flow view communicates each known harness → provider → model route while
      unknown provider and cost status remain labelled. Token and cost timelines, KPI totals, and the
      comparison table preserve C-520/C-521's exact values and provenance.
- [ ] Wide, medium, and compact layouts use the existing TUI theme rather than hardcoded colors and
      remain understandable in monochrome and with motion disabled.
- [ ] Snapshot/state coverage named `usage_observatory_layout_matrix` includes at least wide, narrow,
      empty, unknown-provider, partially-priced, burst-heavy, and focused-inspector states.
- [ ] Inspection loads and displays usage metadata only; prompts, assistant answers, tool arguments,
      and transcript bodies do not enter observatory state.
- [ ] C-140's live per-session `/usage` behavior remains available and is not silently redefined by the
      new historical view; entry-point and help closure remain owned by C-524.

## Progress

- (not started)

## Notes

- Depends on [C-521](C-521-adaptive-usage-buckets-and-comparisons.md).
- The static-first boundary is deliberate: C-523 may decorate this view with motion but cannot make
  any analysis or comparison depend on animation.

---
id: C-368
title: Require every catalog op to publish a risk tier in a risk-bearing table
pillar: Core
status: backlog
epic: structural-gate-blind-spots
design: docs/designs/structural-gate-blind-spots.md
note: "risk verification only walks rows under a `risk` header; coverage accepts a row in ANY table. 57 of 164 op rows sit in risk-less tables today, and the checked>60 floor leaves ~46 ops able to lose their published tier with both gates green"
---

# Require every catalog op to publish a risk tier in a risk-bearing table

## Goal

Close the gap between "this op has a documented row" and "this op has a documented risk tier", and
stop the website coverage check from being satisfiable by prose.

## Acceptance

- [ ] Every op in the production catalog has its reference row in a table that carries a Risk
      column; moving a row into a risk-less table reds the gate.
- [ ] The `checked > 60` floor is replaced by an exact expectation derived from the catalog size.
- [ ] `website/docs/language/ops.md` coverage requires a table row rather than a `contains("`name`")`
      substring (`crates/flux-cli/tests/website_contract.rs:579-582`) — the weakness C-248 fixed for
      the in-repo reference and never fixed here.
- [ ] Failing-first: move one op's row into a risk-less table and document another only in prose;
      both must red.

## Progress

- 2026-08-01 — mutations 8 and 9 from the design doc's table.

## Notes

- C-233 genuinely closed the "unresolved rows are silently skipped" hole; this is the adjacent one
  it did not cover.

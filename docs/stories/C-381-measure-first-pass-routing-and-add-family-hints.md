---
id: C-381
title: Measure first-pass capability routing and give first-party families routing hints
pillar: Agent
status: backlog
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "Family.routing_signals is wired end to end but populated ONLY from KIND_TURN_INTENT matchers, which no built-in group emits — only installed plugins reach it. And edcd9dcc enriched declare_intent with no story, no CHANGELOG entry and no test"
---

# Measure first-pass capability routing and give first-party families routing hints

## Goal

Make routing quality a measured property rather than an anecdote, and let first-party families use
the hint mechanism that already exists for plugins.

## Acceptance

- [ ] A fixed evaluation set of indirect requests with expected family sets, driven through the
      existing cassette machinery (`crates/flux-flow/src/cassette.rs`), scoring first-pass family
      precision, unnecessary-family rate, repair rate (`MAX_INTENT_ATTEMPTS = 2`, so a repair is
      directly observable), surfaced schema bytes and latency.
- [ ] Built-in groups can emit `KIND_TURN_INTENT` matchers so `routing_signals`
      (`crates/flux-flow/src/staged.rs:1482-1489`) renders for first-party families, not only for
      `implicit_plugin_group`.
- [ ] Commit `edcd9dcc`'s `declare_intent` enrichment is filed retroactively — it shipped an agent
      contract change with no story, no board row and no CHANGELOG entry — and gains a test
      asserting the six enriched fields are present and required in the emitted schema.
- [ ] The evaluation set includes a "run a workspace flow" fixture; it is expected to fail until
      C-377 lands, which is the point.
- [ ] No story here widens the surfaced catalog: the ledger records H's explicit rejection of an
      ambient all-tools catalog.

## Progress

- 2026-08-01 — filed from validation of ROUTE-01. The "richer intent fields" half already landed,
  untracked; the hints and measurement halves are open.

## Notes

- `edcd9dcc` is an instance of the repo's own "wiring nothing observes" class (cf. C-328, C-314).

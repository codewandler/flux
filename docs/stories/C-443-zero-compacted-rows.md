---
id: C-443
title: "Zero `Compacted` rows in 112,114 events — does compaction ever actually fire?"
pillar: Core
status: ready
priority: 5
design: docs/designs/docs-completeness.md
epic: docs-completeness
areas: [flux-flow, flux-events]
note: "⚠ found by A-145 while sweeping the event store for a real-run fixture. Not a docs problem: either the threshold is never reached, compaction is effectively disabled, or it fires without recording. Gates C-441, because a page describing behaviour nobody has observed is documentation of an intention"
---

# The feature with no instances

## Goal

Find out whether history compaction fires in real use, and make the answer visible.

## The finding

[A-145](A-145-a-real-run-as-the-mock-fixture.md) swept the local event store to build a real-run
fixture and reported **zero `Compacted` rows in 112,114 events** — enough that it could not construct a
compaction fixture at all, and had to leave `Compacted` in its *absent* column.

The machinery exists: `compact_threshold_chars` on the engine (`0` disables), `maybe_compact` in the
loop host, `EventKind::Compacted { messages }` in the log, and `FLUX_COMPACT_CHARS` in the config
reference.

Three possibilities, and they need different fixes:

1. **The threshold is never reached** in practice — sessions end first. Then the default is arguably
   wrong, and the docs should say reaching it is rare.
2. **Compaction is effectively disabled** — a default of `0`, or a path that never calls
   `maybe_compact`. Then it is a dormant feature.
3. **It fires and does not record.** ⚠ The worst case: history is being replaced with **no durable
   evidence that it happened**, which would silently corrupt every replay, export and reconstruction of
   an affected session.

## Acceptance

- [ ] **Failing-first**: a test driving a session past the threshold and asserting a `Compacted` event
      is recorded — failing at the merge base if possibility 3 holds.
- [ ] Which of the three it is, stated with evidence — the default value, the call path, and a check of
      whether any session in a store has ever crossed the threshold.
- [ ] ⚠ **If it is possibility 3, that is a correctness bug, not a tuning question**, and it outranks the
      documentation work entirely: a replaced history with no record of the replacement means
      `flux replay`, `flux export` and C-422's reconstruction are all reading a truncated past while
      believing it complete.
- [ ] The default threshold is either justified or changed, with the reasoning recorded.
- [ ] The answer is handed to [C-441](C-441-context-management-doc.md) in a form it can document
      honestly — including "this rarely fires" if that is the truth.

## Notes

- ⚠ A-145's sweep is one machine's store and one user's habits. Confirm the store is representative
  before concluding the threshold is wrong for everyone — but a 112k-event store with zero instances is
  a strong signal from *somewhere*.
- Feeds [C-422](C-422-the-render-projection.md), which has "pre- or post-compaction view?" as an open
  question it cannot currently settle against any real data.
- The Notes on `EventKind::Compacted { messages }` say it carries the replacement messages, so the log
  *can* answer "what was replaced" — worth confirming that survives whatever this finds.

## Progress

- Filed 2026-08-02 from A-145's event-store sweep.

---
id: C-24
title: "Advance the observation watermark only past successful writes"
pillar: Core
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "flush_observations writes fire-and-forget then stores the watermark unconditionally — a transient SQLite BUSY/disk error drops those observations AND jumps the watermark past them, so they're never retried; 'crash loses one batch' becomes 'a failed write loses that observation forever'"
---

# Advance the observation watermark only past successful writes

## Goal
Make observation flushing durable against a failing write, not just a crash. `flush_observations` records
each observation fire-and-forget and then stores the watermark unconditionally:
`for obs in &all[start..] { let _ = self.events.record_observation(...); }` then
`self.evidence_flushed.store(all.len(), …)` (`crates/flux-flow/src/engine.rs:598`). A transient
`record_observation` failure (WAL `BUSY`, serialization hiccup, momentary disk-full) drops those
observations while the watermark still jumps to `all.len()`, so they are never retried even after the DB
recovers — and the turn otherwise succeeds, so the loss is invisible.

## Acceptance
- [ ] Failing-first test with a stubbed event store that fails one `record_observation`: the watermark stops
      at the first failed index, and the dropped observations are re-attempted on the next flush (today they
      are lost permanently).
- [ ] Advance `evidence_flushed` only past observations whose write returned `Ok` (track the first failure and
      stop).
- [ ] No behavioural change on the all-success path.

## Progress
- 2026-07-03 DONE — `flush_observations` (via new `flush_tail`) advances the watermark only past successful writes (stops at first failure; dropped observations retried next flush). Test: `flush_tail_stops_at_first_failed_write_and_retries_next_flush`. Full gate green.

## Notes
- Evidence: `crates/flux-flow/src/engine.rs:598` (unconditional watermark store after `let _ = record`).
- Residual of [C-14](C-14-durable-evidence-trail.md). Pairs with [C-25](C-25-events-db-busy-timeout.md)
  (busy_timeout reduces how often the write fails in the first place).
  Design: [library-hardening](../designs/library-hardening.md).

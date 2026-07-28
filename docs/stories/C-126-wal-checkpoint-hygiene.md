---
id: C-126
title: WAL checkpoint hygiene for long-lived daemons — bound events.db-wal growth
pillar: Core
status: backlog
epic: event-store-concurrent-use
design: docs/designs/event-store-concurrent-use.md
note: "a long-lived serve daemon with pinned readers can keep the WAL from checkpointing; a periodic wal_checkpoint(TRUNCATE) in the serve loop bounds the sidecar"
---

# WAL checkpoint hygiene for long-lived daemons — bound events.db-wal growth

## Goal
Under WAL mode, checkpointing back into the main database file needs a moment with no pinned
readers; a long-lived `flux app run --serve` daemon that always holds read snapshots can defer
checkpoints indefinitely, growing `events.db-wal` without bound. Add a periodic
`PRAGMA wal_checkpoint(TRUNCATE)` (or `PASSIVE` + escalation) driven from the serve loop so the
sidecar stays bounded on busy shared stores.

## Acceptance
- [ ] A checkpoint hook exists on `EventStore` (SQLite backend only; Postgres no-op) and the serve
      loop invokes it on an idle/periodic cadence — proven by a failing-first test that grows the
      WAL past a threshold and asserts the hook shrinks it.
- [ ] Checkpointing never blocks or errors a concurrent writer/reader (busy → skip, retry next
      tick — never a turn-visible failure).
- [ ] Interactive CLI behavior is unchanged (short-lived processes already checkpoint on close).

## Progress
- (not started)

## Notes
- Condition to verify first: whether `events.db-wal` growth is actually observed under a served
  workload — the design doc (section 4.3) files this as conditional hygiene, not a known bug.
- R6 applies: a TRUNCATE checkpoint competes for the write lock; schedule off-peak like prunes.

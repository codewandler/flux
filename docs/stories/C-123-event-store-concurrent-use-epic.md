---
id: C-123
title: "Event-store concurrent use — visibility, proof, and hygiene (epic)"
pillar: Core
status: done
epic: event-store-concurrent-use
design: docs/designs/event-store-concurrent-use.md
note: "EPIC — the concurrency envelope (WAL + busy_timeout + BEGIN IMMEDIATE + UNIQUE backstops, PG advisory locks) is built and tested; this funds contention visibility, a multi-process stress proof, and WAL checkpoint hygiene"
---

# Event-store concurrent use — visibility, proof, and hygiene (epic)

## Goal
Multiple flux processes sharing one `~/.flux/events.db` is supported by design (C-25 busy_timeout,
C-87 idempotent appends, D-73/D-76 Postgres advisory locks) — but the envelope's edges are
invisible and only single-process-tested on the SQLite side. This epic makes contention observable
before it becomes a 5s failure, proves the multi-process story with a real subprocess stress test,
and keeps long-lived daemons' WAL sidecars bounded. The rules and limits for operators are
documented in [docs/designs/event-store-concurrent-use.md](../designs/event-store-concurrent-use.md).

## Acceptance
- [ ] C-124 (append-contention visibility), C-125 (multi-process stress test), and C-126 (WAL
      checkpoint hygiene) are filed on the board and each ships with a failing-first test.
- [ ] The design doc's R1–R7 rules stay true as the stories land (no new side-channel state, no
      second write path, append errors stay loud).
- [ ] Epic closes when all three stories are done or explicitly retired with a recorded reason.

## Progress
- 2026-07-28 epic filed from the `.flux/plans/event-store-concurrent-use.md` concurrency review;
  guidance promoted to `docs/designs/event-store-concurrent-use.md`.
- 2026-07-28 C-124 shipped in v0.32.0: the one `begin_write` seam times its wait and warns past a
  threshold, so contention is observable before it becomes a 5s failure.
- 2026-07-29 **epic CLOSED** — C-125 and C-126 landed in v0.33.0, all three stories delivered.
  C-125 replaces the two-connection in-process test with real OS processes and, more usefully,
  proves it guards something: reverting C-25's `BEGIN IMMEDIATE` makes it fail reliably. C-126
  verified the premise before building the fix — a pinned reader really does hold `events.db-wal`
  growth unreclaimed — then bounded it with a `wal_checkpoint(TRUNCATE)` hook on a dedicated
  zero-busy-timeout connection, swallowing `SQLITE_BUSY` so a checkpoint can never surface as a
  turn-visible failure. The design doc's R1–R7 rules all still hold: no side-channel state, no
  second write path, append errors stayed loud, and R6's "schedule off-peak like prunes" is
  honored by the five-minute serve-loop cadence.

## Notes
- Prior art this builds on: C-25 (busy_timeout), C-24 (watermark-after-success), C-87 (idempotent
  append + prune correctness), D-72/D-73/D-75/D-76/D-77 (backend seam, PG backend, retention).
- Key sources: `crates/flux-events/src/store/sqlite.rs:253-267,315-349`,
  `crates/flux-events/src/store/postgres.rs:25-30,105-108`, tests at
  `crates/flux-events/src/store/mod.rs:2487,2605,2662`.

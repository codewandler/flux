---
id: C-123
title: "Event-store concurrent use — visibility, proof, and hygiene (epic)"
pillar: Core
status: backlog
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

## Notes
- Prior art this builds on: C-25 (busy_timeout), C-24 (watermark-after-success), C-87 (idempotent
  append + prune correctness), D-72/D-73/D-75/D-76/D-77 (backend seam, PG backend, retention).
- Key sources: `crates/flux-events/src/store/sqlite.rs:253-267,315-349`,
  `crates/flux-events/src/store/postgres.rs:25-30,105-108`, tests at
  `crates/flux-events/src/store/mod.rs:2487,2605,2662`.

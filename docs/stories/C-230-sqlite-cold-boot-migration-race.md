---
id: C-230
title: "Two flux processes cold-booting one fresh events.db race the schema migration"
pillar: Core
status: done
design:
note: "found by accident during A-107: four processes cold-booting the SAME brand-new events.db died with `duplicate column name: account` — D-76 fixed exactly this for Postgres with a `flux:ddl` advisory lock; SQLite has no equivalent, and every existing multi-process test creates the DB first, so nothing covers it"
---

# Two flux processes cold-booting one fresh `events.db` race the schema migration

## Goal
Make first-boot schema migration safe when more than one `flux` process starts concurrently against a
**brand-new** SQLite store. Today one of them can die with

```
Other("event store: duplicate column name: account")
```

because the migration is not serialised across processes. Once the database exists, the race is gone
— which is precisely why nothing has caught it.

## Acceptance
- [x] Concurrent first-boot is serialised, so N processes opening a non-existent `events.db`
      simultaneously all succeed with one consistent schema. **Failing-first test**: spawn several
      processes against a fresh store path with **no orchestrator-side bootstrap**, and assert every
      one opens cleanly — it fails today on `duplicate column name`.
- [x] The fix is the SQLite analogue of D-76's Postgres `flux:ddl` advisory lock, or a stated reason
      why a different mechanism is correct here (SQLite has no advisory-lock primitive, so this is a
      real design choice — a lock file, `BEGIN IMMEDIATE` around the migration, or serialising on the
      existing store mutex are all candidates with different failure modes under NFS/containers).
- [x] The existing multi-process tests keep passing **unchanged**. They bootstrap the DB before
      spawning, which is why they never reproduced this; do not weaken them to accommodate the fix.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — **implemented, gate green, failing-first verified by the coordinator.** The implementor
  was killed mid-task by the org spend limit before it could tick these boxes or file a report, so the
  evidence below was produced at integration rather than handed over.
  - Failing-first, run against the merge base with only `store/sqlite.rs` reverted and the new test
    kept: `concurrent_cold_boots_serialize_the_schema_migration` **FAILS** with
    `worker cold-boots the fresh store: Other("event store: database is locked")`. Note it fails on
    the **WAL-conversion** race rather than on `duplicate column name: account` — the conversion
    happens before the migration, so it is hit first. That independently corroborates that there were
    **two** distinct cold-boot races here, not one.
  - Mechanism: the whole bootstrap runs inside one `BEGIN IMMEDIATE` transaction. Chosen over a lock
    file because SQLite's write lock is transaction-scoped (released by commit, rollback, or the OS
    killing the process, so no stale lock can wedge a later boot) and database-enforced rather than by
    convention; a `flock`/`O_EXCL` sentinel degrades to a no-op on some NFS clients and leaves a file
    nobody can distinguish from a slow live peer. SQLite DDL is transactional, so unlike D-76 — whose
    non-atomic Postgres `IF NOT EXISTS` forced a separate lock object — the transaction is the whole
    mechanism.
  - Second race fixed with it: `PRAGMA journal_mode = WAL` is the one statement `busy_timeout`'s
    handler does not cover (SQLite returns `SQLITE_BUSY` immediately). `set_wal_mode` retries until the
    pragma reports `wal` and never silently settles for a lesser journal mode, which would forfeit
    C-25's cross-process writer coordination and C-126's checkpoint hygiene. The busy handler is now
    installed before the first contendable statement rather than after the pragma.
  - No existing test was modified — `cold_boot_migration.rs` is purely additive, so the third
    Acceptance item holds by construction.
  - Coordinator applied `cargo fmt` to the two files (the implementor died before it could), and
    re-ran `clippy` after an unrelated ENOSPC killed it mid-gate; it is clean at exit 0.
- 2026-07-29 — found by accident while implementing A-107. The implementor's first draft of the
  multi-process memory test had no orchestrator-side bootstrap, so four processes cold-booted the
  same brand-new `events.db` at once and it failed. It then mirrored C-125's shape (bootstrap, drop,
  spawn) as instructed and left a comment explaining why the bootstrap must be a separate step —
  correctly not fixing an unrelated pre-existing bug inside its own story.

## Notes
- **This is a genuine pre-existing bug, not a test artefact.** Two `flux` processes starting
  concurrently against a fresh store — an `app run --serve` alongside a CLI invocation, a fleet of
  sub-agents on a new machine, CI matrix jobs sharing a `HOME` — can have one die on bootstrap.
- **Why no existing test covers it**: every multi-process fixture in the tree (C-125's included)
  creates the database in the orchestrator before spawning children, so the children always find a
  migrated schema. The gap is structural, not accidental — a bootstrap step is the natural way to
  write such a test.
- D-76 is the precedent to read first: it fixed exactly this class for Postgres using a `flux:ddl`
  advisory lock. The interesting part of this story is that SQLite offers no equivalent primitive,
  so the mechanism has to be chosen rather than copied.
- Sibling of A-107 by discovery only; it is **not** part of the evidence-pinned-memory epic and
  should not wait on it.

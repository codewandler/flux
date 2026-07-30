---
id: C-230
title: "Two flux processes cold-booting one fresh events.db race the schema migration"
pillar: Core
status: in-progress
priority: 20
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
- [ ] Concurrent first-boot is serialised, so N processes opening a non-existent `events.db`
      simultaneously all succeed with one consistent schema. **Failing-first test**: spawn several
      processes against a fresh store path with **no orchestrator-side bootstrap**, and assert every
      one opens cleanly — it fails today on `duplicate column name`.
- [ ] The fix is the SQLite analogue of D-76's Postgres `flux:ddl` advisory lock, or a stated reason
      why a different mechanism is correct here (SQLite has no advisory-lock primitive, so this is a
      real design choice — a lock file, `BEGIN IMMEDIATE` around the migration, or serialising on the
      existing store mutex are all candidates with different failure modes under NFS/containers).
- [ ] The existing multi-process tests keep passing **unchanged**. They bootstrap the DB before
      spawning, which is why they never reproduced this; do not weaken them to accommodate the fix.
- [ ] Standard gate green in both workspaces.

## Progress
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

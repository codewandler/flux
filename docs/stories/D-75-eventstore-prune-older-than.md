---
id: D-75
title: EventStore::prune_older_than — a whole-store retention primitive
pillar: Core
status: done
epic: pg-backend
design: docs/designs/pg-backend.md
note: "tiny (may fold into D-73): prune_inactive is tag-scoped so no single call can implement whole-store retention; server deployments need one — the log otherwise grows without bound"
---

# EventStore::prune_older_than

## Goal
`EventStore::prune_older_than(cutoff_ms) -> Result<usize>`: delete every stream (registry
row + events) whose `updated_at` is older than the cutoff, on both backends. The existing
`prune_inactive(tag, cutoff)` only matches streams whose `agent_id` equals one tag — fine for
a single-agent workspace, structurally unable to express "retain N days" for a server
deployment whose streams carry many tags. Without a whole-store primitive the log grows
without bound and nothing can be scheduled against it.

## Acceptance
- [ ] Same delete shape as `prune_inactive` minus the tag predicate; returns the pruned
      stream count; both backends (SQLite loop / Postgres `WHERE stream = ANY($1)` batch).
- [ ] Rustdoc states the intended use (scheduled retention in long-running deployments) and
      the contrast with `prune_inactive`'s tag scoping — including why the interactive-CLI
      "protect the current session" concern doesn't apply to a caller that owns its store.
- [ ] Unit tests on both backends: streams straddling the cutoff — old pruned (registry row
      AND events gone), fresh untouched; empty-store call returns 0.

## Progress
- (not started — needs the D-72 seam; may ship inside D-73)

## Notes
- Design: [pg-backend.md](../designs/pg-backend.md) — the retention paragraph in §3.

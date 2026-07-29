---
id: A-107
title: "The memory stream — MemoryEntry projection over an append-only memory:<scope> stream"
pillar: Agent
status: backlog
epic: evidence-pinned-memory
design: docs/designs/evidence-pinned-memory.md
note: "cross-session memory needs its own stream in the SAME events.db, not a side table — inherits multi-process safety (C-25/C-125), WAL hygiene (C-126), the PG backend and flush-seam redaction for free"
---

# The memory stream — MemoryEntry projection over an append-only memory:<scope> stream

## Goal
Give memory a durable home that follows the event-store canon: its own stream in the existing
`events.db`, append-only, with a projection as the read model. Edits and forgets are appends, so the
history of what the agent *believed* survives — which is what you want when debugging a bad decision
six sessions later.

## Acceptance
- [ ] `MemoryEntry { id, claim, scope, receipt, git, learned_at_ms }` with
      `Receipt { stream, event_id, turn_id }` and `GitPin { sha, paths }`.
- [ ] `receipt.event_id` cites the **stable event id** (ULID), not `global_seq` — `global_seq` is a
      backend rowid that would not survive a store migration or mean the same thing across the
      SQLite and Postgres backends. Pinned by a test asserting the citation resolves after a
      round-trip.
- [ ] Entries live on a `memory:<scope-key>` stream in the same store; project to latest-state-per-id.
- [ ] An edit appends rather than mutates; a forget appends a tombstone; the projection reflects
      both. **Failing-first test**: a forgotten entry disappears from the projection while its
      history remains in the log.
- [ ] Redaction: a claim containing a credential shape is scrubbed by the same `Redactor` used at
      the evidence flush seam before it reaches the store.
- [ ] Multi-process safety is inherited, not re-implemented — a test appends from two processes to
      one memory stream and asserts gapless `stream_seq` (mirrors C-125's shape).
- [ ] No op and no CLI in this story; the store layer only.

## Progress
- Not started.

## Notes
- Design: [evidence-pinned-memory.md](../designs/evidence-pinned-memory.md).
- Prefer `EventKind::Custom` (the sanctioned open extension point for app facts) over a new closed
  variant if the shape allows — that keeps this story non-breaking, unlike A-104. Decide explicitly
  and record which was chosen and why.

---
id: A-107
title: "The memory stream — MemoryEntry projection over an append-only memory:<scope> stream"
pillar: Agent
status: done
priority: 15
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
- [x] `MemoryEntry { id, claim, scope, receipt, git, learned_at_ms }` with
      `Receipt { stream, event_id, turn_id }` and `GitPin { sha, paths }`.
      → `crates/flux-events/src/memory.rs` (`MemoryEntry`, `Receipt`, `GitPin`, `MemoryScope`);
      round-trip pinned by `memory::tests::entry_round_trips_and_omits_an_absent_git_pin`.
- [x] `receipt.event_id` cites the **stable event id** (ULID), not `global_seq` — `global_seq` is a
      backend rowid that would not survive a store migration or mean the same thing across the
      SQLite and Postgres backends. Pinned by a test asserting the citation resolves after a
      round-trip.
      → `a_receipt_cites_the_stable_event_id_not_the_backend_rowid` (both backends) and
      `a_memory_citation_survives_a_migration_that_renumbers_global_seq`, which re-imports into a
      rowid-offset store and asserts the source's `global_seq` no longer names the cited event.
- [x] Entries live on a `memory:<scope-key>` stream in the same store; project to latest-state-per-id.
      → `MemoryScope::stream`, `EventStore::memories`, `projection::memory_entries`; pinned by
      `memory_entries_live_on_a_scoped_stream_in_the_same_store`.
- [x] An edit appends rather than mutates; a forget appends a tombstone; the projection reflects
      both. **Failing-first test**: a forgotten entry disappears from the projection while its
      history remains in the log.
      → `EventStore::amend_memory` / `forget_memory` / `memory_history`; pinned by
      `forgetting_a_memory_hides_it_from_the_projection_but_keeps_its_history`.
- [x] Redaction: a claim containing a credential shape is scrubbed by the same `Redactor` used at
      the evidence flush seam before it reaches the store.
      → `MemoryNote::new` is the only constructor and takes the live redactor's `redact`; pinned by
      `a_credential_shaped_claim_is_scrubbed_before_it_reaches_the_store` (shape heuristic AND
      registered value, checked on the returned entry, the projection, and the raw stored payload).
- [x] Multi-process safety is inherited, not re-implemented — a test appends from two processes to
      one memory stream and asserts gapless `stream_seq` (mirrors C-125's shape).
      → `multi_process_memory_writers_produce_a_gapless_memory_stream` (4 real OS processes ×25
      `remember` calls on one `memory:global` stream), in `tests/multiprocess_stress.rs` alongside
      C-125's own tests and built from the same worker harness.
- [x] No op and no CLI in this story; the store layer only.
      → the diff touches only `crates/flux-events/`; no tool, no `flux-cli` route.

## Progress
- 2026-07-29 — **implemented** on `impl/A-107`, base `87137cf7`. New module
  `crates/flux-events/src/memory.rs`; projection `memory_entries`; store surface `remember` /
  `amend_memory` / `forget_memory` / `memories` / `memory_history` / `resolve_receipt`.
- **Encoding decision (the story's Notes):** `EventKind::Custom` under the reserved `memory.`
  name prefix (`memory.noted` / `memory.edited` / `memory.forgotten`), **not** new closed variants.
  `EventKind` is public and deliberately not `#[non_exhaustive]`, so three new variants would break
  every downstream `match` — for a fact none of flux's closed projections need to understand. The
  cost is that the payload shape is not compile-checked, so `memory_entries` skips an undecodable
  `memory.*` event instead of failing the read (`decode_all`'s discipline). `EventKind::Custom`'s
  doc now reserves the prefix.
- **Redaction seam:** `MemoryNote`'s claim field is private with exactly one constructor,
  `MemoryNote::new(raw, receipt, git, redact)`, so there is no path that reaches the store with raw
  model text. `flux-events` deliberately does **not** take a `flux-secret` dependency — redaction is
  a caller responsibility throughout this crate (C-22/C-164), and adding one would have meant a
  manifest change. A-108 must pass `|s| ctx.redactor.redact(s)`, the same redactor
  `flux-flow::flush_observations` uses.
- **Left for A-108/A-109/A-110:** nothing on the storage contract. Note that
  `EventStore::prune_adhoc_older_than` (D-77) would delete an aged `memory:*` stream — it has no
  caller today, but a retention job must exclude `MemoryScope::STREAM_PREFIX`.
- Gate green in this worktree: `build --workspace`, `test --workspace`,
  `clippy --workspace --all-targets -D warnings`, `fmt --all --check`, `test -p flux-codegate`.

## Notes
- Design: [evidence-pinned-memory.md](../designs/evidence-pinned-memory.md).
- Prefer `EventKind::Custom` (the sanctioned open extension point for app facts) over a new closed
  variant if the shape allows — that keeps this story non-breaking, unlike A-104. Decide explicitly
  and record which was chosen and why. → **`Custom` was chosen**; rationale in Progress above and in
  `crates/flux-events/src/memory.rs`'s module doc.

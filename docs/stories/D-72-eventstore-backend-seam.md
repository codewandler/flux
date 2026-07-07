---
id: D-72
title: EventStore internal backend seam — split the SQL primitives from the shared projections (pure refactor)
pillar: Core
status: done
epic: pg-backend
design: docs/designs/pg-backend.md
note: "pure refactor, public API byte-identical — the seam D-73 plugs into; unblocked 2026-07-07 (the request-auth/A2A store.rs changes landed in 0.4.0, working tree clean); independent of D-71, parallelizable"
---

# EventStore internal backend seam

## Goal
Restructure `flux-events` so `EventStore` holds an internal `enum Backend` — the ~20 SQL
primitives delegate per-backend while every projection, wrapper, and serde path stays shared —
with the public API byte-identical. This is the seam D-73 plugs a Postgres backend into; it
must land as its own reviewable commit with zero behavior change.

## Acceptance
- [ ] `store.rs` → `store/mod.rs` + `store/sqlite.rs`: `struct EventStore { backend: Backend }`,
      `enum Backend { Sqlite(SqliteEvents) }` (the Postgres arm arrives in D-73). Today's
      rusqlite code moves verbatim into `SqliteEvents`.
- [ ] The primitive set delegates per-backend (init/migrate, `create_session_with_context`,
      `latest_session`, `info`, `list`, `list_for_account`, `account_streams`,
      `find_correlated`, `find_correlated_in_realm`, `children_of`, `prune_empty`,
      `prune_inactive`, `append`, `load_stream`, `load_by_kind`, `conversation_delta`,
      `load_turn`, `head_seq`, `load_by_id`, `all_streams`, + new `streams_with_correlation()`
      splitting the SQL half out of `aggregate_streams`); backends return decoded
      `RawEvent`/`SessionSummary` values — all serde stays in `mod.rs`.
- [ ] Public API byte-identical: no consumer crate (`flux-flow`, `flux-eval`, `flux-cli`,
      `flux-server`, `flux-app`, `flux-sdk`) needs a single line changed; all existing
      flux-events tests pass unchanged; `cargo test --workspace` green.
- [ ] Lands as one commit (no feature flag, no new deps) so the D-73 diff reviews cleanly.

## Progress
- (not started)

## Notes
- Design: [pg-backend.md](../designs/pg-backend.md) §2.
- Was briefly gated on working-tree state: the request-auth/A2A track's additive store.rs
  changes (incl. `find_correlated_in_realm`) landed in 0.4.0 on 2026-07-07, so the file is
  clean to refactor; that method is included in the primitive set above.

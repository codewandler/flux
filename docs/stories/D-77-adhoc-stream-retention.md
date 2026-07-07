---
id: D-77
title: Retention for ad-hoc (unregistered) event streams — prune_older_than cannot reach them
pillar: Core
status: done
design:
note: "the D-55 Custom-facts pattern writes ad-hoc streams (no `streams` registry row); every prune primitive enumerates ONLY the registry, so ad-hoc streams grow forever — a deployment's retention policy silently excludes exactly the app-fact data it most needs to bound"
---

# Retention for ad-hoc (unregistered) event streams

## Goal
Make retention reach **ad-hoc streams**. `append` accepts any stream id; ids that don't parse as
`s_<n>` (the D-55 `EventKind::Custom` app-fact pattern — e.g. per-tenant fact logs) get events but
no `streams` registry row. All three prune primitives (`prune_empty`, `prune_inactive`,
`prune_older_than`) enumerate `SELECT n FROM streams …` and delete reconstructed `s_<n>` streams —
so ad-hoc streams are structurally unreachable by retention: they grow without bound, and a
deployment that schedules `prune_older_than` believes its whole store honors the horizon when the
ad-hoc portion silently doesn't.

## Acceptance
- [ ] A retention path covers ad-hoc streams. Either (choose in design):
      (a) `prune_adhoc_older_than(cutoff_ms) -> Result<usize>` — delete events whose stream has no
      registry row and whose newest event `ts` predates the cutoff (per-stream, not per-event, so
      a still-active ad-hoc stream keeps its full history); or
      (b) register ad-hoc streams in the registry on first append (nullable `n`? separate key?) so
      the existing primitives just work — weigh the migration/compat cost.
- [ ] `prune_older_than`'s rustdoc states explicitly which streams it covers (registry-only today —
      the current doc doesn't say so).
- [ ] Failing-first test on both backends: an aged ad-hoc stream (`audit-shaped`, non-`s_<n>` id)
      is pruned while a fresh one and a registered one inside the horizon survive.

## Progress
- 2026-07-08 — done, via option (a): new `prune_adhoc_older_than(cutoff_ms) -> Result<usize>` on
  the `EventBackend` trait + both backends + the public `EventStore` wrapper (threaded exactly like
  D-75's `prune_older_than`). Option (b) rejected: registering ad-hoc streams would force arbitrary
  string ids into the integer-keyed `s_<n>` session registry (nullable-`n` migration on both
  backends) and would surface non-session fact logs in session listings — a real migration for zero
  gain. Semantics as specified: a stream qualifies iff it has no registry row (`stream NOT IN
  (SELECT 's_' || n FROM streams)`) AND its NEWEST event predates the cutoff (`HAVING MAX(ts) <
  cutoff` — per-stream, so a still-active ad-hoc stream keeps its full history). Returns
  streams-removed count. SQLite = `begin_write` tx + per-stream deletes; PG = one tx + `DELETE …
  WHERE stream = ANY($1)` by TEXT id. `prune_older_than`'s rustdoc (trait + wrapper) now states it
  covers ONLY registry-listed `s_<n>` sessions and points at the new primitive (acceptance item).
  Conformance test `prune_adhoc_older_than_reaches_only_aged_unregistered_streams` runs on BOTH
  backends: aged ad-hoc deleted; fresh ad-hoc keeps full history incl. a pre-cutoff event
  (per-stream proof); aged AND fresh registered sessions survive; re-sweep returns 0. Package gates
  green (default DB-free + `--features postgres` vs live PG).

## Notes
- Found in a post-ship review: a consumer scheduling `prune_empty` + `prune_older_than` as its
  whole-store retention documented ad-hoc facts as covered — structurally impossible today.
- Related: [D-55](D-55-eventkind-custom.md) (the ad-hoc Custom-facts pattern),
  [D-75](D-75-eventstore-prune-older-than.md) (the registry-scoped horizon primitive).

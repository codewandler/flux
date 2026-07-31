---
id: C-231
title: "Ad-hoc stream pruning would silently delete cross-session memory"
pillar: Core
status: in-progress
priority: 21
epic: evidence-pinned-memory
design: docs/designs/evidence-pinned-memory.md
note: "memory:* streams are ad-hoc (no `streams` registry row), which is exactly what prune_adhoc_older_than targets; D-77's prune has NO caller today, so nothing is at risk yet — the whole value is landing the guard before the first retention job exists"
---

# Ad-hoc stream pruning would silently delete cross-session memory

## Goal
`EventStore::prune_adhoc_older_than` (D-77) deletes aged streams that carry no `streams` registry
row. A-107 put memory on `memory:<scope-key>` streams, which are exactly that shape — so the prune
would take them.

Nothing is at risk **today**: that function has no caller anywhere in the tree. That is the entire
reason to do this now. The failure mode is silent and unrecoverable — cross-session memory simply
stops existing, with no error and no signal — and it would arrive attached to a future retention job
whose author has no reason to think about memory at all.

## Acceptance
- [x] `prune_adhoc_older_than` excludes any stream under `MemoryScope::STREAM_PREFIX`.
      **Failing-first test**: seed a memory stream with an old timestamp, run the prune, assert the
      entries survive — it fails today because they are deleted.
      → `prune_adhoc_older_than_never_deletes_cross_session_memory`
      (`crates/flux-events/src/store/mod.rs:2933`), a conformance body registered on all three
      backends. At the merge base it failed `left: 3 / right: 1` — the sweep took both memory
      streams along with the ordinary ad-hoc one.
- [x] The exclusion is expressed so a *future* ad-hoc stream family has to make the same decision
      consciously rather than inheriting deletion by default — e.g. an explicit retained-prefix list
      with a comment per entry, not a bare `!starts_with("memory:")` buried in a query.
      → `crates/flux-events/src/retention.rs`: `ADHOC_STREAM_FAMILIES`, one row per family carrying
      a `Prunable`/`Retained` verdict (no `Default`) plus a required `why`, and
      `every_stream_prefix_declared_in_this_crate_has_a_retention_row` (`retention.rs:156`) reads
      this crate's own source so a new `STREAM_PREFIX` without a row reds the gate instead of
      inheriting deletion. All three backends filter through the one
      `is_retained_from_adhoc_prune`.
- [x] Whether memory should *ever* be prunable is answered in writing, not left implicit. Unbounded
      growth is a real cost, and "never delete" is a choice with consequences; if a retention policy
      is wanted later it should be scope-aware and explicit (A-110's `forget` already exists as the
      deliberate path). State the position in the design doc.
      → `docs/designs/evidence-pinned-memory.md` §7 "Retention: memory is not a timer's business":
      the position, the three reasons, the acknowledged growth cost, and the four terms a future
      memory retention policy must meet.
- [x] Standard gate green in both workspaces.
      → Root workspace green (build / test / clippy `-D warnings` / fmt / `-p flux-codegate`).
      `plugins/` untouched, so its nested workspace was not rebuilt.

## Progress
- 2026-07-29 — surfaced by the A-107 implementor as a finding it deliberately did not act on, which
  was the right call: fixing it inside A-107 would have widened a store-layer story into retention
  policy.
- 2026-07-31 — done. Shape notes for whoever touches this next:
  - The guard lives in **Rust, not SQL**. Each backend's `prune_adhoc_older_than` keeps its existing
    "unregistered and aged" query and then filters the candidate list through
    `is_retained_from_adhoc_prune`. Chosen over per-prefix `NOT LIKE` clauses because three SQL
    strings drift and a LIKE pattern needs metacharacter escaping, whereas one classifier over one
    table cannot disagree with itself. The returned count is taken *after* the filter, so it still
    means "streams actually removed".
  - The forcing test scans source rather than a fixture (a fixture would only agree with the table it
    was written beside — the recurring self-confirming-guard trap). It was verified to bite: with the
    `memory:` row swapped out, it fails naming the file, the prefix and the decision to make. Its
    honest limit, stated in its own doc comment: it sees in-tree families that declare a
    `STREAM_PREFIX` constant in `flux-events`; a prefix an embedder invents in its own crate is
    outside flux's reach and stays `Prunable` by omission, which is what D-77 is for.
  - Still no caller for `prune_adhoc_older_than` — deliberately unchanged. This story was the guard
    and the written decision, not a retention policy.

## Notes
- Seams: `EventStore::prune_adhoc_older_than` (D-77) and `MemoryScope::STREAM_PREFIX` (A-107,
  `crates/flux-events/src/memory.rs`).
- **The window is the point.** Landing the guard while the prune has no callers costs one test.
  Landing it after a retention job ships costs a data-loss incident that nobody can reconstruct,
  because the evidence was the thing deleted.
- Cheap to implement, and its value is entirely in the test — the guard is what stops a future
  author reintroducing the deletion while refactoring the prune.

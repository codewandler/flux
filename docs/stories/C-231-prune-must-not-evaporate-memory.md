---
id: C-231
title: "Ad-hoc stream pruning would silently delete cross-session memory"
pillar: Core
status: ready
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
- [ ] `prune_adhoc_older_than` excludes any stream under `MemoryScope::STREAM_PREFIX`.
      **Failing-first test**: seed a memory stream with an old timestamp, run the prune, assert the
      entries survive — it fails today because they are deleted.
- [ ] The exclusion is expressed so a *future* ad-hoc stream family has to make the same decision
      consciously rather than inheriting deletion by default — e.g. an explicit retained-prefix list
      with a comment per entry, not a bare `!starts_with("memory:")` buried in a query.
- [ ] Whether memory should *ever* be prunable is answered in writing, not left implicit. Unbounded
      growth is a real cost, and "never delete" is a choice with consequences; if a retention policy
      is wanted later it should be scope-aware and explicit (A-110's `forget` already exists as the
      deliberate path). State the position in the design doc.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — surfaced by the A-107 implementor as a finding it deliberately did not act on, which
  was the right call: fixing it inside A-107 would have widened a store-layer story into retention
  policy.

## Notes
- Seams: `EventStore::prune_adhoc_older_than` (D-77) and `MemoryScope::STREAM_PREFIX` (A-107,
  `crates/flux-events/src/memory.rs`).
- **The window is the point.** Landing the guard while the prune has no callers costs one test.
  Landing it after a retention job ships costs a data-loss incident that nobody can reconstruct,
  because the evidence was the thing deleted.
- Cheap to implement, and its value is entirely in the test — the guard is what stops a future
  author reintroducing the deletion while refactoring the prune.

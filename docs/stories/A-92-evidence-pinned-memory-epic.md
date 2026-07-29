---
id: A-92
title: "Evidence-pinned memory — cross-session memory with provenance (epic)"
pillar: Agent
status: in-progress
epic: evidence-pinned-memory
design: docs/designs/evidence-pinned-memory.md
note: "EPIC — every memory entry cites the event-store receipt + git SHA it was learned from and goes stale-visible when the cited evidence changes"
---

# Evidence-pinned memory — cross-session memory with provenance (epic)

## Goal
Cross-session memory is table stakes elsewhere (Cursor, Claude Code — per the research archive)
but flux specs have none. The flux-native version: every memory entry must cite the event-store
receipt and git SHA it was learned from, and goes stale-visible when the cited evidence changes —
memory with provenance instead of a vibes scratchpad, mirroring how C-14's evidence trail works
for turns.

## Acceptance
- [ ] A design doc (`docs/designs/evidence-pinned-memory.md`) covering: the memory-entry schema
      (claim + event-store receipt + git SHA), staleness detection when cited evidence changes,
      injection into turns, and the user-facing inspect/prune surface.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: a memory entry learned in one session is available in a later session with
      its citation intact, and is rendered stale-visible after the cited evidence changes.

## Progress
- 2026-07-29 — **design done**: [evidence-pinned-memory.md](../designs/evidence-pinned-memory.md).
  Confirmed by code read that flux has no memory of any kind today (the `Memory*` hits in
  `flux-capabilities` are the in-memory vector store, unrelated).
- The design's load-bearing invariant, which everything else serves: **the model supplies the claim,
  the host supplies the citation.** `memory_note` takes only `(claim, scope)` — there is no
  parameter through which a receipt or SHA can be supplied, so provenance cannot be forged. Same
  property that makes `ActionBatch` trustworthy (`staged.rs:203`).
- Three other decisions settled:
  - Storage is a `memory:<scope>` **stream in the existing `events.db`**, not a side table —
    inherits C-25/C-125 multi-process safety, C-126 WAL hygiene, the PG backend and flush-seam
    redaction rather than re-earning them.
  - Injection reuses the `<knowledge-base>` `ContextBlock` seam, already hardened for breakout
    (A-21) and budget-bounded with a visible marker (A-24). A second injection path would have to
    re-earn both.
  - **Stale entries are still injected, marked** `stale="true"` with the reason. Stale ≠ false; it
    is *unverified*. Dropping silently loses real knowledge, and a bare scratchpad asserts false
    confidence — marking is the only option that avoids both.
- Decomposed into A-107 (stream + projection) → A-108 (`memory_note`, the no-forgery seam) →
  A-109 (injection + staleness) → A-110 (`flux memory` CLI). A-109 needs only A-107 to be testable.
- Next: A-107, sequenced after the A-93 chain.

## Notes
- Mirrors C-14 (durable evidence trail) — provenance-first, unlike competitor scratchpad memory
  (see docs/archive/research/landscape.md).

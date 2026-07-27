---
id: A-92
title: "Evidence-pinned memory — cross-session memory with provenance (epic)"
pillar: Agent
status: backlog
epic: evidence-pinned-memory
design:
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
- (not started — epic filed from the 2026-07-28 out-of-the-box ideas session)

## Notes
- Mirrors C-14 (durable evidence trail) — provenance-first, unlike competitor scratchpad memory
  (see docs/archive/research/landscape.md).

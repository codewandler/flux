---
id: A-07
title: Bound the symbols block — cap the one context segment that grows without eviction
pillar: Agent
status: ready
priority: 4
note: segment C (per-turn symbols digest) is uncached AND unbounded — FlowStore::view has no count/byte cap, one line per session symbol accumulates forever, and conversation compaction never touches it
---

# Bound the symbols block (segment C)

## Goal
Cap the only per-call context vector with no bounding mechanism. Verified 2026-07-02:
`symbols_block` (`crates/flux-flow/src/compile.rs:684-702`) renders one line per visible/pinned
session symbol from `FlowStore::view` (`crates/flux-flow/src/state.rs:449-481` — no LIMIT, no
count cap, newest-first; only each summary is capped at 80 chars). A long session accumulates an
ever-larger **uncached** trailing segment on every planner call; the 48k conversation compaction
(`engine.rs:673-755`) and the L-08 ctx packer are separate mechanisms that never evict symbols.
This is also what makes README:16's "tool outputs are stored as symbols, not re-sent on every
turn" overstated — the symbol digest IS re-sent and grows.

## Acceptance
- [ ] **Failing-first:** `symbols_block_caps_lines_and_keeps_pinned_newest_first` (flux-flow) —
      100 visible + 5 older pinned symbols → all pinned kept, newest visible kept, marker reads
      "… 41 older symbols omitted" (fails today: all 105 lines render).
- [ ] The cap lives in the renderer `symbols_block` ONLY — `FlowStore::view` stays uncapped
      (verified consumers that must see everything: `ValueStore::binding()` symbol resolution,
      flux-lang store.rs:75-86; L-08 ctx budget ops; REPL/session display surfaces).
- [ ] Defaults: 64-line cap + 10k-char backstop; eviction priority Pinned > Visible, newest-first
      within tier (view is already `ORDER BY updated_at DESC`); drop-and-continue on the char
      backstop (L-08 precedent); omission marker names the count and notes symbols remain
      referencable by `$name`.
- [ ] `FLUX_SYMBOLS_CAP` env override; `0` disables — `symbols_block_cap_zero_disables` test. Pure
      `symbols_block_with_caps(view, lines, chars)` + thin env wrapper (race-free tests).
- [ ] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P4 of the round).

## Notes
- Segment C is already `cache: false` (compile.rs:767-773) — capping is a pure per-call shrink, no
  cache-invalidation interactions.
- C-16 (docs truth pass) updates the README wording once this lands.

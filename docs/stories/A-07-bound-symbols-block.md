---
id: A-07
title: Bound the symbols block — cap the one context segment that grows without eviction
pillar: Agent
status: done
priority:
note: the symbols digest is now bounded in the renderer (64 lines / 10k chars, Pinned > Visible newest-first, drop-and-continue, omission marker, FLUX_SYMBOLS_CAP override with 0=off) — FlowStore::view stays uncapped for resolution/budgeting
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
- [x] **Failing-first:** `symbols_block_caps_lines_and_keeps_pinned_newest_first` (flux-flow) —
      pinned + visible symbols beyond the cap → all pinned kept (even the oldest), newest visible
      kept, oldest visible dropped, marker counts omissions (fails today: everything renders).
- [x] The cap lives in the renderer `symbols_block` ONLY — `FlowStore::view` stays uncapped
      (verified consumers that must see everything: `ValueStore::binding()` symbol resolution,
      flux-lang store.rs:75-86; L-08 ctx budget ops; REPL/session display surfaces).
- [x] Defaults: 64-line cap + 10k-char backstop; eviction priority Pinned > Visible, newest-first
      within tier (view is already `ORDER BY updated_at DESC`); drop-and-continue on the char
      backstop (L-08 precedent); omission marker names the count and notes symbols remain
      referencable by `$name`.
- [x] `FLUX_SYMBOLS_CAP` env override; `0` disables — `symbols_block_cap_zero_disables` test. Pure
      `symbols_block_bounded(view, cap)` + thin env wrapper (race-free tests, no env mutation).
- [x] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P4 of the round).
- Done 2026-07-02. `symbols_block_bounded(view, cap)` renders Pinned-first then Visible (store
  order — newest-updated-first — preserved within each tier), capped at 64 lines with a 10k-char
  backstop; an oversized line is dropped-and-continued so it can't evict later symbols; the
  trailing marker reads "… N older symbol(s) omitted (still referencable as $name)". Env wrapper
  reads `FLUX_SYMBOLS_CAP` (0 = unbounded). Both the loop planner (segment C) and the one-shot
  compile's symbols segment go through the same renderer. 3 tests (cap+pinned ranking, cap-zero,
  char-backstop drop-and-continue).

## Notes
- Segment C is already `cache: false` (compile.rs:767-773) — capping is a pure per-call shrink, no
  cache-invalidation interactions.
- C-16 (docs truth pass) updates the README wording once this lands.

---
id: A-93
title: "Typed session log — session-shape validity by construction (epic)"
pillar: Agent
status: in-progress
epic: typed-session-log
design: docs/designs/typed-session-log.md
note: "EPIC — make the invalid provider-history shapes (split tool_use/tool_result, empty assistant, user-after-user) unrepresentable in the session log's type; the thrice-recurred bug class becomes unwritable instead of test-guarded"
---

# Typed session log — session-shape validity by construction (epic)

## Goal
The "session shape is always a valid provider history" safety invariant has broken three times
(cancel, compaction, the iteration cap) — each time on a newly added turn-termination path. Today
it holds by discipline, not by construction: termination paths funnel through one `finish_turn`
(`crates/flux-flow/src/engine.rs`) and compaction snaps its boundary so a `tool_result` is never
orphaned, but nothing stops a fourth termination path from bypassing the funnel. Make the session
log a typed state machine whose API cannot express an empty assistant message, a split
tool_use/tool_result pair, or a user-after-user sequence — the bug class becomes unwritable, and
the pre-release live-provider gate stops being the only net that catches it.

## Acceptance
- [ ] A design doc (`docs/designs/typed-session-log.md`) covering: the typed log states and legal
      transitions, how every turn-termination path (stop, cancel, compaction, iteration cap, and
      any future path) appends through the one typed API, the migration of existing history
      handling in `flux-flow`, and the provider-wire seam where the typed log projects to each
      codec's message shape.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: the three historical invalid shapes are unrepresentable (rejected at compile
      time or by the log's only constructors), pinned by a hermetic shape gate that would have
      caught all three past regressions without a live provider 400.

## Progress
- 2026-07-29 — **design done**: [typed-session-log.md](../designs/typed-session-log.md). Grounded in
  the current code; two findings sharpened the plan:
  - The write seam is `EventStore::record_message` (`store/mod.rs:766`), which appends any `Message`
    unexamined. The turn's two writes are 750 lines apart (`engine.rs:422` user,
    `engine.rs:1177` assistant) with no type pairing them.
  - **The predicted fourth termination path already exists**: `resurrect.rs:425-438` closes a turn
    outside `finish_turn`, with a comment stating it "Mirrors `finish_turn_lifecycle`'s ordering".
    It is correct only because someone copied the ordering by hand — and it writes
    `assistant_text(answer)` with no non-empty check, so an empty resurrect answer would write
    invalid shape #1 today.
- Decomposed into A-99 (shape types) → A-100 (typed handle) → A-101 (flux-flow migration + delete
  the unguarded API, **breaking**) → A-102 (SDK/CLI rewriters). A-99 is `ready`, priority 1.
- 2026-07-29 — **A-99 DONE**: `crates/flux-events/src/shape.rs` ships `ShapeError`,
  `AssistantMessage`, `ValidHistory`, `ValidHistory::snap`; crate suite 77 → 98 green, fmt + clippy
  clean, no call sites moved yet.
- **A third finding, and this one is a live bug**: compaction can already write `user`-after-`user`
  today. The persisted log is a strict `user, assistant, …` alternation, so `split = len - keep`
  (keep = 2) always lands on a **`user`** message; `has_tool_result` is false for it so the
  walk-back never moves, and `[user_summary] + [user, assistant]` goes to the store. Reachable
  whenever `total > compact_threshold_chars` and `len >= 4`. `ValidHistory::snap` computes the right
  split; the fix lands with A-101, which now carries a failing-first test for it.
- 2026-07-29 — **A-100 DONE**: `crates/flux-events/src/session_log.rs` ships `SessionLog`, `Tail`,
  `LogError`; crate suite 98 → 112 green, full gate clean, no call sites moved yet. The build turned
  up one thing the design had not: re-deriving the tail on `open` is not enough, because
  derive-then-append is a check-then-act — the transitions now compare-and-append against the
  `stream_seq` the tail came from, inside the write transaction (new backend primitive
  `append_if_conversation_head`, SQLite + Postgres). Design doc updated.
- Next: A-101 (`ready`, priority 1) — the flux-flow migration, which also carries the
  compaction `user`-after-`user` fix below.

## Notes
- Downgraded from "design smell" to "hardening opportunity" during the code review: both current
  termination paths do funnel through `finish_turn`, and compaction explicitly protects the
  tool_use/tool_result boundary — the residual risk is the *next* termination path someone adds.
- The mock provider does not catch this class (see the safety invariants in AGENTS.md and the
  pre-release gate in docs/roadmap.md); validity-by-construction closes that structural gap.
- Smallest of the three re-assessment suggestions; purely internal, no user-visible behavior
  change when done right.

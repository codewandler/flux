---
id: A-101
title: "Migrate flux-flow onto the typed log and delete the unguarded write API"
pillar: Agent
status: backlog
epic: typed-session-log
design: docs/designs/typed-session-log.md
note: "BREAKING (published crate) — record_message/record_compaction are removed, not deprecated, per the clean-cutover rule; resurrect.rs:438's hand-mirrored finish_turn ordering becomes the enforced one"
---

# Migrate flux-flow onto the typed log and delete the unguarded write API

## Goal
Move every `flux-flow` conversation write onto the typed handle and then **remove** the unguarded
`EventStore::record_message` / `record_compaction`. Leaving them beside the handle would recreate
the bypass the epic exists to close — a future contributor reaches for the shorter name — so this is
a clean cutover, not a deprecation.

## Acceptance
- [ ] `engine.rs:420-423` (user message at turn start) goes through `open_turn`.
- [ ] `engine.rs:1177` (`finish_turn`) goes through `close_turn` with an `AssistantMessage`.
- [ ] Compaction (`compaction_attempt`) goes through `rewrite(ValidHistory)`, and the local
      `has_tool_result` helper + inline snapping loop (`engine.rs:1449-1455`, `:1629`) are deleted
      in favour of `ValidHistory::snap`.
- [ ] **This fixes a live bug** found while building A-99: the persisted log is a strict
      `user, assistant, …` alternation, so `split = len - keep` (keep = 2) always lands on a
      **`user`** message; `has_tool_result` is false for it, the walk-back never moves, and
      `[user_summary] + [user, assistant]` is written — `user`-after-`user`. Reachable whenever
      `total > compact_threshold_chars` and `len >= 4`. **Failing-first test**: drive a compaction
      over a ≥4-message alternation and assert the resulting history is `ValidHistory`-valid; it
      fails on today's code and passes once `snap` is wired in.
- [ ] `resurrect.rs:425-438` — the path that today hand-copies `finish_turn_lifecycle`'s ordering
      with a comment saying so — goes through `close_turn`. Its comment is removed because the
      ordering is no longer something a reader must replicate by hand.
- [ ] The resurrect path can no longer write an empty assistant message (it currently calls
      `Message::assistant_text(answer)` with no non-empty check) — pinned by a test.
- [ ] `EventStore::record_message` and `record_compaction` no longer exist. **breaking** — a
      published crate surface ⇒ next release is a MINOR per the pre-1.0 rule; note it in the
      CHANGELOG's breaking list.
- [ ] `cancellation_keeps_a_valid_user_assistant_session_shape` (`engine.rs:4239`) stays green
      **unmodified** — behaviour lock.
- [ ] Full gate green in both workspaces.

## Progress
- Not started.

## Notes
- Design: [typed-session-log.md](../designs/typed-session-log.md).
- Blocked by A-100. A-102 (SDK/CLI call sites) must land in the same release — deleting the API
  breaks those crates' builds otherwise, so sequence A-101 → A-102 or land them together.
- The engine holds one `FlowEngine` across turns but the handle is per-turn-scoped by design
  (`open` re-derives `Tail`); do not cache a `SessionLog` on the engine struct.

---
id: A-101
title: "Migrate flux-flow onto the typed log and delete the unguarded write API"
pillar: Agent
status: in-progress
priority: 1
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
- [x] `engine.rs:420-423` (user message at turn start) goes through `open_turn`.
- [x] `engine.rs:1177` (`finish_turn`) goes through `close_turn` with an `AssistantMessage`.
- [x] Compaction (`compaction_attempt`) goes through `rewrite(ValidHistory)`, and the local
      `has_tool_result` helper + inline snapping loop (`engine.rs:1449-1455`, `:1629`) are deleted
      in favour of `ValidHistory::snap`.
- [x] **This fixes a live bug** found while building A-99: the persisted log is a strict
      `user, assistant, …` alternation, so `split = len - keep` (keep = 2) always lands on a
      **`user`** message; `has_tool_result` is false for it, the walk-back never moves, and
      `[user_summary] + [user, assistant]` is written — `user`-after-`user`. Reachable whenever
      `total > compact_threshold_chars` and `len >= 4`. **Failing-first test**: drive a compaction
      over a ≥4-message alternation and assert the resulting history is `ValidHistory`-valid; it
      fails on today's code and passes once `snap` is wired in.
- [x] `resurrect.rs:425-438` — the path that today hand-copies `finish_turn_lifecycle`'s ordering
      with a comment saying so — goes through `close_turn`. Its comment is removed because the
      ordering is no longer something a reader must replicate by hand.
- [x] The resurrect path can no longer write an empty assistant message (it currently calls
      `Message::assistant_text(answer)` with no non-empty check) — pinned by a test.
- [ ] **DEFERRED TO A-102** — `EventStore::record_message` and `record_compaction` no longer exist. **breaking** — a
      published crate surface ⇒ next release is a MINOR per the pre-1.0 rule; note it in the
      CHANGELOG's breaking list.
- [x] `cancellation_keeps_a_valid_user_assistant_session_shape` (`engine.rs:4239`) stays green
      **unmodified** — behaviour lock.
- [x] Full gate green in both workspaces.

## Progress
- 2026-07-29 — started. A-100 landed (`37b2826`), so the handle is available.
- 2026-07-29 — **flux-flow is migrated and the gate is green** (workspace tests, clippy `-D warnings`,
  fmt in both workspaces, `flux-codegate`). Uncommitted.
- Failing-first, verified by reverting the fix: `compaction_never_writes_a_user_after_user_history`
  fails on the old inline walk-back with *"two User messages in a row at index 1 — broken
  alternation"*. The bug was live for every session past the threshold with ≥ 4 messages.
- **The deletion is the one open box** and moved to A-102 — see the note below.

## Notes
- **Why the deletion moved to A-102.** Removing `record_message`/`record_compaction` breaks every
  remaining caller in the same instant: `flux-sdk` fork/`whatif`, `flux-cli` fork/export, and the
  `flux-tui`/`flux-server`/`flux-events` test fixtures — all of them A-102's scope. There is no
  ordering in which A-101 deletes the API and the workspace still builds, so the deletion lands as
  A-102's last step. This story's own note already anticipated it ("must land in the same release").
  Nothing in flux-flow references the unguarded pair any more.
- **Two behaviours the migration forced, neither in the original plan.** Both are recorded in the
  design doc:
  1. **Flow-driven turns open with a synthetic user message.** `start_flow_turn` (SDK `start_flow`,
     the app runner, the voice driver) begins a turn with `user_input: None` and yet `finish_turn`
     persists an answer — so those sessions' logs *started on an `assistant` message*, which no
     Messages-contract provider accepts. It was a live latent bug of the same family, not a
     migration artifact. Timo's call (2026-07-29): synthesize the opener (`[<flow name>]`) rather
     than drop the answer, so the flow's answer stays visible to the next conversational turn.
  2. **An abandoned turn is closed at the next turn boundary.** A crash the embedder chose not to
     resurrect (`auto_resurrect` off) leaves the log owing an answer; the next `open_turn` would be
     `TurnAlreadyOpen` forever. `begin_turn_lifecycle` now closes it with `(turn interrupted)`
     first. Append-only — the crashed turn's telemetry and trace are untouched.
- A blank answer closes the turn as `(no answer)` rather than erroring: failing the write would
  leave the log ending on the `user` message, which is the shape the seam exists to prevent.
- Design: [typed-session-log.md](../designs/typed-session-log.md).
- Blocked by A-100. A-102 (SDK/CLI call sites) must land in the same release — deleting the API
  breaks those crates' builds otherwise, so sequence A-101 → A-102 or land them together.
- The engine holds one `FlowEngine` across turns but the handle is per-turn-scoped by design
  (`open` re-derives `Tail`); do not cache a `SessionLog` on the engine struct.

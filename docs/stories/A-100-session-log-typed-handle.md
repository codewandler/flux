---
id: A-100
title: "SessionLog typed handle — a turn-lifecycle state machine at the write seam"
pillar: Agent
status: done
epic: typed-session-log
design: docs/designs/typed-session-log.md
note: "open/open_turn/close_turn/rewrite over a Tail state derived from the store; makes user-after-user an illegal transition rather than a silent append"
---

# SessionLog typed handle — a turn-lifecycle state machine at the write seam

## Goal
Give the persisted conversation a typed handle whose API can only express legal transitions. The
turn's two writes — the user message at `begin_turn_lifecycle` and the assistant message at
`finish_turn` — are currently 750 lines apart in `flux-flow/engine.rs` with nothing pairing them;
this makes the pairing a type, so a termination path that skips the second write is a compile-time
or `Err` outcome instead of a log that ends on a `user` message.

## Acceptance
- [x] `SessionLog::open(store, stream)` derives its `Tail` (`Empty` | `AwaitingAssistant` |
      `Closed`) from the store on every open — never from a cached handle — so a crash or a
      concurrent writer cannot leave a handle claiming a turn is closed.
- [x] `open_turn` from `AwaitingAssistant` returns `Err(ShapeError::TurnAlreadyOpen)` and appends
      nothing. **Failing-first test**: the equivalent double-append through today's
      `record_message` silently produces `user`-after-`user`.
- [x] `close_turn` from `Empty`/`Closed` returns `Err`; from `AwaitingAssistant` it appends and
      moves to `Closed`.
- [x] `close_turn` takes an `AssistantMessage` (A-99), so an empty answer cannot reach the log.
- [x] `rewrite` takes a `ValidHistory` (A-99) and appends one `Compacted` event.
- [x] Tail derivation uses the existing kind-filtered `conversation_delta` read, not a full stream
      scan — asserted by a test that a large stream's `open` decodes only message-kind events.
- [x] Concurrency semantics are unchanged: appends keep `BEGIN IMMEDIATE` (C-25/C-125). A test
      proves two handles racing `open_turn` on one stream leave exactly one user message.
- [x] No call sites change in this story.

## Progress
- **Done** (2026-07-29). `crates/flux-events/src/session_log.rs` ships `SessionLog`, `Tail`, and
  `LogError`; 14 unit tests, full gate green in both workspaces.
- Failing-first: `double_open_turn_is_rejected_where_record_message_silently_breaks_shape` — its
  first half asserts what the raw seam does today (two `record_message` calls → `user`-after-`user`,
  a history `ValidHistory` rejects), its second half the typed refusal. The module did not exist
  when it was written, so it failed to compile.
- The race guard is load-bearing and was verified as such: neutering the compare-and-append makes
  `racing_open_turn_leaves_exactly_one_user_message` fail (two user messages), not merely flake.

## Notes
- Design: [typed-session-log.md](../designs/typed-session-log.md).
- Blocked by A-99 (needs `AssistantMessage` + `ValidHistory`).
- `Tail` is a cache of the store's truth, not a second source of it — this is the property the
  re-derive-on-open acceptance criterion pins.
- **Three deliberate deviations from the design sketch**, all recorded in the design doc:
  1. Transitions return `Result<(), LogError>`, not `Result<(), ShapeError>` — the write seam does
     IO, so the two failure kinds are separated (`LogError::shape()` matches the actionable one)
     rather than collapsed. `From<LogError> for flux_core::Error` keeps `?` working at call sites.
  2. `open_turn` takes a `Message` and rejects a non-`user` role (`ShapeError::NotAUserMessage`).
     The design's `UserMessage` newtype was never built — A-99 shipped only the two types this
     story is blocked on, and a third would be surface without a second caller.
  3. Re-deriving the tail before appending is not enough on its own: derive-then-append is not
     atomic, so the append became a **compare-and-append** on the newest message-affecting
     `stream_seq`, read inside the same write transaction (new `EventBackend::
     append_if_conversation_head`, implemented for both SQLite and Postgres). Without it the
     race test writes two user messages.
- `EventStore::append_if_conversation_head` is deliberately `pub(crate)`: a public raw
  compare-and-append would be a second unguarded way to write the conversation.

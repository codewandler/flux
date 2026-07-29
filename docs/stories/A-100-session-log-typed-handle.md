---
id: A-100
title: "SessionLog typed handle — a turn-lifecycle state machine at the write seam"
pillar: Agent
status: ready
priority: 1
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
- [ ] `SessionLog::open(store, stream)` derives its `Tail` (`Empty` | `AwaitingAssistant` |
      `Closed`) from the store on every open — never from a cached handle — so a crash or a
      concurrent writer cannot leave a handle claiming a turn is closed.
- [ ] `open_turn` from `AwaitingAssistant` returns `Err(ShapeError::TurnAlreadyOpen)` and appends
      nothing. **Failing-first test**: the equivalent double-append through today's
      `record_message` silently produces `user`-after-`user`.
- [ ] `close_turn` from `Empty`/`Closed` returns `Err`; from `AwaitingAssistant` it appends and
      moves to `Closed`.
- [ ] `close_turn` takes an `AssistantMessage` (A-99), so an empty answer cannot reach the log.
- [ ] `rewrite` takes a `ValidHistory` (A-99) and appends one `Compacted` event.
- [ ] Tail derivation uses the existing kind-filtered `conversation_delta` read, not a full stream
      scan — asserted by a test that a large stream's `open` decodes only message-kind events.
- [ ] Concurrency semantics are unchanged: appends keep `BEGIN IMMEDIATE` (C-25/C-125). A test
      proves two handles racing `open_turn` on one stream leave exactly one user message.
- [ ] No call sites change in this story.

## Progress
- Not started.

## Notes
- Design: [typed-session-log.md](../designs/typed-session-log.md).
- Blocked by A-99 (needs `AssistantMessage` + `ValidHistory`).
- `Tail` is a cache of the store's truth, not a second source of it — this is the property the
  re-derive-on-open acceptance criterion pins.

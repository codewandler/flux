---
id: A-104
title: "Materialize reverse actions — pre-image capture at the dispatch seam + EventKind::Compensated"
pillar: Agent
status: backlog
epic: transactional-turns
design: docs/designs/transactional-turns.md
note: "capture runs inside the guarded boundary immediately before execution (NOT at approval time — prior bytes aren't knowable then); BREAKING: EventKind is a closed set, a new variant ⇒ MINOR"
---

# Materialize reverse actions — pre-image capture at the dispatch seam + EventKind::Compensated

## Goal
Turn the static declaration (A-103) into a concrete, stored reverse `Action` for every mutating
dispatch. Capture happens at `Executor::dispatch_outcome` — the single funnel every op passes
through (hooks → policy → permissions → approval → guarded `System`) — immediately before execution,
so there is no window between reading the pre-image and overwriting it.

## Acceptance
- [ ] A `Snapshot` op's pre-image is captured inside the guarded boundary immediately before
      execution; the capture read is itself policy-checked and audited.
- [ ] Capture is part of the envelope, **not** a `PreToolHook` — pinned by a test that a
      user-registered hook cannot suppress or replace it.
- [ ] New `EventKind::Compensated { action_id, op, reverse: Option<Action>, why: Option<String> }`,
      scoped to the turn via `.in_turn(turn_id)`, plus its projection arm.
      **breaking** — `EventKind` is a deliberately closed set with no `#[non_exhaustive]`
      (`flux-events/src/kind.rs:32`), so this breaks exhaustive matches ⇒ next release is a MINOR;
      note it in the CHANGELOG's breaking list.
- [ ] A failed capture degrades that action to `reverse: None` with a `why` naming the failure and
      is recorded — **failing-first test**: it must never be silently dropped, and it must not fail
      the dispatch itself (an unrecoverable-undo op still runs; the user is told it is unrecoverable).
- [ ] `Inverse` ops materialize their reverse from the input alone, with no capture read.
- [ ] Read-only ops (`NotNeeded`) emit no event — asserted, so a read-heavy turn does not bloat the
      log.
- [ ] Redaction: the stored reverse action passes the same `Redactor` the evidence flush uses
      (`engine.rs:flush_observations`) before it reaches the store — a captured pre-image can
      contain a credential.

## Progress
- Not started.

## Notes
- Design: [transactional-turns.md](../designs/transactional-turns.md).
- Blocked by A-103.
- Storing the reverse action (rather than recomputing at undo time) is what lets undo work after a
  restart, from another process, and on a session this process never ran — do not "optimize" it into
  a recompute.
- Watch the store-size question: a `Snapshot` of a large file is a large event. Bound it and degrade
  to `None { why: "pre-image too large" }` past the bound rather than writing an unbounded blob.

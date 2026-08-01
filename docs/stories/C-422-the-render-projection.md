---
id: C-422
title: "Rebuild a session's visual timeline from the durable log — and say honestly what cannot be rebuilt"
pillar: Core
status: ready
priority: 12
design: docs/designs/session-screencast.md
epic: session-screencast
areas: [flux-tui, flux-events]
note: "the epic's real work and its real risk. The TUI's durable→screen path is 100 lines handling FIVE observation kinds (crates/flux-tui/src/projection.rs) against TWENTY-SIX live UiEvent variants. The data is largely on disk; the projection is not. Gates C-423"
---

# The data is there. The projection is not.

## Goal

Turn a recorded session into the ordered, timestamped timeline a renderer can paint — and publish a
**fidelity table** stating, per `UiEvent` variant, whether a cast shows the truth, an approximation, or
nothing.

## What the code actually says

The premise "everything is recorded anyway" is half right, and the half that is wrong is this story:

**Recorded.** `flux-events` rows carry `ts INTEGER NOT NULL` at **millisecond** resolution
(`crates/flux-events/src/store/sqlite.rs` ~line 481, stamped by `now_ms()`), so pacing needs no new
capture. `EventKind` holds `SessionStarted`, `TurnStarted`, `Message`, `PlanAttempted`, `Compacted`,
`ModelChanged`, `Run(RunEvent)`; since C-43, `RunEvent::OpRecorded` carries redacted op output durably.

**Not recorded.** `UiEvent` (`crates/flux-tui/src/controller.rs:8`) is `pub(super)` and ephemeral — the
live render stream, never persisted. The existing durable→screen path,
`crates/flux-tui/src/projection.rs::historical_observation_entry`, is **100 lines handling five
observation kinds**: `flow.brief`, `flow.halt`, `skill.activated`, `KIND_TURN_INTENT`,
`KIND_DESTRUCTIVE`. Against **26** `UiEvent` variants.

> 5 of 26. Replaying a session into the TUI today produces a thin summary, not the screen the operator
> saw.

## Acceptance

- [ ] **Failing-first**: a test building a timeline from a fixture session and asserting an ordered,
      millisecond-paced sequence, failing at the merge base.
- [ ] One entry point from a session id to an ordered timeline, reusing the store's `ts` for pacing and
      the existing `Entry` vocabulary for content — **a cast and a live run must share one notion of
      what a turn looks like**, or the cast stops being evidence of the product.
- [ ] **The fidelity table ships as a committed artifact**, classifying every one of the 26 variants:
      - *faithful* — rebuilt from a durable event;
      - *approximated* — synthesised, **and the cast says so in its output**;
      - *absent* — not recorded; the cast shows nothing rather than a guess.
- [ ] ⚠ **An approximation is visible in the rendered output, not only in a doc comment.** This repo's
      recurring defect class is *a guard or comment that agrees with its own assumption*, and a
      projection that quietly interpolates a plausible tool-output tail is that bug in a demo costume.
      A test must assert the marker is present for at least one approximated variant.
- [ ] ⚠ **`Compacted` is decided and written down.** A compacted session has had messages *replaced* in
      the log. Rendering post-compaction state as though it were what the operator saw is a silent
      falsification. Pick pre- or post-compaction, say which, and pin it with a test over a fixture
      that compacted.
- [ ] The variants likely to be genuinely unrecoverable are each classified with a reason, not left
      out: `ToolProgress` (C-158's live tail), spinner frames, `Retry` countdowns, `Approval` modal
      pauses, and `SpawnActivity` (check A-79's `subagent.activity` observation before calling it
      absent — it may be the recoverable one).
- [ ] Full gate green.

## Notes

- The renderer ([C-423](C-423-flux-cast.md)) depends on this; [C-421](C-421-tui-takes-a-task-from-the-cli.md)
  does not, and neither does this depend on C-421.
- ⚠ The temptation, once a renderer exists downstream, is to settle the fidelity question with whatever
  makes the demo look good. That is exactly why this is a separate story that lands first.
- A-45's `flux replay` re-executes from the cassette; this reads the log and paints. Different data
  paths — do not assume the replay path already answers this.
- `load_history` in the same file loads messages and is the other half of today's reconstruction; read
  both before deciding what is missing.

## Progress

- Filed 2026-08-01 with the session-screencast epic, after an audit of `projection.rs` against
  `controller.rs` produced the 5-of-26 figure.

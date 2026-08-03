---
id: D-233
title: "A control event *can* be shed, silently and uncounted — three places say it never is"
pillar: Agent
status: done
priority: 3
design: docs/designs/meeting-rooms.md
epic: meeting-rooms
areas: [flux-channels]
note: "found by D-208's review. Audio cannot cause it — the reserve holds against a flood — but with nothing consuming the stream yet, 32 unconsumed control events saturate the reserve and the 33rd is lost AND not counted. ⚠ D-209 is about to lean on the guarantee"
---

# The guarantee is true of audio and not of the queue

## Goal

Make the "control events are never shed" claim true, or make the code and the three places that state
it agree.

## The finding

`crates/flux-channels/src/rooms/media/mod.rs:243-250`: on `TrySendError::Full`, a non-droppable event
returns `Delivery::Dropped`, and the counter is bumped **only** `if event.is_droppable()`. So the 33rd
unconsumed `participant`/`speech_started` is lost **and invisible**.

⚠ Three places assert otherwise:
- `mod.rs:189` — *"Only ever an audio frame"*
- `mod.rs:57-58` — *"Control events … are never shed"*
- `docs/designs/meeting-rooms.md` — *"so `speech_started` and `participant` are never shed"*

**Audio cannot cause it.** `send` drops audio once `tx.capacity() <= reserve`, so audio occupies at most
`capacity - 32` slots — a 2001-event flood was verified not to lose a barge-in. The gap is that the
reserve is finite and *control alone* can exhaust it, which is reachable today precisely because
nothing consumes the inbound stream until D-209.

## Acceptance

- [x] **Failing-first**: a test flooding 33+ *control* events into an unconsumed stream and asserting
      either that none is lost, or that a loss is counted and surfaced — failing at the merge base.
- [x] Either the guarantee becomes true, or the wording in **all three places** is narrowed to what the
      code delivers (e.g. *"never shed because of audio"*). ⚠ Do not fix the comment in one place and
      leave the other two — a claim that survives in two of three files is how this recurs.
- [x] A shed control event is **counted**, whatever the wording ends up being. Silent and uncounted is
      the part with no defence.
- [x] ⚠ Coordinate with [D-209](D-209-room-audio-in.md): it consumes the inbound stream and will lean on
      this guarantee. If D-209 lands first, the pressure disappears in practice but the claim is still
      wrong — fix the claim regardless.

## Notes

- The reserve mechanism itself is sound and reviewed; this is about the boundary case and the docs, not
  a redesign.
- Same class as this repo's recurring finding — a comment asserting a property the code does not
  deliver — which is why it is worth a story rather than a passing edit.

## Progress

- Filed 2026-08-02 from D-208's review.
- 2026-08-03 failing first: `cargo test -p codewandler-flux-channels --features room-media
  rooms::media::tests::a_control_flood_is_shed_visibly_when_the_bounded_queue_is_full -- --exact
  --nocapture` failed to compile because the control-loss counter did not exist; at the merge-base
  behavior the 33rd control event was returned as dropped without incrementing any counter.
- 2026-08-03: audio and control losses now have separate public diagnostics on both channel halves,
  and every full-queue loss increments the matching counter. The module API, delivery variant and
  meeting-room design consistently promise that audio cannot consume the control reserve while
  explicitly documenting that control alone can exhaust it. D-209 remains ready and therefore does
  not change this bounded-queue contract.
- 2026-08-03: the focused regression and all five media module tests passed with `room-media` enabled;
  package clippy with all targets and `-D warnings`, workspace formatting, and `git diff --check`
  passed.

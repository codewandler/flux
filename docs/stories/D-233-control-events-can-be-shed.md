---
id: D-233
title: "A control event *can* be shed, silently and uncounted — three places say it never is"
pillar: Agent
status: ready
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

- [ ] **Failing-first**: a test flooding 33+ *control* events into an unconsumed stream and asserting
      either that none is lost, or that a loss is counted and surfaced — failing at the merge base.
- [ ] Either the guarantee becomes true, or the wording in **all three places** is narrowed to what the
      code delivers (e.g. *"never shed because of audio"*). ⚠ Do not fix the comment in one place and
      leave the other two — a claim that survives in two of three files is how this recurs.
- [ ] A shed control event is **counted**, whatever the wording ends up being. Silent and uncounted is
      the part with no defence.
- [ ] ⚠ Coordinate with [D-209](D-209-room-audio-in.md): it consumes the inbound stream and will lean on
      this guarantee. If D-209 lands first, the pressure disappears in practice but the claim is still
      wrong — fix the claim regardless.

## Notes

- The reserve mechanism itself is sound and reviewed; this is about the boundary case and the docs, not
  a redesign.
- Same class as this repo's recurring finding — a comment asserting a property the code does not
  deliver — which is why it is worth a story rather than a passing edit.

## Progress

- Filed 2026-08-02 from D-208's review.

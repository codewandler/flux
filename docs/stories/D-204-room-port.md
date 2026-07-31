---
id: D-204
title: A Room port — presence, occupants and attributed text as a channel kind
pillar: Agent
status: in-progress
priority: 24
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "the foundation: a `Room` trait + `RoomEvent` stream + `MockRoom`, wired in as `ChannelDecl` kind `room`; every inbound event carries an OccupantId because the existing turn seams have no speaker at all"
---

# A Room port — presence, occupants and attributed text as a channel kind

## Goal

Introduce the `Room` port and its event stream, and admit it into `flux-channels` as a new `room` channel
kind — so a many-party room reaches an agent through the same `Channel`/`Deliverer` machinery D-04 built,
with no new host. Backend-agnostic and text-only: no WebRTC, no vendor.

## Acceptance

- [x] `Room` trait with `join`/`occupants`/`say`/`whisper`/`leave` and a `RoomEvent` stream
      (`Joined`/`Left`/`Message`/`Ended`), per the [design](../designs/meeting-rooms.md).
- [x] **Every inbound event carries an `OccupantId`.** Failing-first test: `room_message_carries_speaker`
      — a `MockRoom` delivering two messages from two occupants yields two turns with distinct speakers.
- [x] `ChannelDecl { kind: "room", settings: { backend, room, nick, address_rule } }` builds through
      `build_channels`; an unknown backend is an error, matching the existing kind→adapter contract.
- [x] `MockRoom` exists as the in-process test double (the `Deliverer` recording-double precedent).
- [x] A room-sourced turn dispatches through the ordinary `Executor` + approver — asserted, not assumed
      (the D-213 invariant this story must not break).

## Progress

Landed. The port is `crates/flux-channels/src/rooms/` (L6, a module — no new crate, so no `layer()`
map change): `Room`, `RoomEvent`, `RoomStream`, `MockRoom`, and `RoomTurnDriver` (the Room → L3
turn-seam bridge). `kind = "room"` is `crates/flux-channels/src/adapters/room.rs`, wired into
`build_channels`; settings in `config.rs::RoomSettings`.

**The L3 seam change is breaking, and deliberately so:** `flux_flow::voice::VoiceTurnHandler::turn` is
now `turn(&self, speaker: &Speaker, user_text: &str)`. `Speaker` (new,
`crates/flux-flow/src/voice/speaker.rs`) is a surface-owned id plus an optional display name; the
realtime driver passes `Speaker::sole()`, so a phone line's single caller is now *named* rather than
absent and voice behavior is unchanged. `flux-flow` is published (`codewandler-flux-flow`), so **the
release carrying this owes a MINOR bump.**

Four departures from the design's trait sketch — each one avoiding a breaking change to the port
D-205/D-206 inherit — are written up in the design doc's "As landed" block: `MessageScope` on
`Message`, `#[non_exhaustive]` on `RoomEvent`, `RoomStream` as an owned bounded receiver rather than a
`Stream` (a backend's read loop is a task feeding a channel, and it keeps `futures` out of this
crate), and `kind`/`is_self` on `Occupant`.

Tests — `crates/flux-channels/tests/rooms.rs` plus unit tests in each new module:
- `room_message_carries_speaker` (the failing-first one; at the merge base the port did not exist and
  `turn` had no speaker parameter)
- `a_room_sourced_turn_dispatches_through_the_executor_and_approver` — the D-213 invariant, asserted
  differentially: the same room message through a real `App` is **denied** with no approver consent and
  **runs** with it, so the approver is provably the thing in the path
- `every_inbound_room_event_names_an_occupant`, `the_agent_never_answers_its_own_room_message`,
  `room_channel_builds_from_a_decl_and_rejects_an_unknown_backend`,
  `room_turn_reply_goes_back_into_the_room_and_leaves_on_completion`
- voice-side regression: `flow_owns_two_voice_turns` now asserts a 1:1 call attributes every turn to
  `SOLE_SPEAKER_ID`

**Left for the stories that own them:** `address_rule` is parsed and carried but **not enforced** —
today every inbound message produces a turn, which is D-207's problem to fix, and half a rule here
would be worse than none. `EngineVoiceHandler` accepts the speaker and ignores it: `FlowEngine::run_turn`
has a single text slot, and giving the flow attributed context is also D-207. The design's invariant 5
(self-announcement on join) is not implemented and is not in any story's Acceptance.

## Notes
- The seam to extend is `flux-flow::voice`'s `VoiceTurnHandler::turn(&self, user_text: &str)` — it has no
  speaker parameter, which is exactly the 1:1 assumption this story breaks.
- Do **not** add media to this story; audio/video land in D-208…D-211 behind a feature gate.

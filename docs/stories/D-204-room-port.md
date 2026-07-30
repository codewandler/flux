---
id: D-204
title: A Room port — presence, occupants and attributed text as a channel kind
pillar: Agent
status: ready
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

- [ ] `Room` trait with `join`/`occupants`/`say`/`whisper`/`leave` and a `RoomEvent` stream
      (`Joined`/`Left`/`Message`/`Ended`), per the [design](../designs/meeting-rooms.md).
- [ ] **Every inbound event carries an `OccupantId`.** Failing-first test: `room_message_carries_speaker`
      — a `MockRoom` delivering two messages from two occupants yields two turns with distinct speakers.
- [ ] `ChannelDecl { kind: "room", settings: { backend, room, nick, address_rule } }` builds through
      `build_channels`; an unknown backend is an error, matching the existing kind→adapter contract.
- [ ] `MockRoom` exists as the in-process test double (the `Deliverer` recording-double precedent).
- [ ] A room-sourced turn dispatches through the ordinary `Executor` + approver — asserted, not assumed
      (the D-213 invariant this story must not break).

## Progress
- (not started)

## Notes
- The seam to extend is `flux-flow::voice`'s `VoiceTurnHandler::turn(&self, user_text: &str)` — it has no
  speaker parameter, which is exactly the 1:1 assumption this story breaks.
- Do **not** add media to this story; audio/video land in D-208…D-211 behind a feature gate.

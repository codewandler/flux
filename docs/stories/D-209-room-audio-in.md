---
id: D-209
title: Room audio in — attributed speech from many speakers into the turn seam
pillar: Agent
status: backlog
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-flow, flux-audio]
note: "48 kHz room audio resampled to the realtime model's 24 kHz via flux-audio's phase-carrying Resampler, with per-speaker attribution — the existing voice seam assumes exactly one caller"
---

# Room audio in — attributed speech from many speakers into the turn seam

## Goal

Feed room audio into flux's existing voice path so the agent can *listen* to a meeting: per-speaker
frames from the sidecar, resampled and reframed, into a `VoiceTurnHandler` that now knows who spoke.

## Acceptance

- [ ] Sidecar `audio_frame` events carry an `OccupantId` and are resampled with `flux-audio`'s
      **streaming** `Resampler` (phase carried across packets — the stateless function is lossy at packet
      seams) and reframed with `Framer` to the model's frame size.
- [ ] Failing-first test `room_audio_attributes_two_speakers`: interleaved frames from two occupants
      produce two attributed transcript segments, not one merged blob.
- [ ] `TranscriptAccumulator` gains per-speaker segmentation, and the close-flush behaviour is preserved
      (a participant leaving mid-utterance must not silently drop the last segment).
- [ ] Barge-in still works: `SpeechStarted` from any occupant cancels an in-flight agent response.
- [ ] Turns and usage land in the event store exactly as a text or phone turn does (telemetry parity).

## Progress
- (not started)

## Notes
- WebRTC is 48 kHz; OpenAI Realtime is 24 kHz. `flux-audio` exists for exactly this seam.
- The 1:1 assumption to break is `VoiceTurnHandler::turn(&self, user_text: &str)`; D-204 supplies the
  speaker identity.

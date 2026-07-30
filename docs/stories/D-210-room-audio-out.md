---
id: D-210
title: Room audio out — the agent speaks into the call, interruptibly
pillar: Agent
status: backlog
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-flow, flux-audio]
note: "proven live 2026-07-30 with a local neural TTS (piper en_US-ryan-high) published through a private virtual mic and confirmed audible by the human in the call; needs productionizing, barge-in, and a level preflight"
---

# Room audio out — the agent speaks into the call, interruptibly

## Goal

Let the agent speak into a room: model or TTS audio published as its microphone track, cancellable the
moment a human starts talking, and never silently inaudible.

## Acceptance

- [ ] Agent speech (realtime-model output or a TTS op) is published through the D-208 sidecar as the bot's
      audio track.
- [ ] **Barge-in:** a human `SpeechStarted` cancels the in-flight utterance — reuse the existing
      `run_flow_turns` cancellation path rather than inventing new logic.
- [ ] **A level preflight, because "unmuted" is not "audible".** Failing-first test
      `published_audio_carries_signal`: a published track whose source is silence is reported as a failure
      rather than presented as success. This exists because the spike observed the bridge electing the bot
      dominant speaker while the human heard nothing.
- [ ] Publishing audio into a room with humans in it is an **approved** act, not an implicit capability
      (the D-213 invariant).
- [ ] The agent's own speech is never fed back as inbound audio (no self-echo turn).

## Progress
- 2026-07-30 — **audible in a real call, confirmed by the human ("now, i hear you!").** Chain:
  `piper en_US-ryan-high` (local neural TTS, 43 s of speech synthesized in 7.6 s) → `sox` to 48 kHz mono
  → `paplay` into a private PipeWire null sink → remapped source → the browser capture stream moved onto
  it per-stream → Opus over WebRTC. espeak-ng was tried first and was intelligible but robotic; the
  neural voice is the one worth shipping.

## Notes
- The routing scars (Chrome 150's dead fake-device flags, `setAudioInputDevice` not sticking) are recorded
  on D-208 — they belong to the sidecar, not here.
- A TTS op needs a provider decision: local (piper) vs a hosted voice. Local keeps a meeting private,
  which fits the room's consent posture (D-213).

---
id: D-208
title: The room media sidecar — a browser-grade WebRTC peer flux drives
pillar: Agent
status: ready
priority: 2
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "the Jibri pattern: headless Chrome runs lib-jitsi-meet, flux drives it over NDJSON; ⚠ measured 2026-07-30 — Chrome 150 IGNORES --use-fake-device-for-media-capture, and Jitsi's setAudioInputDevice does not stick, so the sidecar must own device routing itself"
---

# The room media sidecar — a browser-grade WebRTC peer flux drives

## Goal

Give the room channel an optional, **feature-gated** media peer: a headless browser process that owns the
WebRTC stack (ICE, DTLS-SRTP, simulcast, Jingle) while flux drives it over a thin local NDJSON control
protocol. Audio (D-209/D-210) and screenshare (D-211) both land on this seam. Text and presence must keep
working with the sidecar absent.

## Acceptance

- [ ] A `MediaPeer` seam with a documented NDJSON control protocol: `join`/`leave`, `publish_audio`,
      `publish_video`, `mute`, plus inbound `audio_frame` / `speech_started` / `participant` events.
- [ ] The sidecar is behind a Cargo feature and an explicit config opt-in. Failing-first test
      `room_text_works_without_media_sidecar`: the text+presence suite passes with the feature off and no
      browser on `PATH`.
- [ ] The sidecar owns **device routing** rather than trusting the browser's defaults — see the measured
      findings below; a test or documented preflight asserts the published track actually carries signal
      (a level probe), because "unmuted" is not the same as "audible".
- [ ] Sidecar death is survivable: the room stays joined for text/presence, and the failure surfaces as an
      operation failure rather than killing the session.
- [ ] No unbounded buffering: inbound audio frames drop rather than grow without limit if flux is behind.

## Progress
- 2026-07-30 — spike drove a real Brave Talk call from headless Chrome via the JaaS external API.
  **Audio out worked and was confirmed audible by the human in the call.** Three findings, all measured,
  all of which the sidecar design must absorb:
  1. **Chrome 150 ignores `--use-fake-device-for-media-capture`.** Device labels stay real and even
     Chrome's built-in beep tone never appears (probe peak 0.0004 = silence). The classic
     `--use-file-for-fake-audio-capture` recipe is dead on this version.
  2. **Jitsi's `setAudioInputDevice` did not stick.** The page called it with the correct label and
     deviceId; `getCurrentDevices()` kept reporting `Default`, and the published track stayed silent.
  3. **What worked:** a private PipeWire/Pulse null sink + remapped source, then moving *only our own*
     browser capture stream onto it with `pactl move-source-output <id> fluxagent_mic`. Per-stream, so the
     human's own microphone stream was never touched. A level probe inside the page confirmed
     `rms≈0.12` before the human confirmed audibility.
  Also: the bridge elected the bot dominant speaker *before* audio was truly audible, so
  `dominantSpeakerChanged` is **not** evidence that a track carries signal.

## Notes
- Design decision and the rejected alternatives (native `webrtc-rs`, SIP/jigasi) are in the design's
  "The media problem, and the decision".
- The virtual-device approach is Linux-specific. The seam must keep that inside the sidecar so the port
  stays portable.

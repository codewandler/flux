---
id: D-208
title: The room media sidecar — a browser-grade WebRTC peer flux drives
pillar: Agent
status: done
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "⚠ SEAM ONLY — the browser harness is NOT shipped and is D-232. The MediaPeer port, NDJSON protocol, level probe and backpressure are done and tested; there is still NO audio in a real call. D-209/D-210/D-211 are unblocked on the flux side only"
---

# The room media sidecar — a browser-grade WebRTC peer flux drives

## Goal

Give the room channel an optional, **feature-gated** media peer: a headless browser process that owns the
WebRTC stack (ICE, DTLS-SRTP, simulcast, Jingle) while flux drives it over a thin local NDJSON control
protocol. Audio (D-209/D-210) and screenshare (D-211) both land on this seam. Text and presence must keep
working with the sidecar absent.

## Acceptance

- [x] A `MediaPeer` seam with a documented NDJSON control protocol: `join`/`leave`, `publish_audio`,
      `publish_video`, `mute`, plus inbound `audio_frame` / `speech_started` / `participant` events.
      → `crates/flux-channels/src/rooms/media/{mod,protocol,sidecar,mock}.rs`; the wire is
      `flux.room-media.v1`, specified in `protocol.rs`'s header and in the design, and driven end to end
      over real pipes by `tests/room_media.rs::the_control_protocol_round_trips_over_the_real_wire`.
- [x] The sidecar is behind a Cargo feature and an explicit config opt-in. Failing-first test
      `room_text_works_without_media_sidecar`: the text+presence suite passes with the feature off and no
      browser on `PATH`. → feature `room-media` (off by default, **no new dependency**);
      `RoomSettings.media` is the opt-in and is a **load error** without the feature.
      `tests/rooms.rs::room_text_works_without_media_sidecar` asserts both halves.
- [x] The sidecar owns **device routing** rather than trusting the browser's defaults — see the measured
      findings below; a test or documented preflight asserts the published track actually carries signal
      (a level probe), because "unmuted" is not the same as "audible". → the protocol names no device at
      all (`protocol.rs::the_protocol_never_names_a_capture_device`); the sidecar must claim
      `owns_device_routing` in its handshake or audio publication is refused
      (`room_media.rs::a_sidecar_that_does_not_own_device_routing_may_not_publish_audio`);
      `MediaPeer::verify_audible` fails silence, NaN and a measurement-less reply
      (`a_published_track_that_carries_silence_fails_the_probe`). Preflight runbook: the design's
      "Sidecar preflight". ⚠ **Nothing here has been measured against a real browser** — see Progress.
- [x] Sidecar death is survivable: the room stays joined for text/presence, and the failure surfaces as an
      operation failure rather than killing the session. →
      `room_media.rs::a_media_sidecar_that_cannot_start_leaves_text_and_presence_running` (a real,
      unmocked spawn of a program that is not there) and
      `a_sidecar_that_dies_mid_call_fails_its_operations_with_a_reason`.
- [x] No unbounded buffering: inbound audio frames drop rather than grow without limit if flux is behind.
      → `MediaEventSender::send` sheds audio and reserves `MEDIA_CONTROL_RESERVE` slots for control
      events; `room_media.rs::inbound_audio_drops_rather_than_growing_without_limit` floods 2 001 events
      into a 64-slot queue and asserts it never grew past it *and* the barge-in still arrived.

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

- 2026-08-01 — **the flux-side seam is in and green; the sidecar program itself is not shipped.** Read
  this before treating D-209/D-210/D-211 as unblocked.

  **What landed.** `rooms::media` behind the `room-media` cargo feature: the `MediaPeer` port, the
  `flux.room-media.v1` NDJSON protocol (serde types, codec, correlation ids, handshake), a
  `SidecarMediaPeer` that spawns a sidecar through `flux_system::System::spawn_interactive` and speaks
  it, a bounded audio-shedding `MediaStream`, a `MockMediaPeer` for the stories above, and the
  `media { … }` channel opt-in wired into `RoomChannel`. 23 tests (11 integration + 12 unit), run in CI
  by `scripts/check-feature-gated-tests.sh`.

  **What did not land, deliberately.** The browser harness — the process that runs `lib-jitsi-meet`,
  routes its own capture device and answers the protocol. Writing it means writing several hundred lines
  of browser-side code whose only meaningful test is a live call, and a sidecar that looks finished and
  is silent is worse before a demo than no sidecar at all. The protocol spec plus the preflight runbook
  in the design are the contract to write it against.

  **What no test in this repo can tell you**, stated plainly because a demo is riding on it:
  1. That a real browser publishes audible audio. Every level number here comes from a scripted double —
     the probe is only as honest as the in-page RMS measurement behind it.
  2. That the PipeWire null-sink + `move-source-output` recipe survives being driven by a *spawned*
     sidecar with a **cleared environment**. flux clears `DISPLAY`/`XDG_RUNTIME_DIR`/`PULSE_SERVER`; the
     seam therefore requires argv to carry anything the sidecar needs about the host audio server. That
     is documented and not exercised.
  3. That Chrome runs at all inside flux's bubblewrap confinement. `spawn_interactive` is `Sandboxed`,
     and Chrome's content sandbox needs a nested user namespace — the reason tier-3 browsing has an
     explicit exemption. Untested against a live call; may need `FLUX_SANDBOX=off`.
  4. Anything about ICE/DTLS/simulcast/Jingle behaviour. Nothing in this diff opens a WebRTC session.

  **Also not done** (out of this story's Acceptance, worth filing): the media plane does not
  self-announce on join (the design's invariant 5), and `RoomChannel` starts and holds the sidecar but
  nothing consumes the inbound stream yet — so with media on today, inbound audio is shed by design and
  the count is reported at session end. D-209 is what plugs a consumer in.

## Notes
- Design decision and the rejected alternatives (native `webrtc-rs`, SIP/jigasi) are in the design's
  "The media problem, and the decision". The seam as built is in "The media seam — `MediaPeer` and
  `flux.room-media.v1`", and the operator-facing recipe is in "Sidecar preflight — the runbook".
- The virtual-device approach is Linux-specific. The seam must keep that inside the sidecar so the port
  stays portable. **Held**: the control protocol has no device/sink/source/audio-server field, pinned on
  the rendered wire by `protocol.rs::the_protocol_never_names_a_capture_device` rather than by a comment.
  Host specifics ride in the sidecar's argv, which flux passes through and never interprets.
- D-204 expected the media events to arrive as new `RoomEvent` variants. They did not: they are
  `media::MediaEvent` on a separate stream from a separate port, so a text consumer's `match` never grows
  a browser-shaped arm and `RoomEvent` stays free of the feature. `rooms/mod.rs` records the change where
  the old expectation was written.

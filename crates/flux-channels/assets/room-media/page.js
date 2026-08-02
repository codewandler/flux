// page.js — everything that runs *inside* the browser (D-232).
//
// Injected into an `about:blank` page by `sidecar.js` over CDP, alongside `measure.js`. It owns
// three things and nothing else:
//
//   1. The outbound audio track: an `AudioContext` → `MediaStreamDestination` whose track is what
//      `lib-jitsi-meet` publishes. PCM16 chunks flux sends are appended to it, scheduled
//      back-to-back so consecutive chunks play as continuous speech rather than overlapping.
//   2. The level probe: a **second** `AudioContext` that re-wraps the *published track* as a fresh
//      `MediaStreamSource` and reads real sample frames out of an `AnalyserNode`. Measuring the
//      track rather than the buffers we wrote is the entire point — see below.
//   3. The `lib-jitsi-meet` conference: join, publish, participant events, and the inbound tracks.
//
// ## Why the probe re-wraps the track instead of measuring what it was given
//
// D-208's report names the failure this defends against: *"a sidecar that hardcodes `rms: 0.5`
// passes."* A probe that reported the amplitude of the PCM flux sent would be a slightly subtler
// version of the same lie — it would report what flux *asked for*, which is exactly the number that
// stays healthy when the track is silent. So the measurement path deliberately shares nothing with
// the publish path but the track itself: it reads `getFloatTimeDomainData` off an analyser fed by
// `new MediaStream([track])`.
//
// This was verified rather than assumed. On 2026-08-02 the probe measured a one-chunk **lag** behind
// the amplitude that had just been pushed (send 0.5 → read 0.355; send 0.1 → still read 0.353;
// send 0.0 → read 0.071). That lag is the proof: a probe reporting its input would have tracked the
// requested amplitude instantly. It is reading the track as it actually plays.
//
// ## What is deliberately absent
//
// No `--use-fake-device-for-media-capture`, no `--use-file-for-fake-audio-capture`, no
// `setAudioInputDevice`. All three were measured dead on Chrome 150 (D-206/D-208) and all three fail
// by *reporting success*, which is the worst way for a media path to fail. The outbound track here is
// synthesized in-page from PCM flux sends, so it needs no capture device at all.

"use strict";

(() => {
  const H = (globalThis.FluxRoomMedia = {
    conference: null,
    connection: null,
    localAudio: null,
    outbound: null,
    probe: null,
    events: [],
    joined: false,
  });

  const { windowLevel, pcm16ToFloat } = globalThis.FluxMeasure;

  const decodeBase64 = (b64) => {
    const raw = atob(b64);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return bytes;
  };

  /// Queue an event for the sidecar to poll and forward as a protocol `{"event":…}` line.
  const emit = (event) => {
    H.events.push(event);
    // Bound it. Inbound audio arrives ~50×/s and flux's own `MediaStream` sheds it anyway; letting
    // it accumulate here would just move the unbounded queue into the browser.
    if (H.events.length > 512) H.events.splice(0, H.events.length - 512);
  };

  H.drainEvents = () => H.events.splice(0, H.events.length);

  // ── the outbound track ─────────────────────────────────────────────────────────────────────────

  /// Build the audio graph and the probe. Idempotent.
  H.setupAudio = () => {
    if (H.outbound) return { sampleRate: H.outbound.ctx.sampleRate };

    const ctx = new AudioContext({ sampleRate: 48000 });
    const dest = ctx.createMediaStreamDestination();
    const track = dest.stream.getAudioTracks()[0];
    if (!track) throw new Error("no outbound audio track");
    H.outbound = { ctx, dest, track, playAt: 0 };

    // The probe: a separate context, fed by the published track re-wrapped as a source.
    const probeCtx = new AudioContext({ sampleRate: 48000 });
    const analyser = probeCtx.createAnalyser();
    analyser.fftSize = 2048;
    probeCtx.createMediaStreamSource(new MediaStream([track])).connect(analyser);
    H.probe = { ctx: probeCtx, analyser, buffer: new Float32Array(analyser.fftSize) };

    return { sampleRate: ctx.sampleRate };
  };

  /// Append one PCM16 chunk to the outbound track. Returns its duration in seconds.
  H.pushAudio = (base64, sampleRateHz, channels) => {
    H.setupAudio();
    const { ctx, dest } = H.outbound;
    const samples = pcm16ToFloat(base64, decodeBase64);
    if (samples.length === 0) return 0;

    // Interleaved input, one channel out: a voice track is mono, and the room only ever wants one.
    const frames = Math.floor(samples.length / channels);
    const buffer = ctx.createBuffer(1, frames, sampleRateHz);
    const channel = buffer.getChannelData(0);
    for (let i = 0; i < frames; i++) channel[i] = samples[i * channels];

    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(dest);
    // Schedule after whatever is already queued, never before `currentTime` — chunks that overlap
    // sum into distortion, and chunks scheduled in the past are dropped silently.
    H.outbound.playAt = Math.max(H.outbound.playAt, ctx.currentTime);
    source.start(H.outbound.playAt);
    H.outbound.playAt += buffer.duration;
    return buffer.duration;
  };

  /// Measure the published track over `windowMs`. The answer to the `level` command.
  H.measure = async (windowMs) => {
    H.setupAudio();
    const { analyser, buffer } = H.probe;
    const frames = [];
    const until = Date.now() + windowMs;
    // Sample repeatedly: one analyser read is 2048 samples (~43 ms at 48 kHz), and a single frame
    // can land in the gap between two words.
    do {
      await new Promise((r) => setTimeout(r, 20));
      analyser.getFloatTimeDomainData(buffer);
      frames.push(Float32Array.from(buffer));
    } while (Date.now() < until);

    const level = windowLevel(frames);
    return {
      rms: level.rms,
      peak: level.peak,
      // Diagnostics, not evidence. `muted` is here because reading it *instead of* the level is the
      // mistake invariant 8 exists to prevent, and having both in one reply makes that visible.
      track: H.outbound.track.readyState,
      muted: H.outbound.track.muted,
      published: Boolean(H.localAudio),
    };
  };

  H.setMuted = async (muted) => {
    if (!H.localAudio) throw new Error("no published audio track to mute");
    muted ? await H.localAudio.mute() : await H.localAudio.unmute();
    return { muted };
  };

  // ── the conference ─────────────────────────────────────────────────────────────────────────────

  /// Join `roomJid`'s room via lib-jitsi-meet, publishing the outbound track built above.
  ///
  /// `options` carries what the JaaS handshake already produced on the flux side (D-206): the tenant,
  /// the JWT, and the room name as the *server* spelled it. The sidecar does not re-derive any of it.
  H.join = (options) =>
    new Promise((resolve, reject) => {
      const JitsiMeetJS = globalThis.JitsiMeetJS;
      if (!JitsiMeetJS) return reject(new Error("lib-jitsi-meet did not load"));

      JitsiMeetJS.setLogLevel(JitsiMeetJS.logLevels.ERROR);
      JitsiMeetJS.init({ disableAudioLevels: false });

      const { tenant, token, room, serviceUrl, hosts, nick } = options;
      H.setupAudio();

      const connection = new JitsiMeetJS.JitsiConnection(null, token, {
        hosts: { domain: hosts.domain, muc: hosts.muc },
        serviceUrl,
        clientNode: "https://github.com/codewandler/flux",
      });
      H.connection = connection;

      const events = JitsiMeetJS.events;
      const failed = (e) => reject(new Error(`connection failed: ${e || "unknown"}`));
      connection.addEventListener(events.connection.CONNECTION_FAILED, failed);

      connection.addEventListener(events.connection.CONNECTION_ESTABLISHED, async () => {
        try {
          const conference = connection.initJitsiConference(room.toLowerCase(), {
            openBridgeChannel: true,
          });
          H.conference = conference;

          conference.setDisplayName(nick);

          conference.on(events.conference.USER_JOINED, (id, user) => {
            emit({
              event: "participant",
              occupant: `${room}/${user.getDisplayName() || id}`,
              nick: user.getDisplayName() || id,
              kind: "unknown",
              present: true,
            });
          });
          conference.on(events.conference.USER_LEFT, (id, user) => {
            emit({
              event: "participant",
              occupant: `${room}/${(user && user.getDisplayName()) || id}`,
              nick: (user && user.getDisplayName()) || id,
              kind: "unknown",
              present: false,
            });
          });

          // Barge-in. `TRACK_AUDIO_LEVEL_CHANGED` on a *remote* track is a real measurement of that
          // participant's audio, which is why it is the signal here — unlike
          // `DOMINANT_SPEAKER_CHANGED`, which the 2026-07-30 spike watched fire for a bot whose
          // audio nobody could hear. It is deliberately not subscribed to at all.
          const speaking = new Map();
          conference.on(events.conference.TRACK_AUDIO_LEVEL_CHANGED, (id, level) => {
            const was = speaking.get(id) || false;
            const now = level > 0.02;
            speaking.set(id, now);
            if (now && !was) {
              const user = conference.getParticipantById(id);
              const name = (user && user.getDisplayName()) || id;
              emit({ event: "speech_started", from: `${room}/${name}` });
            }
          });

          conference.on(events.conference.CONFERENCE_JOINED, async () => {
            try {
              // Publish the synthesized track. lib-jitsi-meet ultimately wants a
              // `MediaStreamTrack`, which is exactly what the outbound graph produces — so no
              // capture device is involved anywhere on this path.
              //
              // Two spellings, because the API for wrapping a *caller-supplied* stream differs
              // across lib-jitsi-meet builds and the tenant serves whichever build it likes.
              // ⚠ Both are **unexercised against a live room** (D-232): the guard below is a
              // `typeof` check rather than a `.catch()` precisely because a missing method throws
              // synchronously, which would have made the fallback dead code.
              const stream = new MediaStream([H.outbound.track]);
              let localAudio;
              if (typeof JitsiMeetJS.createLocalTracksFromMediaStreams === "function") {
                [localAudio] = await JitsiMeetJS.createLocalTracksFromMediaStreams([
                  { stream, sourceType: "canvas", mediaType: "audio" },
                ]);
              } else if (typeof JitsiMeetJS.JitsiLocalTrack === "function") {
                localAudio = new JitsiMeetJS.JitsiLocalTrack({
                  stream,
                  track: H.outbound.track,
                  mediaType: "audio",
                  videoType: null,
                  deviceId: "flux-synthesized",
                });
              } else {
                throw new Error(
                  "this lib-jitsi-meet build exposes neither createLocalTracksFromMediaStreams " +
                    "nor JitsiLocalTrack — cannot publish a synthesized track",
                );
              }
              H.localAudio = localAudio;
              await conference.addTrack(localAudio);
              H.joined = true;
              resolve({ joined: true, room: conference.getName() });
            } catch (e) {
              reject(new Error(`publish failed: ${e && e.message}`));
            }
          });

          conference.on(events.conference.CONFERENCE_FAILED, (e) =>
            reject(new Error(`conference failed: ${e}`)),
          );
          conference.join();
        } catch (e) {
          reject(new Error(`join failed: ${e && e.message}`));
        }
      });

      connection.connect();
    });

  H.leave = async () => {
    H.joined = false;
    if (H.conference) {
      try {
        await H.conference.leave();
      } catch {
        // Leaving a conference that already ended is not a failure.
      }
      H.conference = null;
    }
    if (H.connection) {
      try {
        await H.connection.disconnect();
      } catch {
        // Same.
      }
      H.connection = null;
    }
    return { left: true };
  };

  return true;
})();

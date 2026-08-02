---
id: D-232
title: "The browser harness itself — a lib-jitsi-meet peer that joins, publishes audible audio, and proves it"
pillar: Agent
status: in-progress
priority: 1
design: docs/designs/meeting-rooms.md
epic: meeting-rooms
areas: [flux-channels]
note: "⚠ THE Thursday-critical piece. Both D-208 risks are now ANSWERED (2026-08-02): Chrome runs fine under bubblewrap and needs no `--no-sandbox`; but `--tmpfs /run` masks the pulse socket, so argv alone is NOT enough — the operator must also grant `[sandbox] writable = [\"/run/user/<uid>/pulse\"]`. The harness and a genuine in-page RMS probe are in and measured against real Chrome 150. STILL OPEN: nothing has joined a live room, so the lib-jitsi-meet join/publish path is unexercised and no human has confirmed audio"
---

# The half that has to meet a real browser

## Goal

The sidecar process D-208's seam talks to: a headless browser running `lib-jitsi-meet` that joins a
room, publishes **audible** audio, and answers the level probe with a genuine in-page measurement.

## ⚠ Two risks D-208 documented and could not test — both can make a correct sidecar silent

These are the reason this story exists as its own unit rather than as "finish D-208", and either one
sinks a demo without looking broken:

**1. The environment is cleared.** `spawn_interactive` clears the environment down to
PATH/HOME/LANG/TERM/TZ/USER/TMPDIR. **`DISPLAY`, `XDG_RUNTIME_DIR`, `PULSE_SERVER` and
`PULSE_RUNTIME_PATH` do not reach the sidecar** — and D-206's *working* audio recipe is a PipeWire null
sink reached through the user's pulse socket. D-208 therefore requires argv to carry it
(`sidecar ["flux-room-media","--audio-server","unix:/run/user/1000/pulse/native"]`) and deliberately
added no env passthrough, since that would mean new `flux-system` public API and would leak host audio
config into flux. ⚠ **Never exercised.**

**2. Chrome inside bubblewrap.** `spawn_interactive` is `Confinement::Sandboxed`, and Chrome's content
sandbox wants a nested user namespace — which is precisely why tier-3 browsing uses the
`spawn_debug_pipe` exemption. This may need `FLUX_SANDBOX=off` or the browser's own `--no-sandbox`.
D-208 deliberately did **not** add a new `Confinement::Exempt` seam (that inventory is asserted by a
test). ⚠ **Untested against a live call.**

## ⚠ Do the preflight first — it is an hour and it de-risks everything downstream

D-208's implementor named the highest-value next action, and it needs no part of D-209/D-210/D-211:

> Run the preflight runbook end to end against a live Brave Talk room with a **throwaway sidecar that
> only handshakes, joins, and answers `level` with a genuine in-page RMS.**

That single pass validates the cleared environment, Chrome-under-confinement, and whether
`owns_device_routing: true` corresponds to routing that actually happened. **Do this before writing the
real harness**, because if either risk bites, the harness's shape changes.

## Acceptance

- [x] A sidecar that speaks D-208's NDJSON protocol: handshake, `join`, `publish_audio`, `level`.
      → `crates/flux-channels/assets/room-media/sidecar.js` — a dependency-free Node program (Node 22's
      built-in `WebSocket`, no npm install) driving headless Chrome over CDP, answering
      `join`/`leave`/`publish_audio`/`publish_video`/`mute`/`level`. `publish_video` is refused *by name*
      rather than answered `ok`, because a silent no-op is the same class of lie the level probe exists
      to catch; that is D-211's half.
- [ ] ⚠ **Audible audio in a real call, confirmed by a human**, not by a passing test. D-206's spike is
      the precedent — audio out worked and *"was confirmed audible by the human in the call."*
      → **NOT DONE.** No human was in a call, and I cannot put one there. Everything up to the room
      boundary is measured (below); the `join` path against a live Brave Talk room is unexercised, so the
      `lib-jitsi-meet` publish path is code-reviewed, not run. **This is the remaining Thursday risk**;
      the runbook to close it is in Progress.
- [x] ⚠ **The level probe reports a genuine in-page RMS measurement.** D-208 is explicit that flux
      enforces the floor but cannot verify the number: *"a sidecar that hardcodes `rms: 0.5` passes."*
      → `assets/room-media/page.js`'s `measure()` reads real sample frames off the **published
      `MediaStreamTrack`**, re-wrapped through a *second* `AudioContext`, so the measurement path shares
      nothing with the publish path but the track itself. Verified against a real Chrome 150: amplitude
      `0.5` → `rms 0.3550` (analytic `0.3536`), silence → `rms 0.0000`. The arithmetic lives in
      `measure.js`, loaded **both** by the page and by
      `tests/room_media_harness.rs::the_level_probe_computes_a_real_rms_rather_than_reporting_a_constant`
      — the same bytes, so the CI test fails when the shipped page's maths is wrong.
- [x] The two untested risks above are **exercised and their answers recorded** — what argv the sidecar
      needs, and what confinement posture Chrome actually tolerates. → both measured 2026-08-02, and
      **one of them contradicts the story's stated route**; see Progress.
- [x] ⚠ `dominantSpeakerChanged` is **not** treated as evidence of signal.
      → `page.js` never subscribes to `DOMINANT_SPEAKER_CHANGED` at all. Barge-in comes from
      `TRACK_AUDIO_LEVEL_CHANGED` on a *remote* track, which is a real measurement of that participant.
- [x] Device routing is per-stream (`pactl move-source-output <id>`), never touching the default source.
      → `sidecar.js::routeOwnCaptureStream` loads a private null sink + remapped source, then moves only
      the source-outputs whose `application.process.id` is our own Chrome or a descendant of it (matched
      by walking `/proc/<pid>/stat`, since capture happens in a child audio-service process).
      `tests/room_media_harness.rs::the_harness_routes_per_stream_and_never_moves_the_default_source`
      greps the shipped file and fails on `set-default-source`/`set-default-sink` and on the three
      measured dead ends (`use-fake-device-for-media-capture`, `use-file-for-fake-audio-capture`,
      `setAudioInputDevice`). That guard caught a real violation while I wrote it — the forbidden verb
      appeared in a *comment* — which is what says it greps the file rather than the intent.
- [x] Linux-specific machinery stays **inside** the sidecar, per D-208's seam, so the flux-side port
      stays portable. → no flux-side Rust behaviour changed at all. The only Rust here is a new test
      file; `pactl`, PulseAudio, `/proc` and Chrome flags appear exclusively under `assets/room-media/`.

## Notes

- **Priority 1**, alongside [D-224](D-224-the-live-demo-runbook.md): without this there is no audio in a
  call, and D-209/D-210/D-211 have nothing to attach to.
- ⚠ Chrome 150 **ignores** `--use-fake-device-for-media-capture`, and Jitsi's `setAudioInputDevice`
  **does not stick** — both measured on 2026-07-30. Do not build on either.
- ⚠ Two peers in one room means the participant count goes up by one, against Brave's free-tier
  **4-participant cap** (D-206). That cap is a live demo risk in its own right.
- D-208's default timeouts (20 s handshake, 15 s command) are guesses, not measurements — a cold browser
  start on a loaded machine could exceed 20 s. Measure and correct them here.

## Progress

- Filed 2026-08-01 from D-208's PARTIAL report, which delivered the seam and named this half explicitly.

- 2026-08-02 — **the preflight ran first, as the story asked, and it changed the answer to risk 1.**
  Nothing below is folded from memory; every number is from a command run in this worktree on
  Chrome 150.0.7871.46 / PipeWire (pactl 17.0) / bwrap present.

  ### Risk 2 — Chrome under bubblewrap: **it works, and `--no-sandbox` is NOT required**

  Run inside the exact argv `flux-system`'s `bubblewrap_argv` builds (`--die-with-parent --unshare-pid
  --unshare-ipc --unshare-uts --unshare-cgroup-try --ro-bind / / --dev /dev --proc /proc --tmpfs /run
  --bind /tmp /tmp`):

  - `google-chrome-stable --headless=new --dump-dom about:blank` **rendered the page** and exited 0. It
    prints a wall of D-Bus errors (`/run` is masked) which are noise, not failure.
  - `unshare -U -r true` inside the same sandbox **succeeded**, so a nested user namespace is creatable
    and Chrome's own content sandbox has what it needs.
  - The full in-page probe (`selftest.js`) run inside that sandbox reported `rms 0.3550` for a
    0.5-amplitude tone — **identical to the unsandboxed run**.

  So `Confinement::Sandboxed` is fine, no new `Confinement::Exempt` seam is needed, and D-208 was right
  not to add one. `--no-sandbox` stays available as an operator flag for hosts that refuse nesting, and
  is **off by default**: forcing it would trade Chrome's purpose-built sandbox for a weaker generic one.

  ### Risk 1 — the cleared environment: **argv is necessary but NOT sufficient**

  This is the finding that matters, and it contradicts the story's stated route:

  > **`--tmpfs /run` masks the PulseAudio socket.** The socket is at
  > `/run/user/1000/pulse/native`, and the sandbox masks `/run` wholesale (deliberately — it is what
  > keeps `docker.sock` and D-Bus unreachable under `--unshare-net`).

  Measured, inside the sandbox:

  ```
  $ pactl --server=unix:/run/user/1000/pulse/native info
  Connection failure: Connection refused          # inside the sandbox
  Server String: unix:/run/user/1000/pulse/native  # …and it succeeds outside it
  ```

  So `media { sidecar ["flux-room-media","--audio-server","unix:/run/user/1000/pulse/native"] }` alone
  **cannot work**: the path is correct and the file is not there. The operator must *also* grant the
  socket's directory as a writable sandbox path, which re-exposes it past the mask. Verified — adding
  `--bind /run/user/1000/pulse /run/user/1000/pulse` to the same argv makes `pactl … info` succeed
  inside the sandbox.

  ```
  # flux config — BOTH halves are required, and the argv half alone is the silent-failure trap
  [sandbox]
  writable = ["/run/user/1000/pulse"]
  ```

  **No env passthrough was added to `flux-system`**, and none is needed — the sidecar re-exports
  `PULSE_SERVER`/`XDG_RUNTIME_DIR` into *Chrome's* environment from its own argv, which is a child it
  owns rather than new flux public API. `HOME`/`USER` do survive the clear, so `--audio-server` defaults
  to `/run/user/<uid>/pulse/native` instead of being mandatory.

  ### The level probe is a real measurement, and here is why that is checkable

  `page.js` builds the outbound track as `AudioContext → MediaStreamDestination` and measures it by
  re-wrapping **the published track** as a fresh `MediaStreamSource` into an `AnalyserNode` — a separate
  `AudioContext`, sharing nothing with the publish path but the track. Evidence it is not echoing its
  input: while sweeping amplitudes it measured a **one-chunk lag** (sent 0.5 → read 0.355; sent 0.1 →
  still 0.353; sent 0.0 → 0.071). A probe reporting its input would have tracked instantly.

  The arithmetic is a separate file loaded by *both* the page and the Rust test, so CI checks the shipped
  code rather than a reimplementation that would agree with itself. A sine of amplitude `a` has RMS
  `a/√2`, which gives the test a correct answer needing no browser — `1.0/0.5/0.12/0.01/0.0` all checked,
  and a constant-returning probe fails every row but one.

  ### The real sidecar, driven over its real wire (no room)

  Everything up to the room boundary was exercised by piping NDJSON into the shipped `sidecar.js` and
  reading its stdout — a real Chrome, the real protocol, no double anywhere:

  ```
  flux-room-media: device routing: fluxagent_3869885_mic (moved 0 stream(s))
  {"ready":"flux.room-media.v1","owns_device_routing":true}
  {"id":1,"ok":true}                                                    # publish_audio, 5 s @ amp 0.4
  {"id":2,"ok":true,"level":{"rms":0.2840,"peak":0.3999}}               # probed DURING playback
  {"id":3,"ok":true,"level":{"rms":0,"peak":0}}                         # probed AFTER it drained
  {"id":3,"ok":false,"error":"publish_video is not implemented by this sidecar (D-211)"}
  {"id":4,"ok":false,"error":"unknown command nonsense"}
  ```

  `0.2840` against an analytic `0.2828` for amplitude 0.4, and `0` once the tone drained — the probe
  tracks what the track is actually carrying, over the wire, end to end. Note `moved 0 stream(s)`: the
  synthesized track needs no capture device, so there is no source-output to move. The null sink and
  remapped source are still created, so `owns_device_routing` is true and the routing machinery is in
  place for a path that *does* capture.

  ### Timeouts: D-208's 20 s handshake is generous, not tight

  Chrome cold start to a rendered page measured **0.25–0.27 s** over three runs with a fresh
  `--user-data-dir`. Adding CDP attach and asset injection, handshake is ~1 s on this box. The 20 s
  budget stands; I did not change it, because the machine that matters is the demo machine and 20 s of
  headroom on a 1 s operation is the right side to err on.

  ### What is NOT done, stated plainly because the demo rides on it

  **No audio has been in a real call.** The join path (`page.js::join` → `lib-jitsi-meet`) is written
  against D-206's measured handshake but has **never run against a live room**. Specifically unexercised:
  the `JitsiLocalTrack` construction from a synthesized `MediaStream` (two spellings are attempted, one
  as a fallback, because lib-jitsi-meet's API for this differs across builds), the JWT plumbing, and
  whether the bridge accepts a canvas-style synthesized audio track at all.

  Also: **`lib-jitsi-meet` is fetched from the network at join time by default**
  (`https://8x8.vc/libs/lib-jitsi-meet.min.js`, verified reachable, 1 089 184 bytes). That is
  **not reproducible offline and impossible in CI**. `--jitsi <path>` takes a filesystem path instead,
  which is read and injected — the vendored, offline route, and the one to use for anything repeatable.
  Nothing is fetched at build time; no Rust dependency was added; CI never touches either path because
  every browser test is `#[ignore]`d.

  ### The runbook to close the last gap (needs a human in a room)

  ```bash
  # 1. the in-page probe, no room, no network — proves the measurement half
  cargo test -p flux-channels --features room-media --test room_media_harness \
      -- --ignored the_in_page_probe_measures_a_real_track --nocapture

  # 2. against a real room: grant the pulse socket, then join and probe
  #    [sandbox] writable = ["/run/user/1000/pulse"]
  node crates/flux-channels/assets/room-media/sidecar.js \
      --audio-server unix:/run/user/1000/pulse/native
  # then on stdin:
  {"id":1,"cmd":"join","room":"<room>@conference.<tenant>.8x8.vc","nick":"flux","kind":"agent"}
  {"id":2,"cmd":"publish_audio","audio":{"pcm16_le":"<b64 speech>","sample_rate_hz":48000,"channels":1}}
  {"id":3,"cmd":"level"}      # must read > 0.01, and a human must confirm hearing it
  ```

  Expect the first live run to fail in `join`, not in the probe — that is where the untested code is.

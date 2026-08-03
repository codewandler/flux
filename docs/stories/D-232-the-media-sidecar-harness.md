---
id: D-232
title: "The browser harness itself — a lib-jitsi-meet peer that joins, publishes audible audio, and proves it"
pillar: Agent
status: blocked
design: docs/designs/meeting-rooms.md
epic: meeting-rooms
areas: [flux-channels]
note: "blocked on a human in an operator-owned live room confirming the tone is audible; the isolated impl/D-232 worktree proves Chrome confinement, Pulse routing and genuine in-page RMS but has not crossed the live Jitsi boundary"
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

- [ ] A sidecar that speaks D-208's NDJSON protocol: handshake, `join`, `publish_audio`, `level`.
- [ ] ⚠ **Audible audio in a real call, confirmed by a human**, not by a passing test. D-206's spike is
      the precedent — audio out worked and *"was confirmed audible by the human in the call."*
- [ ] ⚠ **The level probe reports a genuine in-page RMS measurement.** D-208 is explicit that flux
      enforces the floor but cannot verify the number: *"a sidecar that hardcodes `rms: 0.5` passes."*
      This story is the other half of that contract, and hardcoding it would defeat the only mechanism
      standing between us and a silent demo.
- [ ] The two untested risks above are **exercised and their answers recorded** — what argv the sidecar
      needs, and what confinement posture Chrome actually tolerates.
- [ ] ⚠ `dominantSpeakerChanged` is **not** treated as evidence of signal. Measured in the 2026-07-30
      spike: the bridge elected the bot dominant speaker *before* its audio was audible.
- [ ] Device routing is per-stream (`pactl move-source-output <id>`), never touching the default source
      — the human's own microphone must not move. This is D-206's measured recipe; do not re-derive it.
- [ ] Linux-specific machinery stays **inside** the sidecar, per D-208's seam, so the flux-side port
      stays portable.

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
- 2026-08-03: The isolated `impl/D-232` worktree contains the browser harness and measured Chrome
  under bubblewrap, the required Pulse socket bind, per-stream routing, and a genuine in-page RMS
  probe. It remains deliberately unmerged: no human has yet joined an operator-owned live room and
  confirmed the published tone is audible, which is an explicit acceptance criterion that CI cannot
  substitute for. Resume from that preserved branch and its live-test runbook.

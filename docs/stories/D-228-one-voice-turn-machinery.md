---
id: D-228
title: "One voice-turn machinery for rooms and SIP — a call is a room with one participant and a worse codec"
pillar: Agent
status: backlog
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-flow, flux-audio, flux-channels]
note: "⚠ rooms are building this RIGHT NOW — D-209 (audio in, attributed) and D-210 (audio out, interruptible). crates/flux-flow/src/voice/ already holds driver, sink, speaker, transcript, room_transcript, and VoiceTurnHandler is the seam. A second voice path would drift within a release"
---

# Do not build voice twice

## Goal

A SIP call and a room call run through the same voice-turn machinery.

## Why this is a story rather than an assumption

The room epic is building exactly this in parallel: **D-209** (room audio in — attributed speech from
many speakers into the turn) and **D-210** (room audio out — the agent speaks, interruptibly).
`crates/flux-flow/src/voice/` already carries `driver.rs`, `sink.rs`, `speaker.rs`, `transcript.rs` and
`room_transcript.rs`, with `VoiceTurnHandler` as the seam — which D-207 extended with an `overheard`
method days ago.

⚠ **A SIP call is a room with one remote participant and a worse codec.** If the SIP channel grows its
own voice path, the two will diverge — different interruption behaviour, different attribution,
different transcripts — and every later voice feature costs twice. This is the same argument as "do not
grow a second plan renderer", applied where the drift is harder to see.

**And the sample math is already written.** `flux-audio` names this target in its own doc: *"telephony's
8 kHz, WebRTC's 48 kHz, a device mic's 16 kHz, versus whatever a model speaks natively"* — PCM16 both
endiannesses, a phase-carrying streaming `Resampler` (so re-chunked audio is not lossy at the seams),
and a `Framer`. G.711 is 8 kHz. It is dependency-free and pure.

## Acceptance

- [ ] SIP audio in and out route through `VoiceTurnHandler` and the existing `voice/` machinery — **no
      second driver, sink or speaker**.
- [ ] Rate conversion uses `flux-audio`'s `Resampler` (phase-carrying across packet boundaries) rather
      than a new one. ⚠ A per-packet stateless resample at a codec seam is lossy in a way that is
      audible but hard to attribute later.
- [ ] Attribution still works with one remote party: the transcript says who spoke, and "the caller" is
      a principal, not a label.
- [ ] Interruption behaves the same as in a room — a caller talking over the agent stops it.
- [ ] ⚠ **A measured latency budget**, not an assumed one. A phone conversation is unforgiving:
      resample → model → resample back must fit inside a turn a person will wait through. Rooms hit this
      first; take their number rather than re-deriving it.
- [ ] Full gate green.

## Notes

- Depends on [D-225](D-225-the-sip-sidecar-seam.md) for transport, and on D-209/D-210 landing the
  machinery it reuses. If SIP arrives first, it must still not fork the path.
- G.711 is narrowband. What a model trained on wideband audio does with 8 kHz telephony speech is an
  empirical question worth measuring before promising quality.

## Progress

- Filed 2026-08-01 with the sip-channel epic.

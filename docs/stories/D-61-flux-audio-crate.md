---
id: D-61
title: flux-audio — L0 crate for PCM16 conversion, streaming resampling, framing
pillar: Agent
status: done
epic: consumer-gaps
note: "from the 2026-07-06 downstream-consumer review: flux-core's audio doc punts resampling to 'the consumer's concern', which guarantees every voice consumer re-writes identical DSP (PCM16 codecs, phase-carrying resampler, framer) — revise the doctrine, ship the crate"
---

# flux-audio

## Goal
Ship the sample-math layer every realtime-voice consumer needs: PCM16 LE/BE ⇄ `i16` samples, a
stateless resampler, a **stateful streaming resampler** (integer up/down ratios that carry phase
across packets — the thing per-packet telephony audio actually requires), and a re-chunking framer.
Dependency-free L0 crate.

## Why (evidence)
`flux-core/src/audio.rs:5-6` declares resampling "the consumer's concern, not flux's" — flux ships
only the `AudioFormat`/`AudioEncoding` vocabulary. The consequence is deterministic: every consumer
bridging telephony 8 kHz / WebRTC 48 kHz / 16 kHz mic audio to a model-native 24 kHz stream writes
the same ~300 lines of DSP. The reviewed downstream consumer's implementation is clean, pure,
fully tested, and contains nothing app-specific — the doctrine, not the difficulty, is why it lives
downstream. This story consciously revises that position: **formats stay vocabulary (flux-core);
sample math is flux-audio, an optional leaf crate.**

## Acceptance
- [x] New `crates/flux-audio` (L0: no flux deps, no external deps): PCM16 LE/BE encode/decode,
      stateless `resample`, stateful streaming `Resampler` (phase carried across packets; integer
      up/down ratios), `Framer` (re-chunk to fixed frame sizes). Tests come with the crate
      (round-trips, ratio correctness, phase continuity across packet boundaries, framer
      remainders).
- [x] Registered in the workspace + `flux-codegate` layer map as L0; codegate test green.
- [x] `flux-core/src/audio.rs` module doc revised: formats = vocabulary here; sample math =
      `flux-audio`; the old "consumer's concern" sentence replaced with the pointer.
- [x] Full gate green; consumer-compat `cargo check` clean (purely additive — a new crate).

## Progress
- 2026-07-06 filed from the consumer review.
- 2026-07-07 implemented. Lifted the downstream consumer's audio crate into `crates/flux-audio`
  (23 tests: all originals ported plus new edge coverage — BE odd-trailing-byte, empty-input
  round-trips, exact-multiple framer boundary, zero-frame-size panic, never-pushed flush, one-
  sample-at-a-time streaming downsample phase, cross-packet upsample interpolation, non-integer-
  ratio fallback, and the PCM16-bytes convenience wrappers). Dropped two consumer-specific
  named-rate constants from the lifted source since that's vocabulary flux-core's `AudioFormat`
  already owns —
  flux-audio only does sample math, not naming specific rates. Registered in the workspace members
  + `workspace.dependencies` (path-only, no version — not in the `flux-sdk` publish closure) and
  classified L0 in `flux-codegate`. Revised `flux-core/src/audio.rs`'s module doc to point at
  `flux-audio` for sample math instead of punting to "the consumer's concern". Full gate green
  (`cargo build/test/clippy -D warnings/fmt --check` on both the root and `plugins/` workspaces);
  consumer-compat `cargo check --workspace` in the downstream consumer's repo stays clean
  (untouched, new crate is invisible to it).

## Notes
- Adoption story in the consumer's repo follows: replace its local audio crate with a dependency on
  flux-audio.

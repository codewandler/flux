// measure.js — the level probe's arithmetic, and the PCM16 decode it shares with the outbound
// track (D-232).
//
// This file is loaded **twice, deliberately**, and that is the whole reason it is a file rather
// than two copies of the same loop:
//
//   1. In the page, by `page.js`, applied to sample frames read out of a **real
//      `MediaStreamTrack`** via `AnalyserNode.getFloatTimeDomainData`. That is the measurement
//      D-208's level probe rests on, and the half no test in this repository can perform.
//   2. In Node, by `crates/flux-channels/tests/room_media_harness.rs`, applied to synthetic frames
//      whose correct answer is known analytically (a sine of amplitude `a` has RMS `a/√2`). That is
//      the half CI can check.
//
// Splitting it this way is what keeps the second test from being worthless. A test written against
// its own private copy of the arithmetic agrees with itself no matter what the shipped page does;
// this one fails when *the code the browser runs* is wrong, because it is the same bytes.
//
// What it deliberately does **not** do is decide anything. There is no floor, no threshold and no
// verdict here — `MediaPeer::verify_audible` owns the floor on the flux side, and a sidecar that
// computed its own pass/fail would be re-deciding the one question flux does not trust it with.

"use strict";

/// Root-mean-square and peak amplitude of one frame of `-1.0..=1.0` float samples.
///
/// `NaN` in, `NaN` out — on purpose. flux's floor check is written `rms > floor` rather than
/// `rms < floor` precisely so that an unmeasurable track is refused instead of waved through
/// (`NaN` compares false against everything), and sanitizing it to `0` here would only move the
/// same silence past a different check.
function frameLevel(samples) {
  let sum = 0;
  let peak = 0;
  for (let i = 0; i < samples.length; i++) {
    const s = samples[i];
    sum += s * s;
    const magnitude = Math.abs(s);
    if (magnitude > peak) peak = magnitude;
  }
  return { rms: Math.sqrt(sum / samples.length), peak };
}

/// The loudest frame across a probe window, as `{rms, peak}`.
///
/// Loudest rather than mean because of what the probe is *for*: it distinguishes a track carrying
/// signal from a track carrying silence, and averaging in the gaps between words would drag a
/// perfectly audible speaker toward the floor. Silence has no loud frames.
function windowLevel(frames) {
  let rms = 0;
  let peak = 0;
  for (const frame of frames) {
    const level = frameLevel(frame);
    // `>` leaves a NaN frame recorded as NaN only if it is the first; take it explicitly so an
    // unmeasurable frame anywhere in the window cannot be hidden by a louder neighbour.
    if (Number.isNaN(level.rms)) return { rms: NaN, peak: NaN };
    if (level.rms > rms) rms = level.rms;
    if (level.peak > peak) peak = level.peak;
  }
  return { rms, peak };
}

/// Decode base64 PCM16 little-endian into `-1.0..<1.0` floats.
///
/// The wire carries raw little-endian bytes rather than samples (`protocol.rs`'s `AudioChunk`), so
/// this is where they become audio. `/ 32768` rather than `/ 32767`: the range is asymmetric
/// (`-32768..=32767`) and dividing by the negative bound is what keeps a full-scale negative sample
/// from clipping past `-1.0`.
function pcm16ToFloat(base64, decodeBase64) {
  const bytes = decodeBase64(base64);
  const count = bytes.length >> 1;
  const out = new Float32Array(count);
  for (let i = 0; i < count; i++) {
    const lo = bytes[i * 2];
    const hi = bytes[i * 2 + 1];
    let value = lo | (hi << 8);
    if (value & 0x8000) value -= 0x10000;
    out[i] = value / 32768;
  }
  return out;
}

// Loaded as a plain script in the page (no module system) and as a CommonJS module under Node.
// Neither environment gets a bundler, so the export is written by hand for both.
if (typeof globalThis !== "undefined") {
  globalThis.FluxMeasure = { frameLevel, windowLevel, pcm16ToFloat };
}
if (typeof module !== "undefined" && module.exports) {
  module.exports = { frameLevel, windowLevel, pcm16ToFloat };
}

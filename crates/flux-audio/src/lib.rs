//! `flux-audio` — sample-rate math for realtime voice pipelines.
//!
//! Bridging a realtime voice pipeline almost always means moving audio between sample rates that
//! don't match — telephony's 8 kHz, WebRTC's 48 kHz, a device mic's 16 kHz, versus whatever a
//! model speaks natively — and re-chunking whatever the wire hands you (arbitrary packet sizes)
//! into whatever frame size the next stage expects. This crate is the sample-math layer under
//! that: byte⇄sample PCM16 conversion (both endiannesses — different transports pick different
//! conventions), a stateless [`resample`] function, a stateful streaming [`Resampler`] that
//! carries phase across packet boundaries (so re-chunked audio isn't lossy at the seams), and a
//! [`Framer`] for re-chunking a byte stream into fixed-size frames. Dependency-free and pure — no
//! IO, no async, no allocation beyond the `Vec`s it returns.
//!
//! Doctrine note: this crate owns sample *math* only. Audio *vocabulary* — naming which encoding,
//! rate, and channel count a stream uses — is `flux-core`'s `AudioFormat`/`AudioEncoding`. This
//! crate doesn't know or care what a given rate is used for; it is an optional leaf that voice-
//! capable consumers pull in directly, not a dependency of flux's core.

/// Decode little-endian PCM16 bytes into samples. A trailing odd byte is ignored.
pub fn pcm16_le_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Decode big-endian PCM16 bytes into samples. A trailing odd byte is ignored.
pub fn pcm16_be_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_be_bytes([c[0], c[1]]))
        .collect()
}

/// Encode samples into little-endian PCM16 bytes.
pub fn samples_to_pcm16_le(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Encode samples into big-endian PCM16 bytes.
pub fn samples_to_pcm16_be(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_be_bytes());
    }
    out
}

/// Linearly resample PCM16 samples from `from_hz` to `to_hz`.
///
/// Linear interpolation is adequate for voice at typical rates; a windowed-sinc filter would
/// improve downsampling anti-aliasing if needed. Returns the input unchanged when the rates match.
/// This is the stateless, whole-buffer variant — it treats `input` as a complete, isolated clip.
/// For audio arriving in successive packets, use the streaming [`Resampler`] instead, which
/// carries phase across calls so chunk boundaries don't drop or duplicate samples.
pub fn resample(input: &[i16], from_hz: u32, to_hz: u32) -> Vec<i16> {
    if from_hz == to_hz || input.is_empty() {
        return input.to_vec();
    }
    let out_len = (input.len() as u64 * to_hz as u64 / from_hz as u64) as usize;
    let ratio = from_hz as f64 / to_hz as f64;
    let last = input.len() - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = src.floor() as usize;
        let frac = src - idx as f64;
        let s0 = input[idx.min(last)] as f64;
        let s1 = input[(idx + 1).min(last)] as f64;
        out.push((s0 + (s1 - s0) * frac).round() as i16);
    }
    out
}

/// Convenience: resample little-endian PCM16 bytes from `from_hz` to `to_hz`.
pub fn resample_pcm16_le(bytes: &[u8], from_hz: u32, to_hz: u32) -> Vec<u8> {
    samples_to_pcm16_le(&resample(&pcm16_le_to_samples(bytes), from_hz, to_hz))
}

/// Re-chunks an arbitrary byte stream into fixed-size frames, buffering any partial tail.
#[derive(Debug)]
pub struct Framer {
    frame_size: usize,
    buf: Vec<u8>,
}

impl Framer {
    /// Create a framer that emits `frame_size`-byte frames.
    pub fn new(frame_size: usize) -> Self {
        assert!(frame_size > 0, "frame_size must be > 0");
        Self {
            frame_size,
            buf: Vec::new(),
        }
    }

    /// Append bytes and return any whole frames now available.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        while self.buf.len() >= self.frame_size {
            let rest = self.buf.split_off(self.frame_size);
            frames.push(std::mem::replace(&mut self.buf, rest));
        }
        frames
    }

    /// The buffered partial frame not yet emitted.
    pub fn remainder(&self) -> &[u8] {
        &self.buf
    }

    /// Take the remaining buffered bytes, padding to a full frame with `pad` if non-empty.
    pub fn flush(&mut self, pad: u8) -> Option<Vec<u8>> {
        if self.buf.is_empty() {
            return None;
        }
        let mut frame = std::mem::take(&mut self.buf);
        frame.resize(self.frame_size, pad);
        Some(frame)
    }
}

/// A **stateful streaming** resampler that carries phase/residual across calls, so re-chunked
/// audio (e.g. per network packet) is resampled without dropping samples at chunk boundaries.
///
/// Supports equal rates (passthrough) and exact integer up/down ratios (e.g. 8 kHz <-> 24 kHz is
/// a 3:1 ratio); for non-integer ratios it falls back to the stateless [`resample`] (per-chunk,
/// may drift at chunk boundaries).
pub struct Resampler {
    from: u32,
    to: u32,
    /// Previous input sample, for interpolation continuity when upsampling.
    prev: Option<i16>,
    /// Buffered input samples not yet consumed (< ratio), when downsampling.
    carry: Vec<i16>,
}

impl Resampler {
    /// Create a resampler from `from_hz` to `to_hz`.
    pub fn new(from_hz: u32, to_hz: u32) -> Self {
        Self {
            from: from_hz,
            to: to_hz,
            prev: None,
            carry: Vec::new(),
        }
    }

    /// Resample the next chunk of samples, carrying state for the following chunk.
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        if self.from == self.to {
            return input.to_vec();
        }
        if self.to > self.from && self.to.is_multiple_of(self.from) {
            return self.upsample(input, (self.to / self.from) as usize);
        }
        if self.from > self.to && self.from.is_multiple_of(self.to) {
            return self.downsample(input, (self.from / self.to) as usize);
        }
        // Non-integer ratio: stateless fallback (no cross-chunk continuity).
        resample(input, self.from, self.to)
    }

    /// Convenience: resample little-endian PCM16 bytes, carrying state across calls.
    pub fn process_pcm16_le(&mut self, bytes: &[u8]) -> Vec<u8> {
        samples_to_pcm16_le(&self.process(&pcm16_le_to_samples(bytes)))
    }

    fn upsample(&mut self, input: &[i16], factor: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(input.len() * factor);
        let mut prev = self
            .prev
            .unwrap_or_else(|| input.first().copied().unwrap_or(0));
        for &cur in input {
            for k in 1..=factor {
                let t = k as f64 / factor as f64;
                out.push((prev as f64 + (cur as f64 - prev as f64) * t).round() as i16);
            }
            prev = cur;
        }
        if !input.is_empty() {
            self.prev = Some(prev);
        }
        out
    }

    fn downsample(&mut self, input: &[i16], factor: usize) -> Vec<i16> {
        self.carry.extend_from_slice(input);
        let groups = self.carry.len() / factor;
        let mut out = Vec::with_capacity(groups);
        for g in 0..groups {
            let sum: i32 = self.carry[g * factor..(g + 1) * factor]
                .iter()
                .map(|&s| s as i32)
                .sum();
            out.push((sum / factor as i32) as i16);
        }
        self.carry.drain(0..groups * factor);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_round_trips_le() {
        let samples = vec![0i16, 1, -1, 32767, -32768, 12345];
        let bytes = samples_to_pcm16_le(&samples);
        assert_eq!(bytes.len(), samples.len() * 2);
        assert_eq!(pcm16_le_to_samples(&bytes), samples);
    }

    #[test]
    fn pcm16_round_trips_be() {
        let samples = vec![0i16, 256, -256, 32767, -32768];
        assert_eq!(pcm16_be_to_samples(&samples_to_pcm16_be(&samples)), samples);
    }

    #[test]
    fn odd_trailing_byte_is_ignored_le() {
        assert_eq!(pcm16_le_to_samples(&[0x01, 0x00, 0x7f]), vec![1]);
    }

    #[test]
    fn odd_trailing_byte_is_ignored_be() {
        assert_eq!(pcm16_be_to_samples(&[0x00, 0x01, 0x7f]), vec![1]);
    }

    #[test]
    fn empty_input_round_trips_to_empty() {
        assert!(pcm16_le_to_samples(&[]).is_empty());
        assert!(samples_to_pcm16_le(&[]).is_empty());
    }

    #[test]
    fn upsample_triples_length_at_3x_rate() {
        let input: Vec<i16> = (0..100).collect();
        let out = resample(&input, 8_000, 24_000);
        assert_eq!(out.len(), 300);
        // Endpoints are preserved.
        assert_eq!(out[0], 0);
    }

    #[test]
    fn downsample_thirds_length_at_3x_rate() {
        let input: Vec<i16> = (0..300).collect();
        let out = resample(&input, 24_000, 8_000);
        assert_eq!(out.len(), 100);
    }

    #[test]
    fn resample_is_identity_when_rates_match() {
        let input: Vec<i16> = vec![5, 6, 7, 8];
        assert_eq!(resample(&input, 8_000, 8_000), input);
    }

    #[test]
    fn resample_of_empty_input_is_empty() {
        assert!(resample(&[], 8_000, 24_000).is_empty());
    }

    #[test]
    fn linear_interpolation_midpoints() {
        // Upsampling a ramp by 3x: interpolated samples lie between neighbours.
        let input = vec![0i16, 30, 60];
        let out = resample(&input, 1, 3);
        assert_eq!(out.len(), 9);
        // monotonic non-decreasing for a ramp.
        assert!(out.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn resample_pcm16_le_round_trips_through_bytes() {
        let samples: Vec<i16> = (0..30).collect();
        let bytes = samples_to_pcm16_le(&samples);
        let out = resample_pcm16_le(&bytes, 8_000, 24_000);
        assert_eq!(out.len(), 90 * 2);
    }

    #[test]
    fn framer_emits_whole_frames_and_buffers_tail() {
        let mut f = Framer::new(4);
        assert!(f.push(&[1, 2, 3]).is_empty());
        assert_eq!(f.remainder(), &[1, 2, 3]);

        let frames = f.push(&[4, 5, 6, 7, 8]);
        assert_eq!(frames, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
        assert!(f.remainder().is_empty());
    }

    #[test]
    fn framer_exact_multiple_leaves_no_remainder() {
        let mut f = Framer::new(4);
        let frames = f.push(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(frames, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
        assert!(f.remainder().is_empty());
        assert_eq!(f.flush(0), None);
    }

    #[test]
    fn framer_flush_pads_partial_tail() {
        let mut f = Framer::new(4);
        f.push(&[9, 9]);
        assert_eq!(f.flush(0), Some(vec![9, 9, 0, 0]));
        assert_eq!(f.flush(0), None);
    }

    #[test]
    fn framer_flush_on_never_pushed_is_none() {
        let mut f = Framer::new(4);
        assert_eq!(f.flush(0), None);
    }

    #[test]
    #[should_panic(expected = "frame_size must be > 0")]
    fn framer_rejects_zero_frame_size() {
        Framer::new(0);
    }

    #[test]
    fn streaming_downsample_drops_no_samples_across_chunks() {
        // Two chunks not aligned to the 3:1 ratio: a stateless resampler would drop the tails.
        let mut r = Resampler::new(24_000, 8_000);
        let total_in = 10 + 11; // neither a multiple of 3
        let a = r.process(&(0..10).collect::<Vec<i16>>());
        let b = r.process(&(10..21).collect::<Vec<i16>>());
        // Every complete group of 3 input samples yields exactly one output; remainder carries over.
        assert_eq!(a.len() + b.len(), total_in / 3);
    }

    #[test]
    fn streaming_downsample_carries_phase_one_sample_at_a_time() {
        // Feed a 3:1 downsampler one sample per call: no output until the 3rd sample of each
        // group arrives, proving the carry buffer — not just chunk alignment — spans calls.
        let mut r = Resampler::new(24_000, 8_000);
        let input: Vec<i16> = vec![0, 30, 60, 90, 120, 150];
        let mut out = Vec::new();
        for &s in &input {
            out.extend(r.process(&[s]));
        }
        assert_eq!(out, vec![30, 120]);
    }

    #[test]
    fn streaming_upsample_triples_each_chunk() {
        let mut r = Resampler::new(8_000, 24_000);
        assert_eq!(r.process(&[0, 30]).len(), 6);
        assert_eq!(r.process(&[60]).len(), 3);
        // Continuity: the last emitted sample equals the last input sample.
        assert_eq!(*r.process(&[90]).last().unwrap(), 90);
    }

    #[test]
    fn streaming_upsample_interpolates_across_the_packet_boundary() {
        // The first output of a new chunk must interpolate from the *last* sample of the
        // previous chunk (phase carried via `prev`), not restart flat at the new chunk's first
        // sample — a stateless per-chunk resampler would produce a jump here instead.
        let mut r = Resampler::new(8_000, 24_000);
        let first = r.process(&[0, 60]);
        assert_eq!(first, vec![0, 0, 0, 20, 40, 60]);
        let second = r.process(&[90]); // continues interpolating 60 -> 90, not a flat 90,90,90
        assert_eq!(second, vec![70, 80, 90]);
    }

    #[test]
    fn resampler_passthrough_on_equal_rates() {
        let mut r = Resampler::new(8_000, 8_000);
        assert_eq!(r.process(&[1, 2, 3]), vec![1, 2, 3]);
    }

    #[test]
    fn resampler_falls_back_to_stateless_for_non_integer_ratio() {
        // 8000 -> 11025 is not an integer ratio in either direction.
        let mut r = Resampler::new(8_000, 11_025);
        let input: Vec<i16> = (0..50).collect();
        assert_eq!(r.process(&input), resample(&input, 8_000, 11_025));
    }

    #[test]
    fn process_pcm16_le_matches_process_on_samples() {
        let mut r = Resampler::new(8_000, 24_000);
        let samples: Vec<i16> = vec![0, 30, 60];
        let bytes = samples_to_pcm16_le(&samples);
        let via_bytes = r.process_pcm16_le(&bytes);

        let mut r2 = Resampler::new(8_000, 24_000);
        let via_samples = samples_to_pcm16_le(&r2.process(&samples));
        assert_eq!(via_bytes, via_samples);
    }
}

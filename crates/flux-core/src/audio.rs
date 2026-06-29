//! Audio vocabulary — IO-free value types describing audio formats for voice/realtime models.
//!
//! These mirror the role [`crate::ImageSource`] plays for images: pure descriptors that both the
//! provider layer and any consumer (a telephony or WebRTC channel) can name without inventing
//! duplicates. A realtime provider speaks its **model-native** format; resampling to a transport's
//! rate (telephony 8 kHz, WebRTC 48 kHz, a 16 kHz mic, …) is the consumer's concern, not flux's.

use serde::{Deserialize, Serialize};

/// How audio samples are encoded on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioEncoding {
    /// 16-bit signed little-endian PCM.
    Pcm16,
    /// G.711 µ-law (telephony).
    G711Ulaw,
    /// G.711 A-law (telephony).
    G711Alaw,
    /// Opus.
    Opus,
}

/// A concrete audio format: encoding + sample rate + channel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Sample encoding.
    pub encoding: AudioEncoding,
    /// Sample rate in Hz (e.g. `24_000` for OpenAI PCM16, `8_000` for telephony).
    pub sample_rate: u32,
    /// Channel count (`1` = mono).
    pub channels: u8,
}

impl AudioFormat {
    /// OpenAI Realtime's native PCM16 @ 24 kHz mono.
    pub const OPENAI_PCM16: Self = Self {
        encoding: AudioEncoding::Pcm16,
        sample_rate: 24_000,
        channels: 1,
    };

    /// G.711 µ-law @ 8 kHz mono (telephony) — a realtime model that accepts it resamples server-side,
    /// so a telephony consumer needs no client-side resampling.
    pub const TELEPHONY_ULAW: Self = Self {
        encoding: AudioEncoding::G711Ulaw,
        sample_rate: 8_000,
        channels: 1,
    };
}

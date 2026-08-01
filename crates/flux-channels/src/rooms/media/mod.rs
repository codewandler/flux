//! The [`MediaPeer`] seam — an optional, feature-gated WebRTC peer for a room (D-208).
//!
//! [`Room`](crate::rooms::Room) carries presence and text natively, with no browser and no vendor
//! SDK. Audio and screenshare cannot be carried that way: they need a real WebRTC endpoint — ICE,
//! DTLS-SRTP, simulcast, Jingle — and the honest options were to reimplement `lib-jitsi-meet`
//! against a vendor we do not control, or to let a browser do the part browsers are good at. This
//! module is the second choice, the [Jibri](https://github.com/jitsi/jibri) pattern: a **sidecar**
//! process owns the media stack and flux drives it over the plainest control protocol that works
//! ([`protocol`], one JSON object per line over the sidecar's stdin/stdout).
//!
//! ## Two peers, one room, and only one of them is required
//!
//! flux is in the room **twice** when media is on: once natively for text and presence, once
//! through the sidecar for media. They share an address and nothing else, and that separation is
//! the design rather than an accident of it:
//!
//! - **Text never depends on media.** No browser, no sidecar, no feature — the text half is
//!   unchanged and untouched. `crates/flux-channels/tests/rooms.rs::room_text_works_without_media_sidecar`
//!   is that claim, and it also asserts a declared-but-unbuildable sidecar is refused *by name*,
//!   because the way this breaks in practice is silent omission, not visible failure.
//! - **Media death is not room death.** A sidecar that never started, crashed, or wedged fails the
//!   *operation* — [`MediaPeer`] methods return `Err` — while the room stays joined and the meeting
//!   keeps going. `RoomChannel` logs it under the channel's name and carries on, the same posture it
//!   already takes for a failed delivery.
//!
//! ## What the spike measured, and why the shape follows from it
//!
//! A real Brave Talk call on 2026-07-30 got audio out and a human in the room confirmed hearing it.
//! Every documented way of getting there failed first, and the three findings are load-bearing here:
//!
//! 1. **Chrome 150 ignores `--use-fake-device-for-media-capture`.** Device labels stay real; even
//!    Chrome's own beep never appears (probe peak `0.0004` — silence). The standard Jibri/CI recipe
//!    is dead on this version.
//! 2. **Jitsi's `setAudioInputDevice` did not stick.** Called with the right label and deviceId,
//!    `getCurrentDevices()` kept reporting `Default`, and the published track stayed silent.
//! 3. **What worked** was a private PipeWire/Pulse null sink plus a remapped source, with *only our
//!    own* browser capture stream moved onto it (`pactl move-source-output <id> fluxagent_mic`).
//!    Per-stream, so the human's own microphone in the same call was never touched.
//!
//! Two consequences, and both are enforced rather than documented:
//!
//! - **The sidecar owns device routing, and has to say so.** (3) is Linux-specific, so it stays
//!   inside the sidecar and the control protocol names no device at all
//!   (`protocol::tests::the_protocol_never_names_a_capture_device`). What crosses the seam instead
//!   is a claim: [`Ready::owns_device_routing`](protocol::Ready::owns_device_routing), defaulting to
//!   `false`. flux refuses to publish audio through a sidecar that has not taken ownership, because
//!   (1) and (2) are both failures that *report success*.
//! - **"Unmuted" is not "audible".** The bridge elected the bot dominant speaker while the human
//!   heard nothing, so `dominantSpeakerChanged` proves nothing about signal. Publication is
//!   therefore checked with a **level probe** ([`MediaPeer::verify_audible`]) against
//!   [`DEFAULT_MIN_PUBLISH_RMS`], not with a mute-state read.
//!
//! ## Backpressure
//!
//! Inbound audio arrives roughly fifty times a second and never stops. [`MediaStream`] is bounded
//! and **sheds audio** past its capacity rather than growing: a queue of stale speech is worth less
//! than the memory it costs. Control events — `speech_started`, `participant` — are never shed;
//! [`MEDIA_CONTROL_RESERVE`] slots are kept back for them, because a barge-in that arrives late is a
//! bot talking over a person.

mod mock;
mod protocol;
mod sidecar;

pub use mock::MockMediaPeer;
pub use protocol::{
    AudioChunk, Level, MediaCommand, MediaEvent, MediaReply, MediaRequest, Ready, SidecarLine,
    VideoFrame, MEDIA_PROTOCOL,
};
pub use sidecar::{MediaSidecarConfig, SidecarMediaPeer};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use flux_core::{Error, Result};

use crate::rooms::{RoomId, RoomIdentity};

/// Inbound media events held in flight by default. Roughly five seconds of 20 ms audio frames —
/// enough to ride out a scheduling hiccup, not enough to accumulate a conversation nobody heard.
pub const DEFAULT_MEDIA_EVENT_BUFFER: usize = 256;

/// Slots [`MediaStream`] keeps back for non-audio events. An audio flood fills the buffer down to
/// this line and no further, so `speech_started` and `participant` still get through — the whole
/// reason the drop policy is per-event-kind rather than a flat `try_send`.
pub const MEDIA_CONTROL_RESERVE: usize = 32;

/// The RMS floor a published audio track must clear to count as audible.
///
/// Sits between the two values the 2026-07-30 call actually measured: **0.0004** peak for a
/// browser-default capture that reported healthy and carried silence, and **≈0.12** RMS for audio a
/// human in the room confirmed hearing. Two and a half orders of magnitude of headroom either way,
/// so this is a silence detector, not a loudness policy.
pub const DEFAULT_MIN_PUBLISH_RMS: f32 = 0.01;

/// How long to wait for a sidecar's `ready` handshake. A browser cold start is seconds.
pub const DEFAULT_MEDIA_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long any one control command may take before it is failed. Long enough for a join to
/// negotiate ICE; short enough that a wedged sidecar surfaces as an operation failure rather than as
/// a room that stopped responding.
pub const DEFAULT_MEDIA_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// A room's media plane: join, publish, probe, leave.
///
/// The port half of the sidecar, so the layers above it never learn what is behind it — the same
/// shape [`Room`](crate::rooms::Room) takes for text. [`SidecarMediaPeer`] is the real one;
/// [`MockMediaPeer`] is the in-process one D-209…D-211 can be built and tested against with no
/// browser.
///
/// **Implementing this grants no authority.** A media peer carries samples and identity; anything an
/// agent then *does* about them goes through the ordinary `Executor` + approver envelope.
#[async_trait]
pub trait MediaPeer: Send + Sync {
    /// Join `room` as `identity` and start receiving media events.
    async fn join(&self, room: &RoomId, identity: &RoomIdentity) -> Result<MediaStream>;

    /// Publish one chunk of audio onto the bot's outbound track.
    async fn publish_audio(&self, audio: &AudioChunk) -> Result<()>;

    /// Publish one video frame onto the bot's outbound track.
    async fn publish_video(&self, video: &VideoFrame) -> Result<()>;

    /// Mute or unmute the outbound audio track.
    async fn mute(&self, muted: bool) -> Result<()>;

    /// Measure the **published** track. The one call that can tell a live track from a healthy-
    /// looking silent one.
    async fn level(&self) -> Result<Level>;

    /// Leave. Idempotent.
    async fn leave(&self) -> Result<()>;

    /// Probe the published track and **fail if it carries no signal**.
    ///
    /// This is invariant 8 of the design, and it is a default method so every implementation
    /// inherits it rather than each one deciding whether silence counts as success. The spike is the
    /// argument: the bridge elected the bot dominant speaker, `mute` read false, the track was
    /// published, every status was green, and the human heard nothing. Call this after publishing
    /// starts — before a room full of people is the audience for the answer.
    async fn verify_audible(&self, floor: f32) -> Result<Level> {
        let level = self.level().await?;
        // `partial_cmp` rather than `>`, and the *absence* of `Greater` rather than the presence of
        // `Less`: an unmeasurable track (NaN) is incomparable, and incomparable must fail closed. A
        // plain `level.rms < floor` would wave it through.
        let carries_signal = matches!(
            level.rms.partial_cmp(&floor),
            Some(std::cmp::Ordering::Greater)
        );
        if !carries_signal {
            return Err(Error::Other(format!(
                "room media: the published track carries no signal (rms {:.5}, peak {:.5}, floor \
                 {floor:.5}) — it is published and unmuted and nobody can hear it; check that the \
                 sidecar routed its own capture device rather than taking the browser default",
                level.rms, level.peak
            )));
        }
        Ok(level)
    }
}

/// The stream of [`MediaEvent`]s a [`MediaPeer::join`] hands back.
///
/// Bounded, and **lossy on purpose** for audio — see [`MediaEventSender::send`]. An owned receiver
/// rather than a `Stream`, matching [`RoomStream`](crate::rooms::RoomStream): the producer is a task
/// reading a pipe and the consumer is a `select!` that also watches a cancellation token.
pub struct MediaStream {
    rx: mpsc::Receiver<MediaEvent>,
    dropped: Arc<AtomicU64>,
}

/// The producing half of a [`MediaStream`].
#[derive(Clone)]
pub struct MediaEventSender {
    tx: mpsc::Sender<MediaEvent>,
    reserve: usize,
    dropped: Arc<AtomicU64>,
}

/// What became of one [`MediaEventSender::send`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Queued for the consumer.
    Sent,
    /// Shed to keep the queue bounded. Only ever an audio frame.
    Dropped,
    /// The consumer is gone; the producer should stop reading its transport.
    Closed,
}

/// Create a media event channel with [`DEFAULT_MEDIA_EVENT_BUFFER`] slots.
pub fn media_event_channel() -> (MediaEventSender, MediaStream) {
    media_event_channel_with_capacity(DEFAULT_MEDIA_EVENT_BUFFER)
}

/// Create a media event channel with an explicit buffer size. The control reserve is clamped to
/// leave at least one audio slot, so a tiny buffer degrades to "audio almost always drops" rather
/// than to "audio never sends".
pub fn media_event_channel_with_capacity(capacity: usize) -> (MediaEventSender, MediaStream) {
    let capacity = capacity.max(2);
    let reserve = MEDIA_CONTROL_RESERVE.min(capacity - 1);
    let (tx, rx) = mpsc::channel(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        MediaEventSender {
            tx,
            reserve,
            dropped: dropped.clone(),
        },
        MediaStream { rx, dropped },
    )
}

impl MediaStream {
    /// The next event, or `None` once the producer is gone (the sidecar died, or left).
    pub async fn recv(&mut self) -> Option<MediaEvent> {
        self.rx.recv().await
    }

    /// How many inbound audio frames have been shed to keep this queue bounded, since the stream was
    /// created. Non-zero means flux is not keeping up — which is a diagnostic, not a failure.
    pub fn dropped_audio_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl MediaEventSender {
    /// Offer one event to the consumer. **Never waits**, and never grows the queue.
    ///
    /// Audio is shed once the queue is down to its last [`MEDIA_CONTROL_RESERVE`] slots; control
    /// events use the full buffer. Blocking here instead would push the backpressure onto the
    /// sidecar's pipe, which stalls the *outbound* half of the same protocol — a flux that is slow
    /// at reading audio would stop being able to speak, which is precisely backwards.
    pub fn send(&self, event: MediaEvent) -> Delivery {
        if event.is_droppable() && self.tx.capacity() <= self.reserve {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return Delivery::Dropped;
        }
        match self.tx.try_send(event) {
            Ok(()) => Delivery::Sent,
            Err(mpsc::error::TrySendError::Full(event)) => {
                if event.is_droppable() {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
                Delivery::Dropped
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Delivery::Closed,
        }
    }

    /// How many audio frames this channel has shed. Same counter [`MediaStream`] reports.
    pub fn dropped_audio_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::OccupantId;

    fn audio() -> MediaEvent {
        MediaEvent::AudioFrame {
            from: OccupantId::new("standup@x/timo"),
            audio: AudioChunk::mono(vec![0; 960], 48_000),
        }
    }

    #[tokio::test]
    async fn an_audio_flood_is_shed_and_never_starves_a_barge_in() {
        // The failure this exists to prevent is not "audio was dropped" — it is a room where flux
        // fell a second behind, grew a queue of speech nobody will hear in time, and then missed the
        // barge-in that was the only event that mattered.
        let (tx, mut stream) = media_event_channel_with_capacity(64);
        let mut sent = 0usize;
        for _ in 0..10_000 {
            match tx.send(audio()) {
                Delivery::Sent => sent += 1,
                Delivery::Dropped => {}
                Delivery::Closed => panic!("the consumer is alive"),
            }
        }
        assert!(
            sent <= 64 - MEDIA_CONTROL_RESERVE.min(63),
            "audio must stop at the control reserve, not at the buffer end: {sent} queued"
        );
        assert!(
            stream.dropped_audio_frames() >= 10_000 - sent as u64,
            "every shed frame is counted"
        );

        let barge_in = MediaEvent::SpeechStarted {
            from: OccupantId::new("standup@x/ada"),
        };
        assert_eq!(
            tx.send(barge_in.clone()),
            Delivery::Sent,
            "a control event must survive a flood of audio"
        );

        let mut drained = Vec::new();
        while let Ok(event) = stream.rx.try_recv() {
            drained.push(event);
        }
        assert_eq!(drained.len(), sent + 1, "nothing else was buffered");
        assert_eq!(
            drained.last(),
            Some(&barge_in),
            "the barge-in is still in the stream"
        );
    }

    #[tokio::test]
    async fn a_gone_consumer_is_reported_rather_than_counted_as_a_drop() {
        let (tx, stream) = media_event_channel_with_capacity(4);
        drop(stream);
        assert_eq!(tx.send(audio()), Delivery::Closed);
        assert_eq!(
            tx.dropped_audio_frames(),
            0,
            "a closed channel is the transport ending, not backpressure"
        );
    }

    /// A peer whose publish and mute always succeed and whose probe reports whatever it was given —
    /// the exact shape of the failure the spike hit.
    struct ScriptedLevel(Level);

    #[async_trait]
    impl MediaPeer for ScriptedLevel {
        async fn join(&self, _room: &RoomId, _identity: &RoomIdentity) -> Result<MediaStream> {
            Ok(media_event_channel().1)
        }
        async fn publish_audio(&self, _audio: &AudioChunk) -> Result<()> {
            Ok(())
        }
        async fn publish_video(&self, _video: &VideoFrame) -> Result<()> {
            Ok(())
        }
        async fn mute(&self, _muted: bool) -> Result<()> {
            Ok(())
        }
        async fn level(&self) -> Result<Level> {
            Ok(self.0)
        }
        async fn leave(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_published_track_that_carries_silence_is_a_failure_not_a_success() {
        // The measured silence: publish said ok, mute said false, the bridge elected the bot
        // dominant speaker, and the probe read peak 0.0004.
        let silent = ScriptedLevel(Level {
            rms: 0.0002,
            peak: 0.0004,
        });
        assert!(
            silent
                .publish_audio(&AudioChunk::mono(vec![0; 4], 48_000))
                .await
                .is_ok(),
            "the failure mode under test is one where every ordinary call succeeds"
        );
        let error = silent
            .verify_audible(DEFAULT_MIN_PUBLISH_RMS)
            .await
            .expect_err("silence must not read as success")
            .to_string();
        assert!(error.contains("no signal"), "{error}");
        assert!(
            error.contains("0.00040"),
            "the measurement is in the message: {error}"
        );

        // The measured audible: rms ≈ 0.12.
        let audible = ScriptedLevel(Level {
            rms: 0.12,
            peak: 0.31,
        });
        assert_eq!(
            audible
                .verify_audible(DEFAULT_MIN_PUBLISH_RMS)
                .await
                .expect("0.12 is what a human confirmed hearing")
                .rms,
            0.12
        );
    }

    #[tokio::test]
    async fn a_nan_probe_is_silence_rather_than_a_pass() {
        // `rms > floor` rather than `rms < floor` in the guard: NaN compares false against
        // everything, so the negated form is the one that refuses an unmeasurable track instead of
        // waving it through.
        let broken = ScriptedLevel(Level {
            rms: f32::NAN,
            peak: f32::NAN,
        });
        assert!(broken
            .verify_audible(DEFAULT_MIN_PUBLISH_RMS)
            .await
            .is_err());
    }
}

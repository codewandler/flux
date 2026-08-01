//! [`MockMediaPeer`] — the in-process [`MediaPeer`], for the same reason [`MockRoom`] exists.
//!
//! D-209 (audio in), D-210 (audio out) and D-211 (screenshare) all land on this seam, and none of
//! them should need a browser, a bridge or a room full of people to be testable. This peer records
//! what flux published, replays scripted inbound events, and — the part that matters — lets a test
//! script the *level probe* independently of whether publishing succeeded, because that gap is
//! exactly the failure the 2026-07-30 spike hit.
//!
//! [`MockRoom`]: crate::rooms::MockRoom

use std::sync::Mutex;

use async_trait::async_trait;

use flux_core::{Error, Result};

use super::protocol::{AudioChunk, Level, MediaEvent, VideoFrame};
use super::{
    media_event_channel_with_capacity, Delivery, MediaEventSender, MediaPeer, MediaStream,
    DEFAULT_MEDIA_EVENT_BUFFER,
};
use crate::rooms::{RoomId, RoomIdentity};

/// An in-process media peer: a recording of everything flux published and a scriptable inbound
/// stream.
pub struct MockMediaPeer {
    level: Mutex<Level>,
    owns_device_routing: bool,
    event_buffer: usize,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    joined: Option<(RoomId, String)>,
    published_audio: Vec<AudioChunk>,
    published_video: Vec<VideoFrame>,
    muted: Option<bool>,
    left: bool,
    /// Held so a test can `emit` after joining; dropped by `leave`, which ends the stream.
    sender: Option<MediaEventSender>,
    /// When set, every command fails with it — a sidecar that died mid-call.
    dead: Option<String>,
}

impl Default for MockMediaPeer {
    fn default() -> Self {
        Self::new()
    }
}

impl MockMediaPeer {
    /// A peer that owns its device routing and publishes an audible track (rms 0.12 — what the
    /// spike measured for audio a human confirmed hearing).
    pub fn new() -> Self {
        Self {
            level: Mutex::new(Level {
                rms: 0.12,
                peak: 0.31,
            }),
            owns_device_routing: true,
            event_buffer: DEFAULT_MEDIA_EVENT_BUFFER,
            state: Mutex::new(State::default()),
        }
    }

    /// Report `level` from the probe, whatever publishing did. The silent-but-healthy case is
    /// `Level { rms: 0.0002, peak: 0.0004 }`.
    pub fn with_level(self, level: Level) -> Self {
        *self.level.lock().unwrap() = level;
        self
    }

    /// A sidecar that has *not* taken ownership of its capture device, so audio publication is
    /// refused rather than silently silent.
    pub fn without_device_routing(mut self) -> Self {
        self.owns_device_routing = false;
        self
    }

    /// Hold fewer inbound events, so a backpressure test does not have to send hundreds.
    pub fn with_event_buffer(mut self, events: usize) -> Self {
        self.event_buffer = events;
        self
    }

    /// Kill the peer: every later command fails with `reason`, and the event stream ends. What a
    /// crashed sidecar looks like from above.
    pub fn die(&self, reason: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        state.dead = Some(reason.into());
        state.sender = None;
    }

    /// Deliver one inbound media event. Returns how the bounded stream handled it, so a test can
    /// observe shedding rather than infer it.
    pub fn emit(&self, event: MediaEvent) -> Delivery {
        let sender = self.state.lock().unwrap().sender.clone();
        match sender {
            Some(sender) => sender.send(event),
            None => Delivery::Closed,
        }
    }

    /// Every audio chunk flux published, in order.
    pub fn published_audio(&self) -> Vec<AudioChunk> {
        self.state.lock().unwrap().published_audio.clone()
    }

    /// Every video frame flux published, in order.
    pub fn published_video(&self) -> Vec<VideoFrame> {
        self.state.lock().unwrap().published_video.clone()
    }

    /// The last mute state flux set, or `None` if it never set one.
    pub fn muted(&self) -> Option<bool> {
        self.state.lock().unwrap().muted
    }

    /// The room and nick flux joined as, if it joined.
    pub fn joined(&self) -> Option<(RoomId, String)> {
        self.state.lock().unwrap().joined.clone()
    }

    /// Whether flux left.
    pub fn has_left(&self) -> bool {
        self.state.lock().unwrap().left
    }

    fn alive(&self) -> Result<()> {
        match self.state.lock().unwrap().dead.clone() {
            Some(reason) => Err(Error::Other(format!("room media: {reason}"))),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl MediaPeer for MockMediaPeer {
    async fn join(&self, room: &RoomId, identity: &RoomIdentity) -> Result<MediaStream> {
        self.alive()?;
        let (sender, stream) = media_event_channel_with_capacity(self.event_buffer);
        let mut state = self.state.lock().unwrap();
        state.joined = Some((room.clone(), identity.nick.clone()));
        state.left = false;
        state.sender = Some(sender);
        Ok(stream)
    }

    async fn publish_audio(&self, audio: &AudioChunk) -> Result<()> {
        self.alive()?;
        if !self.owns_device_routing {
            return Err(Error::Other(
                "room media: this sidecar does not claim to own device routing".into(),
            ));
        }
        self.state
            .lock()
            .unwrap()
            .published_audio
            .push(audio.clone());
        Ok(())
    }

    async fn publish_video(&self, video: &VideoFrame) -> Result<()> {
        self.alive()?;
        self.state
            .lock()
            .unwrap()
            .published_video
            .push(video.clone());
        Ok(())
    }

    async fn mute(&self, muted: bool) -> Result<()> {
        self.alive()?;
        self.state.lock().unwrap().muted = Some(muted);
        Ok(())
    }

    async fn level(&self) -> Result<Level> {
        self.alive()?;
        Ok(*self.level.lock().unwrap())
    }

    async fn leave(&self) -> Result<()> {
        self.alive()?;
        let mut state = self.state.lock().unwrap();
        state.left = true;
        state.sender = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::media::DEFAULT_MIN_PUBLISH_RMS;
    use crate::rooms::OccupantId;

    #[tokio::test]
    async fn a_dead_peer_fails_every_operation_and_ends_its_stream() {
        let peer = MockMediaPeer::new();
        let mut stream = peer
            .join(&RoomId::new("standup@x"), &RoomIdentity::agent("flux"))
            .await
            .unwrap();
        peer.die("the sidecar exited with status 1");

        let error = peer
            .publish_audio(&AudioChunk::mono(vec![0; 4], 48_000))
            .await
            .expect_err("a dead sidecar cannot publish")
            .to_string();
        assert!(error.contains("exited with status 1"), "{error}");
        assert!(peer.level().await.is_err());
        assert_eq!(stream.recv().await, None, "a dead sidecar delivers nothing");
    }

    #[tokio::test]
    async fn a_peer_that_does_not_own_routing_refuses_audio_before_it_is_silent() {
        let peer = MockMediaPeer::new().without_device_routing();
        peer.join(&RoomId::new("standup@x"), &RoomIdentity::agent("flux"))
            .await
            .unwrap();
        assert!(peer
            .publish_audio(&AudioChunk::mono(vec![0; 4], 48_000))
            .await
            .is_err());
        assert!(
            peer.published_audio().is_empty(),
            "nothing reached the track"
        );
    }

    #[tokio::test]
    async fn the_probe_is_scriptable_independently_of_publishing() {
        // The spike's shape: every ordinary call succeeds and the track carries nothing.
        let peer = MockMediaPeer::new().with_level(Level {
            rms: 0.0002,
            peak: 0.0004,
        });
        peer.join(&RoomId::new("standup@x"), &RoomIdentity::agent("flux"))
            .await
            .unwrap();
        peer.publish_audio(&AudioChunk::mono(vec![0; 4], 48_000))
            .await
            .expect("publishing reports success — that is the problem");
        peer.mute(false).await.unwrap();
        assert_eq!(peer.muted(), Some(false));
        assert!(peer.verify_audible(DEFAULT_MIN_PUBLISH_RMS).await.is_err());
    }

    #[tokio::test]
    async fn scripted_inbound_events_reach_the_stream() {
        let peer = MockMediaPeer::new();
        let mut stream = peer
            .join(&RoomId::new("standup@x"), &RoomIdentity::agent("flux"))
            .await
            .unwrap();
        let event = MediaEvent::SpeechStarted {
            from: OccupantId::new("standup@x/timo"),
        };
        assert_eq!(peer.emit(event.clone()), Delivery::Sent);
        assert_eq!(stream.recv().await, Some(event));
        assert_eq!(peer.joined().unwrap().1, "flux");
    }
}

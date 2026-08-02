//! `flux.room-media.v1` — the wire the [media sidecar](super) speaks (D-208).
//!
//! **One JSON object per line, UTF-8, `\n`-terminated, in both directions.** Nothing here is
//! streamed, chunked or framed by length: a line is a message. That is the entire transport, and it
//! is deliberately the plainest thing that works, because the process on the other end is a browser
//! harness written in whatever language its author reaches for.
//!
//! ## flux → sidecar (stdin)
//!
//! Every command carries a monotonically increasing `id` and is answered by exactly one reply
//! carrying that `id`. Replies may arrive out of order; events may be interleaved with them.
//!
//! ```text
//! {"id":1,"cmd":"join","room":"standup@conference.example.org","nick":"flux","kind":"agent"}
//! {"id":2,"cmd":"publish_audio","audio":{"pcm16_le":"<base64>","sample_rate_hz":48000,"channels":1}}
//! {"id":3,"cmd":"publish_video","video":{"rgba":"<base64>","width":1280,"height":720}}
//! {"id":4,"cmd":"mute","muted":true}
//! {"id":5,"cmd":"level"}
//! {"id":6,"cmd":"leave"}
//! ```
//!
//! ## sidecar → flux (stdout)
//!
//! One handshake line first, then replies and events in any order:
//!
//! ```text
//! {"ready":"flux.room-media.v1","owns_device_routing":true}
//! {"ready":"flux.room-media.v1","owns_device_routing":false,"routing_error":"audio server … is unreachable"}
//! {"id":1,"ok":true}
//! {"id":5,"ok":true,"level":{"rms":0.12,"peak":0.31}}
//! {"id":2,"ok":false,"error":"no published audio track"}
//! {"event":"audio_frame","from":"standup@…/timo","audio":{"pcm16_le":"<base64>","sample_rate_hz":48000,"channels":1}}
//! {"event":"speech_started","from":"standup@…/timo"}
//! {"event":"participant","occupant":"standup@…/timo","nick":"timo","kind":"human","present":true}
//! ```
//!
//! **Anything else on stdout is skipped and counted, never fatal.** A browser harness prints. A line
//! that is not valid JSON, or valid JSON that is not one of the three shapes above, is noise —
//! killing a live call over a stray log line would be the worse failure. Only a malformed line that
//! *claims* to be protocol (a `{"event":…}` whose body will not deserialize) is surfaced as a
//! diagnostic, and even that only counts.
//!
//! ## What this protocol deliberately cannot say
//!
//! There is **no device, sink, source or audio-server field anywhere in it**, and there never should
//! be. What made audio work on 2026-07-30 was a private PipeWire null sink plus a per-stream
//! `pactl move-source-output`, and that is Linux — naming it here would make the port Linux-only for
//! everyone. The sidecar owns device routing and says so once, in the handshake
//! ([`Ready::owns_device_routing`]); flux refuses to publish through one that does not claim it,
//! because a browser-default capture on Chrome 150 is silent and reports success.

use serde::{Deserialize, Serialize};

use crate::rooms::{OccupantId, OccupantKind, RoomId};

/// The protocol identifier a sidecar must announce in its handshake. A mismatch is refused rather
/// than negotiated: the sidecar is an out-of-tree program, and "probably compatible" is exactly the
/// judgement that puts a silent bot in a room full of people.
pub const MEDIA_PROTOCOL: &str = "flux.room-media.v1";

/// Base64 for the PCM/RGBA payloads. Standard alphabet with padding — the spelling every language a
/// sidecar might be written in produces by default.
mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serialize as _, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        base64::engine::general_purpose::STANDARD
            .encode(bytes)
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

fn mono() -> u16 {
    1
}

/// A chunk of linear PCM16 audio crossing the seam in either direction.
///
/// Raw **little-endian bytes**, not samples: this crate carries the chunk, it does not do sample
/// math. `flux-audio` is the crate that converts and resamples, and D-209/D-210 are where that
/// belongs — 48 kHz is what WebRTC hands a browser and 24 kHz is what a realtime model wants, and
/// neither of those facts is this protocol's business.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioChunk {
    /// PCM16 little-endian bytes, base64 on the wire.
    #[serde(with = "b64")]
    pub pcm16_le: Vec<u8>,
    /// The chunk's sample rate. Stated per chunk rather than negotiated once, so a sidecar that
    /// changes rate mid-call cannot silently corrupt the stream.
    pub sample_rate_hz: u32,
    /// Interleaved channel count. Defaults to mono, which is what a voice track is.
    #[serde(default = "mono")]
    pub channels: u16,
}

impl AudioChunk {
    /// A mono chunk at `sample_rate_hz`.
    pub fn mono(pcm16_le: impl Into<Vec<u8>>, sample_rate_hz: u32) -> Self {
        Self {
            pcm16_le: pcm16_le.into(),
            sample_rate_hz,
            channels: 1,
        }
    }
}

/// One frame of video flux publishes as the bot's camera/screenshare track.
///
/// Straight RGBA rather than an encoded format: the sidecar draws it into a canvas whose
/// `captureStream()` is the published track. Headless has no display, so `getDisplayMedia` is not
/// the path (measured — see the design); a canvas source needs neither a display nor a capture
/// permission, and D-211 is where the drawing lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFrame {
    /// Row-major RGBA8 bytes, `width * height * 4` of them, base64 on the wire.
    #[serde(with = "b64")]
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A control command flux sends the sidecar. Serialized as `{"id":…,"cmd":"…",…}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum MediaCommand {
    /// Join `room` as `nick`. The sidecar is a *second* peer in the same room the text backend is
    /// already in — flux joins twice, once natively for text and presence and once through the
    /// browser for media, because the two halves have nothing in common but the address.
    Join {
        room: RoomId,
        nick: String,
        kind: OccupantKind,
    },
    /// Leave. Idempotent; leaving a room the sidecar is not in is not an error.
    Leave,
    /// Publish one chunk of audio onto the bot's outbound track.
    PublishAudio { audio: AudioChunk },
    /// Publish one video frame onto the bot's outbound track.
    PublishVideo { video: VideoFrame },
    /// Mute or unmute the outbound audio track. **Not** a substitute for a level probe: the spike
    /// watched the bridge elect an unmuted, silent bot dominant speaker.
    Mute { muted: bool },
    /// Measure the *published* track and report it. The one question that distinguishes "unmuted"
    /// from "audible", and the reason [`Level`] exists.
    Level,
}

/// A command with its correlation id, as it goes onto the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaRequest {
    pub id: u64,
    #[serde(flatten)]
    pub command: MediaCommand,
}

impl MediaRequest {
    /// One NDJSON line, newline included.
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }
}

/// The sidecar's opening line, and the only place it makes a claim about itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ready {
    /// Must equal [`MEDIA_PROTOCOL`].
    pub ready: String,
    /// Whether the sidecar routes its own capture device rather than taking the browser's default.
    ///
    /// **Absent means `false`, and `false` means flux refuses to publish audio.** Chrome 150 ignores
    /// `--use-fake-device-for-media-capture` and Jitsi's `setAudioInputDevice` does not stick
    /// (both measured 2026-07-30), so a sidecar that has not taken ownership of routing will publish
    /// a track that reports healthy and carries silence. Defaulting this to `false` is what makes an
    /// older or naive sidecar fail loudly instead of quietly.
    #[serde(default)]
    pub owns_device_routing: bool,
    /// Why device routing could not be established. A sidecar should name a masked/missing socket
    /// here so flux refuses publication with the cause rather than with an unexplained zero level.
    #[serde(default)]
    pub routing_error: Option<String>,
}

/// A published track's measured signal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Root-mean-square amplitude over the probe window, `0.0..=1.0`. The 2026-07-30 call measured
    /// **≈0.12** for audio a human confirmed hearing.
    pub rms: f32,
    /// Peak amplitude over the same window. Silence measured **0.0004** — that is what a
    /// browser-default capture on Chrome 150 looks like, and it looked like success everywhere else.
    #[serde(default)]
    pub peak: f32,
}

/// The sidecar's answer to one command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaReply {
    pub id: u64,
    #[serde(default)]
    pub ok: bool,
    /// Why it failed. Present when `ok` is false; a failure with no reason still fails.
    #[serde(default)]
    pub error: Option<String>,
    /// The measurement, for a [`MediaCommand::Level`].
    #[serde(default)]
    pub level: Option<Level>,
}

/// Something that happened in the room's media plane.
///
/// `#[non_exhaustive]`: D-209…D-211 land consumers for these, and a fourth variant (a
/// dominant-speaker signal, say) must not force a consumer to change. Note that a dominant-speaker
/// signal would be exactly that — a signal, not evidence of audio; see [`Level`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MediaEvent {
    /// Inbound audio from one occupant. **Droppable**: this is the variant that arrives fifty times
    /// a second, and the one [`MediaStream`](super::MediaStream) sheds when flux falls behind.
    AudioFrame { from: OccupantId, audio: AudioChunk },
    /// An occupant started speaking — the barge-in signal `flux_flow::voice` already knows what to
    /// do with. Never dropped for backpressure: it is rare, it is cheap, and a barge-in that
    /// arrives late is a bot talking over a person.
    SpeechStarted { from: OccupantId },
    /// A participant appeared in or disappeared from the media plane. Distinct from the text
    /// backend's presence: a person can be in the MUC without a camera or a microphone.
    Participant {
        occupant: OccupantId,
        #[serde(default)]
        nick: String,
        #[serde(default)]
        kind: OccupantKind,
        present: bool,
    },
}

impl MediaEvent {
    /// The occupant this event is about. Every variant has one — unlike [`RoomEvent`], the media
    /// plane has no lifecycle terminator of its own; the stream simply ends.
    ///
    /// [`RoomEvent`]: crate::rooms::RoomEvent
    pub fn occupant(&self) -> &OccupantId {
        match self {
            MediaEvent::AudioFrame { from, .. } | MediaEvent::SpeechStarted { from } => from,
            MediaEvent::Participant { occupant, .. } => occupant,
        }
    }

    /// Whether this event may be shed under backpressure. Only inbound audio may: stale speech is
    /// worth less than the memory it costs, and everything else here is a control fact.
    pub fn is_droppable(&self) -> bool {
        matches!(self, MediaEvent::AudioFrame { .. })
    }
}

/// One line the sidecar wrote, once recognized.
#[derive(Debug, Clone, PartialEq)]
pub enum SidecarLine {
    Ready(Ready),
    Reply(MediaReply),
    Event(MediaEvent),
}

impl SidecarLine {
    /// Classify and parse one line of the sidecar's stdout.
    ///
    /// Three outcomes, and the distinction between the last two is the whole point:
    ///
    /// - `Ok(Some(line))` — a protocol line.
    /// - `Ok(None)` — **not protocol at all**: blank, not JSON, not an object, or an object with
    ///   none of the three discriminating keys. A browser harness writes to stdout; this is that.
    ///   Skip it.
    /// - `Err(_)` — a line that *claims* to be protocol and is malformed. Worth counting and
    ///   surfacing, still not worth ending a live call over.
    pub fn parse(line: &str) -> Result<Option<Self>, serde_json::Error> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // Not JSON — a log line, a stack trace, a banner. Noise, not a protocol violation.
            Err(_) => return Ok(None),
        };
        let Some(object) = value.as_object() else {
            return Ok(None);
        };
        // Keyed dispatch rather than `#[serde(untagged)]`: untagged resolves by trying each variant
        // in order and taking the first that fits, which turns a typo'd field into a *different
        // message* instead of an error. Here a line either names its shape or is not ours.
        if object.contains_key("ready") {
            serde_json::from_value(value).map(|r| Some(SidecarLine::Ready(r)))
        } else if object.contains_key("id") {
            serde_json::from_value(value).map(|r| Some(SidecarLine::Reply(r)))
        } else if object.contains_key("event") {
            serde_json::from_value(value).map(|e| Some(SidecarLine::Event(e)))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_round_trips_through_its_ndjson_line() {
        let request = MediaRequest {
            id: 7,
            command: MediaCommand::PublishAudio {
                audio: AudioChunk::mono(vec![0x01, 0x02, 0x03, 0x04], 48_000),
            },
        };
        let line = request.to_line().unwrap();
        assert!(line.ends_with('\n'), "one line, newline-terminated");
        assert!(
            !line.trim_end().contains('\n'),
            "a command never spans two lines: {line}"
        );
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["cmd"], "publish_audio");
        assert_eq!(value["audio"]["sample_rate_hz"], 48_000);
        assert_eq!(value["audio"]["pcm16_le"], "AQIDBA==");
        assert_eq!(
            serde_json::from_str::<MediaRequest>(line.trim()).unwrap(),
            request
        );
    }

    #[test]
    fn the_protocol_never_names_a_capture_device() {
        // The portability contract, asserted on the rendered wire rather than on the doc comment:
        // the virtual-device recipe is Linux-specific and lives inside the sidecar. If a field for
        // it ever appears here, this fails and the reviewer has to justify making the port
        // Linux-only.
        let commands = [
            MediaCommand::Join {
                room: RoomId::new("standup@conference.example.org"),
                nick: "flux".into(),
                kind: OccupantKind::Agent,
            },
            MediaCommand::Leave,
            MediaCommand::PublishAudio {
                audio: AudioChunk::mono(vec![0; 4], 48_000),
            },
            MediaCommand::PublishVideo {
                video: VideoFrame {
                    rgba: vec![0; 4],
                    width: 1,
                    height: 1,
                },
            },
            MediaCommand::Mute { muted: false },
            MediaCommand::Level,
        ];
        for command in commands {
            let rendered = serde_json::to_string(&MediaRequest { id: 1, command }).unwrap();
            for banned in [
                "device", "sink", "source", "pulse", "pipewire", "pactl", "alsa",
            ] {
                assert!(
                    !rendered.to_ascii_lowercase().contains(banned),
                    "the control protocol must not name `{banned}`: {rendered}"
                );
            }
        }
    }

    #[test]
    fn stdout_noise_is_skipped_and_only_malformed_protocol_is_an_error() {
        for noise in [
            "",
            "   ",
            "[1234:5678:0801/…:ERROR:bus.cc(399)] Failed to connect to the bus",
            "{\"level\":\"info\",\"msg\":\"chrome started\"}",
            "[1,2,3]",
            "\"a bare string\"",
        ] {
            assert_eq!(
                SidecarLine::parse(noise).unwrap(),
                None,
                "a browser harness prints; that is not a protocol violation: {noise}"
            );
        }
        // Claims to be an event, and is not one.
        assert!(SidecarLine::parse("{\"event\":\"audio_frame\"}").is_err());
        assert!(SidecarLine::parse("{\"event\":\"telepathy\"}").is_err());
    }

    #[test]
    fn each_protocol_shape_parses_as_itself() {
        assert_eq!(
            SidecarLine::parse("{\"ready\":\"flux.room-media.v1\",\"owns_device_routing\":true}")
                .unwrap(),
            Some(SidecarLine::Ready(Ready {
                ready: MEDIA_PROTOCOL.into(),
                owns_device_routing: true,
                routing_error: None,
            }))
        );
        // An omitted claim is not a claim: the default is the refusing one.
        assert_eq!(
            SidecarLine::parse("{\"ready\":\"flux.room-media.v1\"}").unwrap(),
            Some(SidecarLine::Ready(Ready {
                ready: MEDIA_PROTOCOL.into(),
                owns_device_routing: false,
                routing_error: None,
            }))
        );
        assert_eq!(
            SidecarLine::parse("{\"id\":5,\"ok\":true,\"level\":{\"rms\":0.12,\"peak\":0.31}}")
                .unwrap(),
            Some(SidecarLine::Reply(MediaReply {
                id: 5,
                ok: true,
                error: None,
                level: Some(Level {
                    rms: 0.12,
                    peak: 0.31
                }),
            }))
        );
        assert_eq!(
            SidecarLine::parse("{\"event\":\"speech_started\",\"from\":\"standup@x/timo\"}")
                .unwrap(),
            Some(SidecarLine::Event(MediaEvent::SpeechStarted {
                from: OccupantId::new("standup@x/timo"),
            }))
        );
        let participant = SidecarLine::parse(
            "{\"event\":\"participant\",\"occupant\":\"standup@x/timo\",\"present\":true}",
        )
        .unwrap();
        assert_eq!(
            participant,
            Some(SidecarLine::Event(MediaEvent::Participant {
                occupant: OccupantId::new("standup@x/timo"),
                nick: String::new(),
                kind: OccupantKind::Unknown,
                present: true,
            })),
            "a sidecar that cannot tell what a participant is must not be read as saying `human`"
        );
    }

    #[test]
    fn only_inbound_audio_is_shed_under_backpressure() {
        let from = OccupantId::new("standup@x/timo");
        assert!(MediaEvent::AudioFrame {
            from: from.clone(),
            audio: AudioChunk::mono(vec![0; 2], 48_000),
        }
        .is_droppable());
        assert!(!MediaEvent::SpeechStarted { from: from.clone() }.is_droppable());
        assert!(!MediaEvent::Participant {
            occupant: from,
            nick: "timo".into(),
            kind: OccupantKind::Human,
            present: false,
        }
        .is_droppable());
    }
}

//! D-208 — the room media sidecar, driven over its real NDJSON wire with no browser anywhere.
//!
//! Every test here runs the actual [`SidecarMediaPeer`] against an in-process double connected by a
//! pair of `tokio::io::duplex` pipes, so the bytes, the correlation ids, the handshake and the drop
//! policy are the shipping ones — the same posture `xmpp_room.rs` takes with its WebSocket double.
//! What is *not* proven here is stated plainly in the story and the design: nothing in this file
//! opens a WebRTC session, and no test can tell you whether a real browser publishes audible audio.
//! That is what the level probe and the preflight runbook are for.
//!
//! Behind `--features room-media`; `scripts/check-feature-gated-tests.sh` is what runs it in CI.
#![cfg(feature = "room-media")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use flux_app::JourneyRun;
use flux_channels::rooms::media::{
    AudioChunk, Level, MediaCommand, MediaEvent, MediaPeer, MediaReply, MediaRequest,
    MediaSidecarConfig, Ready, SidecarMediaPeer, VideoFrame, DEFAULT_MIN_PUBLISH_RMS,
    MEDIA_PROTOCOL,
};
use flux_channels::rooms::{MessageScope, MockRoom, Occupant, OccupantId, OccupantKind, RoomEvent};
use flux_channels::rooms::{RoomId, RoomIdentity};
use flux_channels::{Channel, Deliverer, RoomChannel};
use flux_lang::program::ChannelDecl;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ── the in-process sidecar double ───────────────────────────────────────────────────────────────

/// What the double writes to flux next.
enum Wire {
    /// A raw NDJSON line.
    Line(String),
    /// Close stdout — what a crashed sidecar looks like from flux's side.
    Hangup,
}

/// A scripted sidecar: records every command it was sent, answers each one through a closure, and
/// can push unsolicited events or hang up at any moment.
struct Double {
    received: Arc<Mutex<Vec<MediaRequest>>>,
    wire: mpsc::UnboundedSender<Wire>,
}

impl Double {
    /// The commands flux has sent, in order.
    fn received(&self) -> Vec<MediaRequest> {
        self.received.lock().unwrap().clone()
    }

    /// The `cmd` names flux has sent, in order.
    fn commands(&self) -> Vec<String> {
        self.received()
            .iter()
            .map(|r| {
                serde_json::to_value(&r.command).unwrap()["cmd"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    /// Push one unsolicited event onto the wire.
    fn emit(&self, event: &MediaEvent) {
        let _ = self
            .wire
            .send(Wire::Line(serde_json::to_string(event).unwrap()));
    }

    /// Push a raw line — for the noise a browser harness writes onto its own stdout.
    fn emit_raw(&self, line: &str) {
        let _ = self.wire.send(Wire::Line(line.to_string()));
    }

    /// Close stdout. Everything waiting on a reply fails immediately.
    fn hangup(&self) {
        let _ = self.wire.send(Wire::Hangup);
    }
}

/// Connect a real [`SidecarMediaPeer`] to a scripted double.
///
/// `handshake` is the opening line (`None` never announces anything); `answer` turns each command
/// into a reply line, and `None` from it means "never answer this one".
async fn connect(
    handshake: Option<String>,
    answer: impl Fn(&MediaRequest) -> Option<String> + Send + Sync + 'static,
    config: MediaSidecarConfig,
) -> (flux_core::Result<SidecarMediaPeer>, Arc<Double>) {
    // Two pipes: flux reads the sidecar's stdout, the sidecar reads flux's stdin.
    let (flux_reads, sidecar_writes) = tokio::io::duplex(64 * 1024);
    let (sidecar_reads, flux_writes) = tokio::io::duplex(64 * 1024);

    let (wire_tx, mut wire_rx) = mpsc::unbounded_channel::<Wire>();
    let received: Arc<Mutex<Vec<MediaRequest>>> = Default::default();

    // The writer half: the one place bytes reach flux, so replies and events can never interleave.
    tokio::spawn(async move {
        let mut out = sidecar_writes;
        while let Some(item) = wire_rx.recv().await {
            match item {
                Wire::Line(mut line) => {
                    line.push('\n');
                    if out.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = out.flush().await;
                }
                // Dropping the writer is the EOF flux sees.
                Wire::Hangup => return,
            }
        }
    });

    if let Some(handshake) = handshake {
        let _ = wire_tx.send(Wire::Line(handshake));
    }

    // The reader half: parse each command, record it, answer it.
    let answers_to = wire_tx.clone();
    let recorded = received.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(sidecar_reads).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: MediaRequest =
                serde_json::from_str(&line).expect("flux wrote a well-formed command line");
            recorded.lock().unwrap().push(request.clone());
            if let Some(reply) = answer(&request) {
                if answers_to.send(Wire::Line(reply)).is_err() {
                    return;
                }
            }
        }
    });

    let peer = SidecarMediaPeer::connect(flux_reads, flux_writes, config).await;
    (
        peer,
        Arc::new(Double {
            received,
            wire: wire_tx,
        }),
    )
}

fn ok(id: u64) -> String {
    serde_json::to_string(&MediaReply {
        id,
        ok: true,
        error: None,
        level: None,
    })
    .unwrap()
}

fn ready(owns_device_routing: bool) -> String {
    serde_json::to_string(&Ready {
        ready: MEDIA_PROTOCOL.into(),
        owns_device_routing,
        routing_error: None,
    })
    .unwrap()
}

fn config() -> MediaSidecarConfig {
    let mut config = MediaSidecarConfig::new(vec!["/nonexistent/flux-room-media".into()]);
    // Short, so a wedged double fails the suite in a second rather than stalling it.
    config.handshake_timeout = Duration::from_secs(2);
    config.command_timeout = Duration::from_secs(2);
    config
}

fn chunk() -> AudioChunk {
    AudioChunk::mono(vec![0x11, 0x22, 0x33, 0x44], 48_000)
}

// ── the seam ────────────────────────────────────────────────────────────────────────────────────

/// The whole control surface the story names — `join` / `leave`, `publish_audio`, `publish_video`,
/// `mute` — over the real wire, asserted on the bytes flux actually emitted rather than on the Rust
/// calls that produced them.
#[tokio::test]
async fn the_control_protocol_round_trips_over_the_real_wire() {
    let (peer, double) = connect(
        Some(ready(true)),
        |request| {
            Some(match &request.command {
                // The probe answers with a measurement; everything else with a bare ack.
                MediaCommand::Level => serde_json::to_string(&MediaReply {
                    id: request.id,
                    ok: true,
                    error: None,
                    level: Some(Level {
                        rms: 0.12,
                        peak: 0.31,
                    }),
                })
                .unwrap(),
                _ => ok(request.id),
            })
        },
        config(),
    )
    .await;
    let peer = peer.expect("the handshake completes");
    assert!(peer.owns_device_routing());

    let mut stream = peer
        .join(
            &RoomId::new("standup@conference.example.org"),
            &RoomIdentity::agent("flux"),
        )
        .await
        .expect("join");
    peer.publish_audio(&chunk()).await.expect("publish_audio");
    peer.publish_video(&VideoFrame {
        rgba: vec![0xff; 4],
        width: 1,
        height: 1,
    })
    .await
    .expect("publish_video");
    peer.mute(true).await.expect("mute");
    let level = peer
        .verify_publish_carries_signal()
        .await
        .expect("rms 0.12 is what a human confirmed hearing");
    assert_eq!(level.rms, 0.12);
    peer.leave().await.expect("leave");

    assert_eq!(
        double.commands(),
        vec![
            "join",
            "publish_audio",
            "publish_video",
            "mute",
            "level",
            "leave"
        ]
    );
    // The join names the room as the *server* spells it and the nick flux joined text under — one
    // room, two peers.
    match &double.received()[0].command {
        MediaCommand::Join { room, nick, kind } => {
            assert_eq!(room.as_str(), "standup@conference.example.org");
            assert_eq!(nick, "flux");
            assert_eq!(*kind, OccupantKind::Agent);
        }
        other => panic!("expected a join, got {other:?}"),
    }
    // Correlation ids are distinct and ascending — a reply belongs to exactly one command.
    let ids: Vec<u64> = double.received().iter().map(|r| r.id).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");

    // The three inbound events the story names, over the same wire.
    let timo = OccupantId::new("standup@conference.example.org/timo");
    let inbound = [
        MediaEvent::Participant {
            occupant: timo.clone(),
            nick: "timo".into(),
            kind: OccupantKind::Human,
            present: true,
        },
        MediaEvent::SpeechStarted { from: timo.clone() },
        MediaEvent::AudioFrame {
            from: timo.clone(),
            audio: chunk(),
        },
    ];
    for event in &inbound {
        double.emit(event);
    }
    for expected in &inbound {
        assert_eq!(stream.recv().await.as_ref(), Some(expected));
    }
}

/// A browser harness prints to stdout. Killing a live call over a log line would be the worse
/// failure, so noise is counted and skipped — and a line that *claims* to be protocol and is
/// malformed is counted too, not fatal.
#[tokio::test]
async fn stdout_noise_from_the_browser_never_ends_the_session() {
    let (peer, double) = connect(Some(ready(true)), |r| Some(ok(r.id)), config()).await;
    let peer = peer.expect("the handshake completes");

    double.emit_raw("[0801/120000.1:ERROR:bus.cc(399)] Failed to connect to the bus");
    double.emit_raw("{\"event\":\"telepathy\"}");
    double.emit_raw("not json at all");
    // A command after the noise still works, which is the actual assertion.
    peer.mute(false).await.expect("the session survived it");
    assert!(peer.death_reason().is_none());
    assert!(
        peer.unrecognized_lines() >= 3,
        "the noise was counted: {}",
        peer.unrecognized_lines()
    );
}

// ── device routing, and the difference between unmuted and audible ──────────────────────────────

/// The 2026-07-30 finding, enforced instead of documented: a sidecar that has not taken ownership
/// of its capture device is refused **before** it publishes a track that reads healthy and carries
/// silence.
#[tokio::test]
async fn a_sidecar_that_does_not_own_device_routing_may_not_publish_audio() {
    let (peer, double) = connect(Some(ready(false)), |r| Some(ok(r.id)), config()).await;
    let peer =
        peer.expect("the handshake completes — the refusal is about publishing, not joining");
    assert!(!peer.owns_device_routing());

    let error = peer
        .publish_audio(&chunk())
        .await
        .expect_err("a browser-default capture is silent on Chrome 150")
        .to_string();
    assert!(error.contains("device routing"), "{error}");
    assert!(
        error.contains("Chrome 150"),
        "names the measurement: {error}"
    );
    assert!(
        double.commands().is_empty(),
        "nothing reached the wire: {:?}",
        double.commands()
    );

    // Video is unaffected — a canvas-sourced track needs no capture device at all (D-211).
    peer.publish_video(&VideoFrame {
        rgba: vec![0; 4],
        width: 1,
        height: 1,
    })
    .await
    .expect("video does not depend on audio capture");
}

/// D-235: preserve the sidecar's concrete routing failure through the handshake and publication
/// refusal. A masked Pulse socket must not collapse into generic "device routing" or a zero level.
#[tokio::test]
async fn a_masked_audio_socket_survives_as_the_publication_failure_reason() {
    let handshake = serde_json::to_string(&Ready {
        ready: MEDIA_PROTOCOL.into(),
        owns_device_routing: false,
        routing_error: Some(
            "audio server /run/user/1000/pulse/native is unreachable; flux's sandbox masks `/run`; \
             add [sandbox] writable = [\"/run/user/1000/pulse\"]"
                .into(),
        ),
    })
    .unwrap();
    let (peer, double) = connect(Some(handshake), |r| Some(ok(r.id)), config()).await;
    let peer = peer.expect("a diagnosed routing failure still completes the control handshake");

    let error = peer
        .publish_audio(&chunk())
        .await
        .expect_err("the masked socket must stop publication")
        .to_string();
    assert!(error.contains("/run/user/1000/pulse/native"), "{error}");
    assert!(error.contains("sandbox masks `/run`"), "{error}");
    assert!(error.contains("[sandbox] writable"), "{error}");
    assert!(
        double.commands().is_empty(),
        "publication was refused before a silent track reached the wire"
    );
}

/// Invariant 8 of the design, end to end over the wire: publish succeeds, mute reads false, and the
/// probe says the track is silent — so publication is a **failure**.
#[tokio::test]
async fn a_published_track_that_carries_silence_fails_the_probe() {
    let (peer, _double) = connect(
        Some(ready(true)),
        |request| {
            Some(match &request.command {
                MediaCommand::Level => serde_json::to_string(&MediaReply {
                    id: request.id,
                    ok: true,
                    error: None,
                    // What the spike measured with no device routing: silence, reported healthy.
                    level: Some(Level {
                        rms: 0.0002,
                        peak: 0.0004,
                    }),
                })
                .unwrap(),
                _ => ok(request.id),
            })
        },
        config(),
    )
    .await;
    let peer = peer.expect("the handshake completes");

    peer.publish_audio(&chunk())
        .await
        .expect("publishing reports success — that is exactly the trap");
    peer.mute(false).await.expect("and it is not muted");
    let error = peer
        .verify_publish_carries_signal()
        .await
        .expect_err("`unmuted` is not `audible`")
        .to_string();
    assert!(error.contains("no signal"), "{error}");
    assert!(
        error.contains("0.00040"),
        "the measurement is quoted: {error}"
    );
}

/// A probe with no measurement is not a pass. A sidecar that answers `ok` and says nothing about the
/// level has told flux nothing, and "nothing" must not read as "fine".
#[tokio::test]
async fn a_level_reply_with_no_measurement_is_not_a_pass() {
    let (peer, _double) = connect(Some(ready(true)), |r| Some(ok(r.id)), config()).await;
    let peer = peer.expect("the handshake completes");
    let error = peer
        .verify_publish_carries_signal()
        .await
        .expect_err("an unmeasured track is not an audible one")
        .to_string();
    assert!(error.contains("without a measurement"), "{error}");
}

/// The handshake is refused rather than negotiated. A sidecar built against a different protocol is
/// a peer that joins a room full of people and behaves in a way nobody predicted.
#[tokio::test]
async fn a_protocol_mismatch_is_refused_rather_than_negotiated() {
    let mismatched = serde_json::to_string(&Ready {
        ready: "flux.room-media.v9".into(),
        owns_device_routing: true,
        routing_error: None,
    })
    .unwrap();
    let (peer, _double) = connect(Some(mismatched), |r| Some(ok(r.id)), config()).await;
    let error = peer.expect_err("v9 is not v1").to_string();
    assert!(error.contains("flux.room-media.v9"), "{error}");
    assert!(error.contains(MEDIA_PROTOCOL), "{error}");
}

/// A sidecar that never announces itself is a failed *operation*, at the configured timeout.
#[tokio::test]
async fn a_sidecar_that_never_announces_itself_fails_the_handshake() {
    let (peer, _double) = connect(None, |r| Some(ok(r.id)), config()).await;
    let error = peer.expect_err("no handshake, no peer").to_string();
    assert!(error.contains("did not announce"), "{error}");
    assert!(error.contains(MEDIA_PROTOCOL), "{error}");
}

// ── backpressure ────────────────────────────────────────────────────────────────────────────────

/// Inbound audio **drops** rather than growing without limit, and a barge-in still gets through.
///
/// The failure this prevents is not "audio was lost". It is a flux that fell a second behind, grew a
/// queue of speech nobody will hear in time, and then missed the `speech_started` that was the only
/// event that mattered.
#[tokio::test]
async fn inbound_audio_drops_rather_than_growing_without_limit() {
    let mut config = config();
    config.event_buffer = 64;
    let (peer, double) = connect(Some(ready(true)), |r| Some(ok(r.id)), config).await;
    let peer = peer.expect("the handshake completes");
    let mut stream = peer
        .join(&RoomId::new("standup@x"), &RoomIdentity::agent("flux"))
        .await
        .expect("join");

    let timo = OccupantId::new("standup@x/timo");
    let frame = MediaEvent::AudioFrame {
        from: timo.clone(),
        // 20 ms of 48 kHz mono PCM16 — a real frame, so the memory the queue would grow is real too.
        audio: AudioChunk::mono(vec![0u8; 1920], 48_000),
    };
    // Nothing reads `stream` during this loop: flux is "behind" for the whole of it.
    for _ in 0..2_000 {
        double.emit(&frame);
    }
    let barge_in = MediaEvent::SpeechStarted { from: timo };
    double.emit(&barge_in);

    // Let the reader task consume the whole pipe **before** anything reads the stream — that is what
    // "flux is behind" means, and draining concurrently would keep freeing slots and hide the bound.
    let mut settled = 0;
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let dropped = stream.dropped_audio_frames();
        if dropped == settled && dropped > 0 {
            break;
        }
        settled = dropped;
    }

    let mut drained = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), stream.recv()).await
    {
        drained.push(event);
    }

    assert!(
        drained.len() <= 64,
        "2001 events were sent into a 64-slot queue and it never grew past it: {} held",
        drained.len()
    );
    assert!(
        stream.dropped_audio_frames() > 1_000,
        "the shed frames are counted: {}",
        stream.dropped_audio_frames()
    );
    assert!(
        drained.contains(&barge_in),
        "the barge-in survived a flood of audio: {drained:?}"
    );
    assert!(
        drained.iter().filter(|e| *e == &barge_in).count() == 1
            && drained.len() > 1
            && drained
                .iter()
                .any(|e| matches!(e, MediaEvent::AudioFrame { .. })),
        "audio was shed, not switched off"
    );
}

// ── death is survivable ─────────────────────────────────────────────────────────────────────────

/// A sidecar that dies mid-call fails the *operation*, immediately and with a reason, and never
/// waits out a timeout against a process that will not answer.
#[tokio::test]
async fn a_sidecar_that_dies_mid_call_fails_its_operations_with_a_reason() {
    let (peer, double) = connect(
        Some(ready(true)),
        // Answers `join` and nothing after it — the sidecar is about to die holding a command.
        |request| match &request.command {
            MediaCommand::Join { .. } => Some(ok(request.id)),
            _ => None,
        },
        config(),
    )
    .await;
    let peer = peer.expect("the handshake completes");
    let mut stream = peer
        .join(&RoomId::new("standup@x"), &RoomIdentity::agent("flux"))
        .await
        .expect("join");

    // A command in flight when stdout closes.
    let publishing = {
        let hangup = double.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            hangup.hangup();
        });
        peer.publish_audio(&chunk()).await
    };
    let error = publishing
        .expect_err("a dead sidecar cannot publish")
        .to_string();
    assert!(error.contains("went away"), "{error}");

    // And every command afterwards fails immediately with the recorded reason rather than waiting.
    let started = std::time::Instant::now();
    let later = peer.mute(false).await.expect_err("still dead").to_string();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "a known-dead sidecar must not be waited on: took {:?}",
        started.elapsed()
    );
    assert!(later.contains("closed its stdout"), "{later}");
    assert!(peer.death_reason().is_some());
    assert_eq!(stream.recv().await, None, "the media stream ends");
}

/// **The acceptance item that matters most before a demo.** A declared sidecar that cannot start
/// does not take the meeting with it: the room stays joined, the addressed turn is answered, and the
/// channel returns `Ok` — `flux_channels::serve` ends the whole process on a channel `Err`, so this
/// is the difference between "no audio" and "the agent left".
///
/// Nothing is mocked out of the spawn path: `/nonexistent/flux-room-media` really is run through
/// `flux_system::System`, and really does fail.
#[tokio::test]
async fn a_media_sidecar_that_cannot_start_leaves_text_and_presence_running() {
    struct Answering;
    #[async_trait]
    impl Deliverer for Answering {
        async fn deliver(&self, _label: &str, _payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
            Ok(vec![JourneyRun {
                journey: "answer".into(),
                result: "still here".into(),
                steps: 1,
                usage: None,
                model: "mock".into(),
            }])
        }
    }

    let timo = Occupant::new("standup@rooms.example/timo", "timo", OccupantKind::Human);
    let room = Arc::new(
        MockRoom::new("standup@rooms.example")
            .with_occupant(timo.clone())
            .script(vec![
                RoomEvent::Message {
                    from: timo.id.clone(),
                    text: "flux: are you there?".into(),
                    scope: MessageScope::Groupchat,
                },
                RoomEvent::Ended,
            ]),
    );
    let channel = RoomChannel::with_room(
        &ChannelDecl {
            name: "standup".into(),
            kind: "room".into(),
            settings: json!({
                "backend": "mock",
                "room": "standup@rooms.example",
                "media": {
                    "sidecar": ["/nonexistent/flux-room-media"],
                    "handshake_timeout_secs": 1,
                },
            }),
        },
        room.clone(),
    )
    .expect("the declaration is valid — the program just is not there");
    assert!(channel.media().is_some(), "media really was opted into");

    let d: Arc<dyn Deliverer> = Arc::new(Answering);
    channel
        .start(d, CancellationToken::new())
        .await
        .expect("a dead sidecar is a media outage, never a channel error");

    assert_eq!(
        room.said(),
        vec!["still here".to_string()],
        "text kept working with the media half dead"
    );
    assert!(room.has_left(), "and the session ended cleanly");
}

// ── the declaration ─────────────────────────────────────────────────────────────────────────────

/// The `media { … }` block is validated at **load**, not at join: an empty argv is a typo, and
/// finding it out when the meeting starts is finding it out too late.
#[test]
fn a_defective_media_declaration_fails_the_load() {
    let build = |media: Value| {
        RoomChannel::from_decl(&ChannelDecl {
            name: "standup".into(),
            kind: "room".into(),
            settings: json!({
                "backend": "mock",
                "room": "standup@rooms.example",
                "media": media,
            }),
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
    };

    let error = build(json!({ "sidecar": [] })).expect_err("an empty argv runs nothing");
    assert!(error.contains("standup"), "names the channel: {error}");
    assert!(error.contains("media.sidecar"), "names the field: {error}");

    let error = build(json!({ "sidecar": ["  "] })).expect_err("a blank program runs nothing");
    assert!(error.contains("media.sidecar[0]"), "{error}");

    let error = build(json!({ "sidecar": ["x"], "min_publish_rms": -1.0 }))
        .expect_err("a negative floor is a probe that passes everything");
    assert!(error.contains("min_publish_rms"), "{error}");

    // And the ordinary case builds, carrying its defaults.
    let channel = RoomChannel::from_decl(&ChannelDecl {
        name: "standup".into(),
        kind: "room".into(),
        settings: json!({
            "backend": "mock",
            "room": "standup@rooms.example",
            "media": { "sidecar": ["flux-room-media", "--audio-server", "unix:/run/user/1000/pulse/native"] },
        }),
    })
    .expect("a well-formed media declaration builds");
    let media = channel.media().expect("opted in");
    assert_eq!(media.argv.len(), 3, "argv reaches the sidecar verbatim");
    assert_eq!(media.min_publish_rms, DEFAULT_MIN_PUBLISH_RMS);
}

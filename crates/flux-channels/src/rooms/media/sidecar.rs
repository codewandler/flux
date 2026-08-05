//! [`SidecarMediaPeer`] — the real [`MediaPeer`], driving an out-of-process browser harness over
//! [`protocol`](super::protocol) NDJSON (D-208).
//!
//! ## How the process is started, and what it does *not* get
//!
//! Through [`System::spawn_interactive`], the same seam the plugin host uses: **argv-only** (no
//! shell, `argv[0]` is the program), workspace-pinned cwd, `kill_on_drop`, and an environment
//! **cleared** to flux's minimal non-secret allow-list. That last one has a consequence an operator
//! has to know about, because it is invisible until the sidecar is silent:
//!
//! > `DISPLAY`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, `PULSE_SERVER` and `PULSE_RUNTIME_PATH` **do
//! > not reach the sidecar.** Anything it needs to know about the host's audio server must be passed
//! > in its argv, which flux treats as opaque and never interprets. `HOME`, `USER`, `PATH` and
//! > `TMPDIR` do survive, so a sidecar can derive the conventional `/run/user/<uid>/pulse/native`
//! > for itself.
//!
//! That is not a workaround for the env-clearing rule; it is the rule working. A media sidecar is a
//! subprocess like any other, and flux does not hand subprocesses the host's environment.
//!
//! ## Argv is necessary and not sufficient (D-235)
//!
//! ⚠ The paragraph above is the whole recipe only when the sandbox is off. Under flux's Linux
//! confinement there is a **second, required** step, and omitting it fails silently.
//!
//! `flux-system` mounts a tmpfs over `/run` on every sandboxed spawn — deliberately, to keep
//! `docker.sock`, D-Bus and other host IPC sockets unreachable even when the network namespace is
//! shared. The PulseAudio/PipeWire socket lives at `/run/user/<uid>/pulse/native`, under that mask.
//! So argv can name the audio server *correctly* and still reach nothing: no value of
//! `--audio-server` can point at a path the confinement removed.
//!
//! The operator must therefore also grant the socket's directory back:
//!
//! ```toml
//! # flux.toml — required for host audio, not optional. `id -u` gives the uid.
//! [sandbox]
//! enabled = true
//! writable = ["/run/user/1000/pulse"]
//! ```
//!
//! `writable` is the right mechanism despite its name. It emits a read-write bwrap `--bind`, and
//! read-write is what a unix socket needs — `connect(2)` takes `MAY_WRITE` on the socket inode, so
//! a read-only bind would leave the socket visible and unconnectable. The bind is emitted *after*
//! the `/run` tmpfs, so it re-exposes the host directory through the mask rather than being erased
//! by it. Grant the directory that holds the socket, not the socket file itself.
//!
//! Two things flux does to keep a mistake here loud rather than silent:
//!
//! - A configured `writable` path under `/run` that does not exist is **refused at startup**
//!   instead of being created. An empty directory bound over the mask would apply cleanly and still
//!   reach nothing, and a wrong uid is the common typo.
//! - A sidecar that could not reach its audio server reports
//!   [`Ready::routing_error`](super::protocol::Ready::routing_error), which
//!   [`SidecarMediaPeer::publish_audio`] quotes in its refusal. The failure names the socket; it
//!   does not arrive as a level probe reading zero.
//!
//! ## Failure posture
//!
//! Every failure here is an **operation** failure. A sidecar that would not start, died mid-call, or
//! stopped answering fails the [`MediaPeer`] call and nothing else — the room stays joined, text and
//! presence keep flowing, and the caller decides what to do about the missing audio. Once the
//! sidecar's stdout reaches EOF the peer is permanently dead: pending commands fail, later ones fail
//! immediately with the recorded reason instead of waiting out a timeout against a process that will
//! never answer, and the media stream ends. There is no reconnect here on purpose — rejoining a live
//! call is a decision with a room full of people in it, and it belongs to whoever owns the session,
//! not to a transport.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use flux_core::{Error, Result};
use flux_system::System;

use super::protocol::{
    AudioChunk, Level, MediaCommand, MediaReply, MediaRequest, Ready, SidecarLine, VideoFrame,
    MEDIA_PROTOCOL,
};
use super::{
    media_event_channel_with_capacity, MediaEventSender, MediaPeer, MediaStream,
    DEFAULT_MEDIA_COMMAND_TIMEOUT, DEFAULT_MEDIA_EVENT_BUFFER, DEFAULT_MEDIA_HANDSHAKE_TIMEOUT,
    DEFAULT_MIN_PUBLISH_RMS,
};
use crate::config::MediaSettings;
use crate::rooms::{RoomId, RoomIdentity};

/// Everything flux needs to know about a media sidecar. Note what is not here: no device, no audio
/// server, no display. Those are the sidecar's, and they ride in [`argv`](Self::argv) — which is
/// necessary but not sufficient for a host socket under `/run`, whose matching `[sandbox] writable`
/// grant the module header describes.
///
/// `Debug` is **hand-written**, for the reason `WebhookSettings` gives: a channel declaration's
/// values are host-resolved before this struct is built, so an operator who wrote
/// `sidecar ["…", secret "ROOM_TOKEN"]` has a plaintext credential sitting in `argv`. The derive
/// would print it the first time anyone adds a trace line. The program is kept — "which sidecar" is
/// the diagnosable part — and everything after it is counted, not shown.
#[derive(Clone, PartialEq)]
pub struct MediaSidecarConfig {
    /// The sidecar's argv, `argv[0]` first. Opaque to flux beyond being non-empty.
    pub argv: Vec<String>,
    /// How long to wait for the `ready` handshake.
    pub handshake_timeout: Duration,
    /// How long any one command may take.
    pub command_timeout: Duration,
    /// Inbound media events held in flight before audio is shed.
    pub event_buffer: usize,
    /// The RMS floor [`SidecarMediaPeer::verify_publish_carries_signal`] is held to.
    pub min_publish_rms: f32,
}

impl std::fmt::Debug for MediaSidecarConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSidecarConfig")
            .field("program", &self.argv.first())
            .field(
                "args",
                &format_args!("{} <redacted>", self.argv.len().saturating_sub(1)),
            )
            .field("handshake_timeout", &self.handshake_timeout)
            .field("command_timeout", &self.command_timeout)
            .field("event_buffer", &self.event_buffer)
            .field("min_publish_rms", &self.min_publish_rms)
            .finish()
    }
}

impl MediaSidecarConfig {
    /// A sidecar at `argv` with every default.
    pub fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            handshake_timeout: DEFAULT_MEDIA_HANDSHAKE_TIMEOUT,
            command_timeout: DEFAULT_MEDIA_COMMAND_TIMEOUT,
            event_buffer: DEFAULT_MEDIA_EVENT_BUFFER,
            min_publish_rms: DEFAULT_MIN_PUBLISH_RMS,
        }
    }

    /// Read the declaration's `media { … }` block. Fails the **load** rather than the join: an empty
    /// argv is a typo, and discovering it when the meeting starts is discovering it too late.
    pub fn from_settings(settings: &MediaSettings) -> std::result::Result<Self, String> {
        match settings.sidecar.first() {
            None => {
                return Err(
                    "`media.sidecar` needs the sidecar's argv, starting with the program".into(),
                )
            }
            Some(program) if program.trim().is_empty() => {
                return Err("`media.sidecar[0]` is the program to run and must not be empty".into())
            }
            Some(_) => {}
        }
        let mut config = Self::new(settings.sidecar.clone());
        if let Some(secs) = settings.handshake_timeout_secs {
            config.handshake_timeout = Duration::from_secs(secs);
        }
        if let Some(secs) = settings.command_timeout_secs {
            config.command_timeout = Duration::from_secs(secs);
        }
        if let Some(buffer) = settings.event_buffer {
            config.event_buffer = buffer;
        }
        if let Some(floor) = settings.min_publish_rms {
            // NaN is called out rather than left to `<`, which is false against everything: a floor
            // nothing can be compared against is a probe that passes silence.
            if floor.is_nan() || floor < 0.0 {
                return Err(format!(
                    "`media.min_publish_rms` must be a non-negative number, got {floor}"
                ));
            }
            config.min_publish_rms = floor;
        }
        Ok(config)
    }
}

/// State the reader task and the command path share. One `Arc`, created before the reader starts, so
/// there is exactly one pending-command table and exactly one place the sidecar's death is recorded.
#[derive(Default)]
struct Shared {
    /// Command id → the waiter for its reply. Dropping a sender is how a dead sidecar releases a
    /// waiter immediately rather than at the timeout.
    pending: Mutex<HashMap<u64, oneshot::Sender<MediaReply>>>,
    /// Set once, when the sidecar is gone for good.
    death: Mutex<Option<String>>,
    /// Lines on stdout that were not protocol. Browser harnesses print; this is a diagnostic.
    unrecognized: AtomicU64,
}

/// A [`MediaPeer`] backed by a sidecar process speaking [`MEDIA_PROTOCOL`].
pub struct SidecarMediaPeer {
    config: MediaSidecarConfig,
    shared: Arc<Shared>,
    writer: tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    next_id: AtomicU64,
    /// The sidecar's own claim that it routes its capture device. Refusing to publish audio without
    /// it is what stops a browser-default (and therefore silent) track from reaching a real room.
    owns_device_routing: bool,
    /// The sidecar's explanation for a `false` claim, when it has one (D-235). Kept so a masked
    /// audio socket is reported as a confinement/configuration failure rather than as silence.
    routing_error: Option<String>,
    /// Handed out once, by [`MediaPeer::join`]. The reader task starts at connect — the handshake
    /// needs it — so the stream exists before anyone has joined.
    stream: Mutex<Option<MediaStream>>,
    /// Kept alive for exactly as long as the peer is: `spawn_interactive` sets `kill_on_drop`, so
    /// dropping the peer stops the browser.
    _child: Option<tokio::process::Child>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for SidecarMediaPeer {
    /// The three facts worth reading in a failure: which sidecar, whether it owns its capture
    /// device, and whether it is still alive. The argv beyond the program stays redacted for the
    /// reason [`MediaSidecarConfig`]'s own `Debug` gives.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarMediaPeer")
            .field("config", &self.config)
            .field("owns_device_routing", &self.owns_device_routing)
            .field("routing_error", &self.routing_error)
            .field("death_reason", &self.death_reason())
            .finish()
    }
}

impl SidecarMediaPeer {
    /// Spawn the sidecar and complete its handshake.
    ///
    /// The spawn goes through the one guarded path every flux subprocess takes. See this module's
    /// header for what the child's environment does and does not contain — it is the detail most
    /// likely to make an otherwise-correct sidecar silent.
    pub async fn spawn(system: &System, config: MediaSidecarConfig) -> Result<Self> {
        let flux_system::InteractiveChild {
            child,
            stdin,
            stdout,
        } = system.spawn_interactive(&config.argv).map_err(|e| {
            Error::Other(format!(
                "room media: the sidecar `{}` did not start: {e}",
                config.argv.first().map(String::as_str).unwrap_or("")
            ))
        })?;
        Self::connect_inner(stdout, stdin, config, Some(child)).await
    }

    /// Drive a sidecar over an already-open pair of streams, without spawning anything.
    ///
    /// The seam a test scripts a sidecar through — the same posture `xmpp_room.rs` takes with its
    /// in-process WebSocket double, and the reason the whole protocol is exercisable in CI with no
    /// browser on the machine.
    pub async fn connect<R, W>(reader: R, writer: W, config: MediaSidecarConfig) -> Result<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::connect_inner(reader, writer, config, None).await
    }

    async fn connect_inner<R, W>(
        reader: R,
        writer: W,
        config: MediaSidecarConfig,
        child: Option<tokio::process::Child>,
    ) -> Result<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (events, stream) = media_event_channel_with_capacity(config.event_buffer);
        let program = config
            .argv
            .first()
            .cloned()
            .unwrap_or_else(|| "<sidecar>".to_string());
        let shared: Arc<Shared> = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = oneshot::channel();

        // The reader has to be running before the handshake can arrive.
        let task = tokio::spawn(read_lines(
            reader,
            events,
            shared.clone(),
            ready_tx,
            program.clone(),
        ));
        // Aborts the reader if this function returns early — a refused handshake must not leave a
        // task holding a pipe.
        let task = AbortOnDrop(Some(task));

        let ready = match tokio::time::timeout(config.handshake_timeout, ready_rx).await {
            Ok(Ok(ready)) => ready,
            // The reader dropped the sender: stdout ended before a handshake line arrived. The
            // recorded reason is the informative one (a program that is not there, a crash on
            // start); the fallback covers a clean but silent exit.
            Ok(Err(_)) => {
                let reason =
                    shared.death.lock().unwrap().clone().unwrap_or_else(|| {
                        "its stdout ended before it announced itself".to_string()
                    });
                return Err(Error::Other(format!(
                    "room media: the sidecar `{program}` never completed the {MEDIA_PROTOCOL} \
                     handshake — {reason}"
                )));
            }
            Err(_) => {
                return Err(Error::Other(format!(
                    "room media: the sidecar `{program}` did not announce {MEDIA_PROTOCOL} within \
                     {:?}",
                    config.handshake_timeout
                )))
            }
        };
        if ready.ready != MEDIA_PROTOCOL {
            return Err(Error::Other(format!(
                "room media: the sidecar `{program}` speaks `{}`, this flux speaks \
                 `{MEDIA_PROTOCOL}` — refused rather than negotiated, because a media peer that is \
                 only probably compatible is one that joins a room full of people and stays silent",
                ready.ready
            )));
        }

        Ok(Self {
            config,
            shared,
            writer: tokio::sync::Mutex::new(Box::new(writer)),
            next_id: AtomicU64::new(1),
            owns_device_routing: ready.owns_device_routing,
            routing_error: ready.routing_error,
            stream: Mutex::new(Some(stream)),
            _child: child,
            reader: Mutex::new(task.disarm()),
        })
    }

    /// This peer's configuration.
    pub fn config(&self) -> &MediaSidecarConfig {
        &self.config
    }

    /// Whether the sidecar claims to route its own capture device. `false` means audio publication
    /// is refused — see [`super`] for the two measured reasons.
    pub fn owns_device_routing(&self) -> bool {
        self.owns_device_routing
    }

    /// Why the sidecar could not take routing ownership, when it said (D-235).
    ///
    /// Only meaningful while [`Self::owns_device_routing`] is `false`. The common answer on Linux
    /// is that the host audio socket named in argv is behind the sandbox's `/run` tmpfs and was
    /// never granted back with `[sandbox] writable`.
    pub fn routing_error(&self) -> Option<&str> {
        self.routing_error.as_deref()
    }

    /// Why the sidecar is gone, or `None` while it is alive.
    pub fn death_reason(&self) -> Option<String> {
        self.shared.death.lock().unwrap().clone()
    }

    /// Lines the sidecar wrote to stdout that were not protocol. A browser prints; a rising count is
    /// a hint that the harness is logging onto its own control channel, not a failure.
    pub fn unrecognized_lines(&self) -> u64 {
        self.shared.unrecognized.load(Ordering::Relaxed)
    }

    /// Probe the published track against **this sidecar's own** configured floor. The call that
    /// distinguishes a live track from a healthy-looking silent one.
    pub async fn verify_publish_carries_signal(&self) -> Result<Level> {
        self.verify_audible(self.config.min_publish_rms).await
    }

    async fn command(&self, command: MediaCommand) -> Result<MediaReply> {
        if let Some(reason) = self.shared.death.lock().unwrap().clone() {
            return Err(Error::Other(format!(
                "room media: `{}` cannot run — {reason}",
                command_name(&command)
            )));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let line = MediaRequest {
            id,
            command: command.clone(),
        }
        .to_line()?;

        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);

        // The write happens under its own lock so two concurrent commands cannot interleave half a
        // line each — NDJSON has no framing to recover from that.
        let written = {
            let mut writer = self.writer.lock().await;
            match writer.write_all(line.as_bytes()).await {
                Ok(()) => writer.flush().await,
                Err(e) => Err(e),
            }
        };
        if let Err(e) = written {
            self.shared.pending.lock().unwrap().remove(&id);
            return Err(Error::Other(format!(
                "room media: writing `{}` to the sidecar failed: {e}",
                command_name(&command)
            )));
        }

        match tokio::time::timeout(self.config.command_timeout, rx).await {
            Ok(Ok(reply)) if reply.ok => Ok(reply),
            Ok(Ok(reply)) => Err(Error::Other(format!(
                "room media: the sidecar refused `{}`: {}",
                command_name(&command),
                reply.error.unwrap_or_else(|| "no reason given".into())
            ))),
            // The reader dropped our sender: the sidecar died with this command in flight.
            Ok(Err(_)) => Err(Error::Other(format!(
                "room media: the sidecar went away during `{}` — {}",
                command_name(&command),
                self.shared
                    .death
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "no reason recorded".into())
            ))),
            Err(_) => {
                self.shared.pending.lock().unwrap().remove(&id);
                Err(Error::Other(format!(
                    "room media: the sidecar did not answer `{}` within {:?}",
                    command_name(&command),
                    self.config.command_timeout
                )))
            }
        }
    }
}

/// Aborts a reader task unless it is disarmed — so an early return from the handshake never leaves
/// one holding the sidecar's stdout.
struct AbortOnDrop(Option<JoinHandle<()>>);

impl AbortOnDrop {
    fn disarm(mut self) -> Option<JoinHandle<()>> {
        self.0.take()
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// The command's wire name, for error messages — one spelling, taken from the protocol rather than
/// retyped per call site.
fn command_name(command: &MediaCommand) -> String {
    match serde_json::to_value(command) {
        Ok(serde_json::Value::Object(map)) => map
            .get("cmd")
            .and_then(|c| c.as_str())
            .unwrap_or("?")
            .to_string(),
        _ => "?".to_string(),
    }
}

/// The sidecar's stdout, line by line, until it ends.
async fn read_lines<R>(
    reader: R,
    events: MediaEventSender,
    shared: Arc<Shared>,
    ready_tx: oneshot::Sender<Ready>,
    program: String,
) where
    R: AsyncRead + Send + Unpin + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    let mut ready_tx = Some(ready_tx);
    let reason = loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break format!("the sidecar `{program}` closed its stdout"),
            Err(e) => break format!("reading the sidecar `{program}`'s stdout failed: {e}"),
        };
        match SidecarLine::parse(&line) {
            Ok(Some(SidecarLine::Ready(ready))) => {
                // A second handshake is noise, not a re-handshake: the protocol has one.
                match ready_tx.take() {
                    Some(tx) => {
                        let _ = tx.send(ready);
                    }
                    None => {
                        shared.unrecognized.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Ok(Some(SidecarLine::Reply(reply))) => {
                let waiting = shared.pending.lock().unwrap().remove(&reply.id);
                match waiting {
                    Some(tx) => {
                        let _ = tx.send(reply);
                    }
                    // A reply to a command that already timed out, or an id we never sent.
                    None => {
                        shared.unrecognized.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            // A consumer that dropped the stream does not end the session: replies still matter, and
            // `leave` still has to be sendable. The send is non-blocking and sheds audio on its own
            // (see `MediaEventSender::send`), so a slow consumer cannot stall this loop — which is
            // what would otherwise stall the *outbound* half of the same pipe.
            Ok(Some(SidecarLine::Event(event))) => {
                let _ = events.send(event);
            }
            // Not protocol at all — a log line, a banner, a stack trace.
            Ok(None) => {
                shared.unrecognized.fetch_add(1, Ordering::Relaxed);
            }
            // Claims to be protocol and is malformed. Counted, and deliberately not fatal: ending a
            // live call over one bad line is the worse failure.
            Err(e) => {
                shared.unrecognized.fetch_add(1, Ordering::Relaxed);
                eprintln!("room media: unparseable line from `{program}`: {e}");
            }
        }
    };

    // The sidecar is gone. Record why *before* releasing the waiters, so each one reads the reason
    // rather than racing it, then drop every pending sender — that is what turns a wait into an
    // immediate, explained failure instead of a timeout.
    *shared.death.lock().unwrap() = Some(reason);
    shared.pending.lock().unwrap().clear();
    drop(events);
}

#[async_trait]
impl MediaPeer for SidecarMediaPeer {
    async fn join(&self, room: &RoomId, identity: &RoomIdentity) -> Result<MediaStream> {
        let stream = self.stream.lock().unwrap().take().ok_or_else(|| {
            Error::Other(
                "room media: this sidecar has already joined — one sidecar is one media session"
                    .into(),
            )
        })?;
        match self
            .command(MediaCommand::Join {
                room: room.clone(),
                nick: identity.nick.clone(),
                kind: identity.kind,
            })
            .await
        {
            Ok(_) => Ok(stream),
            Err(e) => {
                // A failed join must be retryable against the same peer, so the stream goes back.
                *self.stream.lock().unwrap() = Some(stream);
                Err(e)
            }
        }
    }

    async fn publish_audio(&self, audio: &AudioChunk) -> Result<()> {
        if !self.owns_device_routing {
            // D-235: the sidecar's own reason is the diagnosable half. Without it the operator's
            // only evidence is a level probe reading zero, which points at the room rather than at
            // the sandbox — so when the sidecar said nothing, name the likeliest cause here.
            let cause = match self.routing_error.as_deref() {
                Some(reason) => format!(" — {reason}"),
                None => " — the sidecar gave no reason. On Linux the usual one is the sandbox: it \
                         masks `/run` with a tmpfs, so the host audio socket named in the \
                         sidecar's argv (conventionally `/run/user/<uid>/pulse/native`) is absent \
                         unless the operator also granted its directory back with `[sandbox] \
                         writable = [\"/run/user/<uid>/pulse\"]`. Argv alone does not reach it"
                    .to_string(),
            };
            return Err(Error::Other(format!(
                "room media: the sidecar `{}` does not claim to own device routing, so flux will \
                 not publish audio through it — a browser-default capture is silent on Chrome 150 \
                 (measured 2026-07-30: `--use-fake-device-for-media-capture` ignored, \
                 `setAudioInputDevice` did not stick) and every status reads healthy while it \
                 is{cause}",
                self.config.argv.first().map(String::as_str).unwrap_or("")
            )));
        }
        self.command(MediaCommand::PublishAudio {
            audio: audio.clone(),
        })
        .await
        .map(|_| ())
    }

    async fn publish_video(&self, video: &VideoFrame) -> Result<()> {
        self.command(MediaCommand::PublishVideo {
            video: video.clone(),
        })
        .await
        .map(|_| ())
    }

    async fn mute(&self, muted: bool) -> Result<()> {
        self.command(MediaCommand::Mute { muted }).await.map(|_| ())
    }

    async fn level(&self) -> Result<Level> {
        let reply = self.command(MediaCommand::Level).await?;
        reply.level.ok_or_else(|| {
            Error::Other(
                "room media: the sidecar answered the level probe without a measurement — an \
                 unmeasured track is not an audible one"
                    .into(),
            )
        })
    }

    async fn leave(&self) -> Result<()> {
        self.command(MediaCommand::Leave).await.map(|_| ())
    }
}

impl Drop for SidecarMediaPeer {
    fn drop(&mut self) {
        if let Some(task) = self.reader.lock().unwrap().take() {
            task.abort();
        }
    }
}

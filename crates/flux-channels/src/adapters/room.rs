//! The **room** adapter (`kind = "room"`): a many-party meeting room as an event source (D-204).
//!
//! Each attributed inbound message wakes the program under the channel's name, carrying **who** said
//! it — so a `trigger { on = "<channel name>" }` routes a room the same way it routes a webhook, with
//! no new host. The triggered journeys' non-empty results are said back into the room, the way the
//! webhook adapter returns them as the HTTP response.
//!
//! ## Safety
//!
//! A room is untrusted multi-party input and **joining one grants no authority**: the delivery goes
//! through `flux_app::App::deliver` like every other channel, so the turn it wakes meets the ordinary
//! `Executor` + approver envelope. One consequence shows up here: a *delivery* error (a denied op, a
//! failing journey) is logged and the room keeps running. A room is a live conversation with people in
//! it, and one message that trips the envelope must not tear the channel down — the same posture the
//! schedule adapter takes for a failed tick.
//!
//! ## Which failures are fatal (D-205)
//!
//! [`crate::serve`] ends the whole process on a channel error, so the room's two failures are kept
//! apart in [`Channel::start`] rather than collapsed: **a failed join is fatal** (the channel never
//! started, and a silently absent agent is worse than a loud stop), while **a session that fails after
//! joining is not** (a socket that died mid-meeting ends the room, not the schedule and the webhook
//! running beside it). See [`RoomSessionEnd`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use flux_flow::voice::{RoomTranscript, Speaker, VoiceReply, VoiceTurnHandler};
use flux_lang::program::ChannelDecl;

#[cfg(feature = "room-media")]
use flux_system::System;

use crate::config::{RoomSettings, DEFAULT_ROOM_NICK};
#[cfg(feature = "room-media")]
use crate::rooms::media::{MediaPeer, MediaSidecarConfig, MediaStream, SidecarMediaPeer};
use crate::rooms::{
    AddressRule, BraveTalkTokens, JaasConfig, JaasRoom, MockRoom, ReplyBudget, Room, RoomIdentity,
    RoomSessionEnd, RoomTurnDriver, XmppConfig, XmppMucRoom, DEFAULT_ROOM_REPLY_BUDGET,
    DEFAULT_ROOM_REPLY_WINDOW,
};
use crate::{Channel, Deliverer};

pub struct RoomChannel {
    name: String,
    identity: RoomIdentity,
    room: Arc<dyn Room>,
    address_rule: AddressRule,
    reply_budget: usize,
    reply_window: Duration,
    /// The optional media sidecar (D-208), `None` unless the declaration opted in. Validated at
    /// load; nothing is spawned until [`Channel::start`].
    #[cfg(feature = "room-media")]
    media: Option<MediaSidecarConfig>,
}

impl RoomChannel {
    /// Build the channel from its declaration, selecting the backend named by `settings.backend`.
    pub fn from_decl(decl: &ChannelDecl) -> anyhow::Result<Self> {
        let settings = Self::settings(decl)?;
        let room: Arc<dyn Room> = match settings.backend.as_str() {
            // The in-process backend, the same role the `mock` provider plays for models: a real
            // implementation that makes the layers above testable with no network and no vendor.
            "mock" => Arc::new(MockRoom::new(settings.room.clone())),
            // The portable one: a standards-compliant MUC over the RFC 7395 WebSocket binding, with
            // no browser and no vendor SDK (D-205).
            "xmpp" => Arc::new(XmppMucRoom::new(
                XmppConfig::from_settings(&settings)
                    .map_err(|e| anyhow::anyhow!("channel `{}`: {e}", decl.name))?,
            )),
            // The vendor one: Brave Talk and any 8x8 JaaS tenant (D-206). Same MUC machinery as
            // `xmpp` underneath — all this adds is where the guest token comes from and what
            // happens when it expires three hours later.
            "jaas" => Arc::new(JaasRoom::new(
                JaasConfig::from_settings(&settings)
                    .map_err(|e| anyhow::anyhow!("channel `{}`: {e}", decl.name))?,
                Arc::new(BraveTalkTokens::from_settings(&settings)),
            )),
            other => anyhow::bail!(
                "channel `{}`: unknown room backend `{other}` (known: mock, xmpp, jaas)",
                decl.name
            ),
        };
        Self::over(decl, &settings, room)
    }

    /// Build the channel over an already-constructed [`Room`], ignoring `settings.backend`. The seam a
    /// test drives a scripted room through — and the seam a host with its own credential-bearing
    /// backend uses rather than re-deriving one from the declaration.
    pub fn with_room(decl: &ChannelDecl, room: Arc<dyn Room>) -> anyhow::Result<Self> {
        let settings = Self::settings(decl)?;
        Self::over(decl, &settings, room)
    }

    /// The channel's declared settings, with the declaration's name in any error.
    fn settings(decl: &ChannelDecl) -> anyhow::Result<RoomSettings> {
        serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))
    }

    fn over(
        decl: &ChannelDecl,
        settings: &RoomSettings,
        room: Arc<dyn Room>,
    ) -> anyhow::Result<Self> {
        let nick = settings
            .nick
            .clone()
            .unwrap_or_else(|| DEFAULT_ROOM_NICK.to_string());
        // A rule outside the vocabulary fails the load rather than degrading to "answer everything"
        // — the failure D-207's whole point is to prevent, arriving as a typo (D-204 carried this
        // field unvalidated on purpose, because the vocabulary had not been chosen yet).
        let address_rule = match settings.address_rule.as_deref() {
            Some(spec) => AddressRule::parse(spec)
                .map_err(|e| anyhow::anyhow!("channel `{}`: {e}", decl.name))?,
            None => AddressRule::default(),
        };
        let reply_window = match settings.reply_window_secs {
            Some(0) => anyhow::bail!(
                "channel `{}`: `reply_window_secs` of 0 is a budget that resets on every message",
                decl.name
            ),
            Some(secs) => Duration::from_secs(secs),
            None => DEFAULT_ROOM_REPLY_WINDOW,
        };
        // D-208's opt-in, and the reason it is *two* gates rather than one. The cargo feature says
        // this flux can drive a media sidecar at all; the declaration says this room should. A
        // declaration the build cannot honour is refused by name here rather than dropped, because
        // the way this fails in the wild is an operator watching text work and concluding media is
        // working too.
        #[cfg(not(feature = "room-media"))]
        if settings.media.is_some() {
            anyhow::bail!(
                "channel `{}`: `media` is declared but this flux was built without the \
                 `room-media` cargo feature — text and presence still work, media does not",
                decl.name
            );
        }
        Ok(Self {
            name: decl.name.clone(),
            identity: RoomIdentity::agent(nick),
            room,
            address_rule,
            reply_budget: settings.reply_budget.unwrap_or(DEFAULT_ROOM_REPLY_BUDGET),
            reply_window,
            #[cfg(feature = "room-media")]
            media: settings
                .media
                .as_ref()
                .map(|media| {
                    MediaSidecarConfig::from_settings(media)
                        .map_err(|e| anyhow::anyhow!("channel `{}`: {e}", decl.name))
                })
                .transpose()?,
        })
    }

    /// The media sidecar this channel would start, or `None` when the declaration did not opt in.
    #[cfg(feature = "room-media")]
    pub fn media(&self) -> Option<&MediaSidecarConfig> {
        self.media.as_ref()
    }

    /// Start the media sidecar, if one was declared, and join it to the room.
    ///
    /// **Returns `None` on every failure, and that is the contract** (D-208): a sidecar that would
    /// not start, would not handshake, or would not join is a media outage, not a room outage. It is
    /// reported under the channel's name and the meeting continues with text and presence — the same
    /// posture this adapter already takes for a failed delivery, and for the same reason: there are
    /// people in the room.
    ///
    /// The peer is returned so it stays alive for the session; dropping it kills the sidecar.
    #[cfg(feature = "room-media")]
    async fn start_media(&self) -> Option<(Arc<SidecarMediaPeer>, MediaStream)> {
        let config = self.media.as_ref()?.clone();
        let program = config.argv.first().cloned().unwrap_or_default();
        // The workspace this flux is already confined to. `spawn_interactive` pins the child's cwd
        // to it and clears the environment — see `rooms::media::sidecar` for what that means for a
        // sidecar that needs to find the host's audio server.
        let system = match std::env::current_dir()
            .map_err(|e| flux_core::Error::Other(e.to_string()))
            .and_then(System::from_env)
        {
            Ok(system) => system,
            Err(e) => {
                eprintln!("channel `{}`: room media is off — {e}", self.name);
                return None;
            }
        };
        let peer = match SidecarMediaPeer::spawn(&system, config).await {
            Ok(peer) => Arc::new(peer),
            Err(e) => {
                eprintln!("channel `{}`: room media is off — {e}", self.name);
                return None;
            }
        };
        match peer.join(self.room.id(), &self.identity).await {
            Ok(stream) => {
                eprintln!(
                    "channel `{}`: room media sidecar `{program}` joined {}",
                    self.name,
                    self.room.id()
                );
                Some((peer, stream))
            }
            Err(e) => {
                eprintln!("channel `{}`: room media is off — {e}", self.name);
                None
            }
        }
    }
}

#[async_trait]
impl Channel for RoomChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
        let handler = RoomDelivery {
            label: self.name.clone(),
            room: self.room.id().as_str().to_string(),
            deliverer: d,
            overheard: Mutex::new(RoomTranscript::new()),
        };
        // Media first, and deliberately *before* the text session rather than inside it: whatever it
        // returns, the session below runs. `None` is a media outage that has already been reported.
        // The peer is held for the whole session — dropping it kills the sidecar (`kill_on_drop`).
        #[cfg(feature = "room-media")]
        let media = self.start_media().await;
        // The posture, decided explicitly (D-205). `crate::serve` treats a channel error as fatal to
        // the whole process, so the two failures are separated here rather than collapsed:
        //
        // - **the join failed** — a wrong endpoint, a refused credential, a room that does not exist.
        //   The channel never started, nobody is going to notice a silently absent agent, and the fix
        //   is the operator's. Fatal.
        // - **the session failed after joining** — the socket died mid-meeting. The room is over, and
        //   nothing else is: a schedule and a webhook in the same program must not go down with it.
        //   Logged under the channel's name and ended, the same posture this adapter already takes
        //   for a failed delivery.
        let ended = RoomTurnDriver::new(self.room.clone(), self.identity.clone())
            .with_address_rule(self.address_rule.clone())
            .with_reply_budget(ReplyBudget::new(self.reply_budget, self.reply_window))
            .run(&handler, &cancel)
            .await;

        #[cfg(feature = "room-media")]
        self.stop_media(media).await;

        match ended {
            Err(e) => Err(anyhow::anyhow!("channel `{}`: {e}", self.name)),
            Ok(RoomSessionEnd::Failed(e)) => {
                eprintln!("channel `{}`: the room session ended: {e}", self.name);
                Ok(())
            }
            Ok(RoomSessionEnd::Ended) => Ok(()),
        }
    }
}

#[cfg(feature = "room-media")]
impl RoomChannel {
    /// Leave the media plane and report what the session shed.
    ///
    /// A failure here is logged, never returned: the text session has already ended, and a sidecar
    /// that cannot say goodbye is not a reason to fail a channel. The peer is dropped afterwards,
    /// which kills the process whatever it decided about `leave`.
    async fn stop_media(&self, media: Option<(Arc<SidecarMediaPeer>, MediaStream)>) {
        let Some((peer, stream)) = media else {
            return;
        };
        // D-208 ships the seam, not a consumer — D-209 (audio in) is what reads this stream. Until
        // it does, inbound audio is shed by the bounded queue exactly as designed, and saying so is
        // more useful than a silence an operator would read as "no audio arrived".
        let dropped = stream.dropped_audio_frames();
        if dropped > 0 {
            eprintln!(
                "channel `{}`: room media shed {dropped} inbound audio frames — nothing consumes \
                 them yet (D-209)",
                self.name
            );
        }
        if let Err(e) = peer.leave().await {
            eprintln!(
                "channel `{}`: the media sidecar did not leave: {e}",
                self.name
            );
        }
    }
}

/// The turn handler the channel drives: one **addressed** room message → one `deliver` under the
/// channel name → the journeys' results said back into the room.
///
/// The messages that were *not* addressed to flux still land here, as
/// [`overheard`](VoiceTurnHandler::overheard), and accumulate in an attributed
/// [`RoomTranscript`]. They cost nothing until flux is next spoken to, and then they ride the
/// delivery as `context` — which is what lets an answer refer to "what Timo asked" rather than to a
/// question with no conversation around it.
struct RoomDelivery {
    label: String,
    room: String,
    deliverer: Arc<dyn Deliverer>,
    /// What was said in the room while flux was not the addressee, oldest first and bounded.
    overheard: Mutex<RoomTranscript>,
}

#[async_trait]
impl VoiceTurnHandler for RoomDelivery {
    async fn turn(&self, speaker: &Speaker, user_text: &str) -> VoiceReply {
        // Drained, so the same context is never carried into two turns and re-billed. Each line
        // keeps its speaker id — a MUC nick is occupant-chosen and non-unique (C-408), so context
        // attributed by display name would merge two strangers into one voice.
        let context: Vec<_> = self
            .overheard
            .lock()
            .unwrap()
            .drain()
            .into_iter()
            .map(|line| {
                json!({
                    "speaker": line.speaker.id(),
                    "nick": line.speaker.display_name(),
                    "text": line.text,
                })
            })
            .collect();
        let payload = json!({
            "room": self.room,
            "text": user_text,
            "speaker": speaker.id(),
            "nick": speaker.display_name(),
            "name": self.label,
            "context": context,
        });
        match self.deliverer.deliver(&self.label, payload).await {
            Ok(runs) => {
                let reply = runs
                    .iter()
                    .map(|r| r.result.trim())
                    .filter(|r| !r.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                VoiceReply::Continue(reply)
            }
            // People are still in the room: log it and stay. A denied op or a failing journey is one
            // message going wrong, not a reason to walk out of the meeting.
            Err(e) => {
                eprintln!("channel `{}`: room delivery failed: {e}", self.label);
                VoiceReply::Continue(String::new())
            }
        }
    }

    /// A line flux heard but was not addressed by. It is recorded and **not delivered** — no
    /// journey, no planner, no spend — so the only thing an unaddressed room costs is memory, and
    /// that is bounded by the transcript's own capacity.
    async fn overheard(&self, speaker: &Speaker, text: &str) {
        self.overheard.lock().unwrap().push(speaker, text);
    }
}

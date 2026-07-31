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

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use flux_flow::voice::{Speaker, VoiceReply, VoiceTurnHandler};
use flux_lang::program::ChannelDecl;

use crate::config::{RoomSettings, DEFAULT_ROOM_NICK};
use crate::rooms::{MockRoom, Room, RoomIdentity, RoomTurnDriver};
use crate::{Channel, Deliverer};

pub struct RoomChannel {
    name: String,
    identity: RoomIdentity,
    room: Arc<dyn Room>,
}

impl RoomChannel {
    /// Build the channel from its declaration, selecting the backend named by `settings.backend`.
    pub fn from_decl(decl: &ChannelDecl) -> anyhow::Result<Self> {
        let settings = Self::settings(decl)?;
        let room: Arc<dyn Room> = match settings.backend.as_str() {
            // The in-process backend, the same role the `mock` provider plays for models: a real
            // implementation that makes the layers above testable with no network and no vendor.
            "mock" => Arc::new(MockRoom::new(settings.room.clone())),
            // `xmpp` (D-205) and `jaas` (D-206) register here.
            other => anyhow::bail!(
                "channel `{}`: unknown room backend `{other}` (known: mock)",
                decl.name
            ),
        };
        Ok(Self::over(decl, &settings, room))
    }

    /// Build the channel over an already-constructed [`Room`], ignoring `settings.backend`. The seam a
    /// test drives a scripted room through — and the seam a host with its own credential-bearing
    /// backend uses rather than re-deriving one from the declaration.
    pub fn with_room(decl: &ChannelDecl, room: Arc<dyn Room>) -> anyhow::Result<Self> {
        let settings = Self::settings(decl)?;
        Ok(Self::over(decl, &settings, room))
    }

    /// The channel's declared settings, with the declaration's name in any error.
    fn settings(decl: &ChannelDecl) -> anyhow::Result<RoomSettings> {
        serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))
    }

    fn over(decl: &ChannelDecl, settings: &RoomSettings, room: Arc<dyn Room>) -> Self {
        let nick = settings
            .nick
            .clone()
            .unwrap_or_else(|| DEFAULT_ROOM_NICK.to_string());
        Self {
            name: decl.name.clone(),
            identity: RoomIdentity::agent(nick),
            room,
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
        };
        RoomTurnDriver::new(self.room.clone(), self.identity.clone())
            .run(&handler, &cancel)
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: {e}", self.name))
    }
}

/// The turn handler the channel drives: one attributed room message → one `deliver` under the channel
/// name → the journeys' results said back into the room.
struct RoomDelivery {
    label: String,
    room: String,
    deliverer: Arc<dyn Deliverer>,
}

#[async_trait]
impl VoiceTurnHandler for RoomDelivery {
    async fn turn(&self, speaker: &Speaker, user_text: &str) -> VoiceReply {
        let payload = json!({
            "room": self.room,
            "text": user_text,
            "speaker": speaker.id(),
            "nick": speaker.display_name(),
            "name": self.label,
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
}

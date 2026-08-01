//! The [`Room`] port — presence, occupants and **attributed** text in a many-party room (D-204).
//!
//! Every other channel this crate carries is either 1:1 or fire-and-forget. A room is the shape flux
//! did not have: flux is *one participant among several*, it hears everything, it is addressed by
//! only some of it, and every inbound event therefore has to say **who** produced it. That is why
//! [`RoomEvent`] carries an [`OccupantId`] on every variant a participant can cause — attribution is
//! not a feature of the room, it is the precondition for deciding whether to answer at all. D-207
//! turns that decision into the [`AddressRule`] the [`RoomTurnDriver`] applies, bounded by a
//! per-room [`ReplyBudget`] for the case the rule cannot see.
//!
//! ## The port, and what is behind it
//!
//! [`Room`] is a trait with swappable backends, the same shape the state store (D-71), the workboard
//! port (A-113) and the agent-runtime port (A-121) use. It is deliberately **text-only and
//! backend-agnostic**: presence + groupchat + private messages are pure XMPP and need no browser and
//! no vendor SDK. Media (audio, screenshare) needs a real WebRTC endpoint and lands later behind a
//! feature gate, as extra [`RoomEvent`] variants — which is why that enum is `#[non_exhaustive]`.
//!
//! Backends: [`MockRoom`] (in-process, here), [`XmppMucRoom`] (D-205, the portable one — a generic
//! prosody/ejabberd/JaaS MUC over the RFC 7395 WebSocket binding, no browser and no vendor SDK) and
//! [`JaasRoom`] (D-206, vendor guest-token acquisition and refresh over the same machinery).
//!
//! ## Safety
//!
//! **A room is untrusted multi-party input, and joining one grants no authority whatsoever.** Anyone
//! holding the link can put a client in the room without an account, so a room-sourced turn meets the
//! same `Executor` + approver envelope as a CLI turn — see
//! `tests/rooms.rs::a_room_sourced_turn_dispatches_through_the_executor_and_approver`. Nothing in
//! this module dispatches a tool, holds a permission, or approves anything; it carries text and
//! identity, and hands both to the ordinary machinery.
//!
//! Design: [`docs/designs/meeting-rooms.md`](https://github.com/codewandler/flux/blob/main/docs/designs/meeting-rooms.md).

mod address;
mod budget;
mod driver;
mod jaas;
mod mock;
mod xmpp;

pub use address::{
    is_a2a_envelope, AddressRule, AddressRuleError, Addressing, DEFAULT_ADDRESS_RULE,
};
pub use budget::{ReplyBudget, DEFAULT_ROOM_REPLY_BUDGET, DEFAULT_ROOM_REPLY_WINDOW};
pub use driver::{RoomSessionEnd, RoomTurnDriver};
pub use jaas::{
    BraveTalkTokens, Conference, GuestToken, JaasConfig, JaasRoom, JaasTokens,
    DEFAULT_BRAVE_TOKEN_SERVICE, DEFAULT_JAAS_CONFERENCE_SERVICE, DEFAULT_JAAS_REFRESH_LEAD,
    DEFAULT_JAAS_SIGNALLING, JAAS_REFRESH_RETRY, MIN_JAAS_REFRESH_INTERVAL,
};
pub use mock::MockRoom;
pub use xmpp::{XmppConfig, XmppMucRoom, DEFAULT_XMPP_HANDSHAKE_TIMEOUT, DEFAULT_XMPP_KEEPALIVE};

use std::fmt;

use async_trait::async_trait;
use tokio::sync::mpsc;

use flux_core::Result;

/// A room's address, as the **server** spells it. Opaque to flux: an XMPP MUC JID
/// (`standup@conference.example.org`), a vendor room handle, whatever the backend uses.
///
/// Take this from the server rather than rebuilding it locally — JaaS lowercases the room name in the
/// MUC JID while the guest token's `room` claim keeps the original case (D-205/D-206), so a
/// locally-assembled address is subtly wrong in exactly the way that is hard to see.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomId(String);

/// One occupant's address **within** a room, as the server assigns it — an XMPP occupant JID
/// (`standup@conference.example.org/timo`). Room-scoped and opaque: two rooms' ids never compare, and
/// flux never parses one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OccupantId(String);

macro_rules! room_string_id {
    ($t:ty, $what:literal) => {
        impl $t {
            #[doc = concat!("Wrap a server-supplied ", $what, ".")]
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// The underlying server-supplied string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $t {
            fn from(s: &str) -> Self {
                Self::new(s)
            }
        }

        impl From<String> for $t {
            fn from(s: String) -> Self {
                Self::new(s)
            }
        }
    };
}

room_string_id!(RoomId, "room address");
room_string_id!(OccupantId, "occupant address");

/// What kind of participant an occupant is. Not cosmetic: D-207 needs it to keep two flux agents from
/// ping-ponging on each other's plain text, and a MUC's own service occupants (Jitsi's `focus`) must
/// never be treated as someone to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OccupantKind {
    /// A person.
    Human,
    /// Another automated participant — a flux agent, or someone else's bot.
    Agent,
    /// Room infrastructure (Jitsi's `focus`, a transcription bot, a bridge).
    Service,
    /// The backend could not tell. Treat as untrusted, like every other occupant.
    Unknown,
}

/// One participant in a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occupant {
    /// The server-assigned, room-scoped address.
    pub id: OccupantId,
    /// The room-visible display name (an XMPP nick). Cosmetic — occupants may share one, so never
    /// key anything on it.
    pub nick: String,
    /// What kind of participant this is.
    pub kind: OccupantKind,
    /// True for the occupant *this* session joined as. A MUC echoes our own groupchat messages back
    /// to us, so a driver that cannot recognize itself answers itself forever.
    pub is_self: bool,
}

impl Occupant {
    /// Another occupant of the room.
    pub fn new(id: impl Into<OccupantId>, nick: impl Into<String>, kind: OccupantKind) -> Self {
        Self {
            id: id.into(),
            nick: nick.into(),
            kind,
            is_self: false,
        }
    }

    /// Mark this occupant as the one we joined as.
    pub fn as_self(mut self) -> Self {
        self.is_self = true;
        self
    }
}

/// Who flux joins a room *as*: the room-visible nick and what it presents itself as. Backend
/// credentials (a JaaS token, a MUC password) are the backend's own configuration, not part of the
/// room-visible identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomIdentity {
    /// The nick to join under. The server may hand back a different one on collision — trust the
    /// occupant it reports, not this.
    pub nick: String,
    /// What flux presents itself as. Almost always [`OccupantKind::Agent`]: a room containing humans
    /// is owed an honest answer about what just joined it.
    pub kind: OccupantKind,
}

impl RoomIdentity {
    /// Join as an agent under `nick` — the ordinary case.
    pub fn agent(nick: impl Into<String>) -> Self {
        Self {
            nick: nick.into(),
            kind: OccupantKind::Agent,
        }
    }
}

/// Whether a message was said to the whole room or to us alone. A whisper is a *stronger* signal of
/// being addressed than public text (D-207), and a reply to one must go back the same way — so the
/// distinction has to survive the trip through the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageScope {
    /// `type='groupchat'` — everyone in the room sees it.
    Groupchat,
    /// A private message between two occupants.
    Private,
}

/// Something that happened in a room.
///
/// **Every variant a participant can cause names the occupant who caused it** — see
/// [`Self::occupant`]. Only [`Ended`](Self::Ended), the room's own lifecycle terminator, has none.
///
/// `#[non_exhaustive]`: the media variants (`Audio`, `SpeechStarted`) arrive with the feature-gated
/// sidecar in D-208…D-211, and a consumer written today must not have to change to accommodate them.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RoomEvent {
    /// An occupant joined (or was already present when we joined, replayed from MUC presence).
    Joined { occupant: Occupant },
    /// An occupant left.
    Left { occupant: OccupantId },
    /// An occupant said something. `from` is what makes this a *turn by somebody* rather than
    /// anonymous text.
    Message {
        from: OccupantId,
        text: String,
        scope: MessageScope,
    },
    /// The room is over: the session was closed, kicked, or the transport went away for good. No
    /// further events follow.
    Ended,
}

impl RoomEvent {
    /// The occupant this event is about — `None` only for [`Ended`](Self::Ended), which no
    /// participant causes. A consumer that needs attribution can require `Some` and be sure the port
    /// is holding up its end.
    pub fn occupant(&self) -> Option<&OccupantId> {
        match self {
            RoomEvent::Joined { occupant } => Some(&occupant.id),
            RoomEvent::Left { occupant } => Some(occupant),
            RoomEvent::Message { from, .. } => Some(from),
            RoomEvent::Ended => None,
        }
    }
}

/// The stream of [`RoomEvent`]s a [`Room::join`] hands back.
///
/// An owned receiver rather than a `Stream`: a backend's protocol loop is a task feeding a channel
/// (that is exactly what the XMPP WebSocket read loop is), and the consumer is a `select!` that also
/// watches a cancellation token. Bounded on purpose — a busy room must apply backpressure to its own
/// socket rather than growing an unbounded queue of unheard chatter.
pub struct RoomStream(mpsc::Receiver<RoomEvent>);

/// The producing half of a [`RoomStream`] — a backend's protocol loop holds this.
#[derive(Clone)]
pub struct RoomEventSender(mpsc::Sender<RoomEvent>);

/// Default in-flight room events. A room that outruns this is a room whose agent is already behind.
pub const DEFAULT_ROOM_EVENT_BUFFER: usize = 64;

/// Create a room event channel with [`DEFAULT_ROOM_EVENT_BUFFER`] slots.
pub fn room_event_channel() -> (RoomEventSender, RoomStream) {
    room_event_channel_with_capacity(DEFAULT_ROOM_EVENT_BUFFER)
}

/// Create a room event channel with an explicit buffer size.
pub fn room_event_channel_with_capacity(capacity: usize) -> (RoomEventSender, RoomStream) {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    (RoomEventSender(tx), RoomStream(rx))
}

impl RoomStream {
    /// The next event, or `None` once the backend has dropped its sender (the transport is gone).
    pub async fn recv(&mut self) -> Option<RoomEvent> {
        self.0.recv().await
    }
}

impl RoomEventSender {
    /// Publish one event, waiting for buffer space. `Err` means the consumer is gone — the backend
    /// should stop reading its socket.
    pub async fn send(&self, event: RoomEvent) -> std::result::Result<(), RoomEvent> {
        self.0.send(event).await.map_err(|e| e.0)
    }
}

/// A many-party room flux can be present in: presence, an occupant list, public and private text.
///
/// Implementations are the **only** thing that talks to a room server. Everything above this trait
/// (the [`RoomTurnDriver`], the `room` channel adapter) is backend-agnostic, which is what lets D-205
/// land a real XMPP MUC backend and D-206 a vendor one without touching a consumer.
///
/// Implementing this grants no authority: a room backend delivers text and identity, and the turn it
/// wakes runs through the ordinary safety envelope.
#[async_trait]
pub trait Room: Send + Sync {
    /// The room's address as the server spells it.
    fn id(&self) -> &RoomId;

    /// Join the room as `identity` and start receiving events. The occupants already present are
    /// replayed as [`RoomEvent::Joined`], ours among them, so a consumer needs no separate priming
    /// call and cannot miss an occupant that arrived between joining and asking.
    async fn join(&self, identity: &RoomIdentity) -> Result<RoomStream>;

    /// The occupants the backend currently believes are present, tracked from presence.
    async fn occupants(&self) -> Result<Vec<Occupant>>;

    /// Say something to the whole room (`type='groupchat'`).
    async fn say(&self, text: &str) -> Result<()>;

    /// Say something to one occupant privately.
    async fn whisper(&self, to: &OccupantId, text: &str) -> Result<()>;

    /// Leave the room. Idempotent — leaving a room we are not in is not an error.
    async fn leave(&self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_lifecycle_terminator_lacks_an_occupant() {
        let timo = Occupant::new("standup@x/timo", "timo", OccupantKind::Human);
        assert_eq!(
            RoomEvent::Joined {
                occupant: timo.clone()
            }
            .occupant(),
            Some(&timo.id)
        );
        assert_eq!(
            RoomEvent::Left {
                occupant: timo.id.clone()
            }
            .occupant(),
            Some(&timo.id)
        );
        assert_eq!(
            RoomEvent::Message {
                from: timo.id.clone(),
                text: "hi".into(),
                scope: MessageScope::Groupchat,
            }
            .occupant(),
            Some(&timo.id)
        );
        assert_eq!(RoomEvent::Ended.occupant(), None);
    }

    #[tokio::test]
    async fn the_stream_ends_when_the_backend_drops_its_sender() {
        let (tx, mut stream) = room_event_channel_with_capacity(2);
        tx.send(RoomEvent::Ended).await.unwrap();
        drop(tx);
        assert_eq!(stream.recv().await, Some(RoomEvent::Ended));
        assert_eq!(
            stream.recv().await,
            None,
            "a dead transport ends the stream"
        );
    }
}

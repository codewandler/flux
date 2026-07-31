//! [`XmppMucRoom`] — the **portable** [`Room`] backend (D-205): a standards-compliant XMPP MUC over
//! the RFC 7395 WebSocket binding, with **no browser and no vendor SDK**.
//!
//! This is the backend CI runs, and the one the design's invariant 6 ("text needs no browser") is
//! about: presence, occupant tracking, `groupchat` and private messages are pure XMPP, so the whole
//! suite in `tests/xmpp_room.rs` runs against an in-process double with no Chrome installed. D-206
//! layers vendor token acquisition on top of exactly this machinery.
//!
//! ## Three findings the 2026-07-30 spike paid for
//!
//! Each is a regression test rather than a comment alone (`tests/xmpp_room.rs`):
//!
//! - **Every stanza is `jabber:client`-qualified.** prosody answers an unqualified one with
//!   `<unsupported-stanza-type/>` and closes the stream. [`stanza::stanza`] is the only door to the
//!   three stanza kinds and it always qualifies.
//! - **The keepalive is an XMPP ping IQ, never whitespace.** A `" "` frame is illegal on the
//!   WebSocket binding and is closed with `1007 Invalid payload start character`.
//! - **The room's JID comes from the server.** JaaS lowercases the room name in the MUC JID while the
//!   guest token's `room` claim keeps the original case, so [`XmppMucRoom::id`] answers what the
//!   server said the moment presence has told us — never a locally-rebuilt address.
//!
//! ## Safety
//!
//! The WebSocket endpoint goes through **the** egress guard,
//! [`flux_system::net::guard_url_scoped`], in its `http`/`https` form; there is no second URL guard
//! here. A private or loopback endpoint (a self-hosted prosody) needs an explicit grant, which is what
//! [`XmppConfig::allow_private_net`] is. Credentials are never logged, never rendered into an error,
//! and [`XmppConfig`]'s `Debug` redacts them.
//!
//! Joining a room grants no authority: this module carries text and identity, and hands both to the
//! ordinary [`Room`] machinery.

mod session;
mod stanza;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use std::{fmt, result::Result as StdResult};

use async_trait::async_trait;

use flux_core::{Error, Result};
use flux_system::net::PrivateNetAllow;

use super::{Occupant, OccupantId, Room, RoomId, RoomIdentity, RoomStream};
use crate::rooms::room_event_channel;
use stanza::{stanza, Element, NS_CLIENT};

/// How long any one handshake step waits before the join is declared failed.
pub const DEFAULT_XMPP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the XMPP ping IQ goes out. Comfortably inside the 60-90 s idle timeout a reverse proxy
/// in front of a MUC service typically enforces.
pub const DEFAULT_XMPP_KEEPALIVE: Duration = Duration::from_secs(30);

/// How to reach and join one MUC.
///
/// Built with [`XmppConfig::new`] plus the optional setters — the credential fields are `pub` for the
/// same reason every other channel's settings are, but constructing through the setters keeps a
/// caller from silently skipping the egress decision.
#[derive(Clone)]
pub struct XmppConfig {
    /// The RFC 7395 endpoint: `wss://example.org/xmpp-websocket` (or `ws://` for a local server).
    pub url: String,
    /// The MUC JID to join, as configured. The **server's** spelling wins once presence arrives.
    pub room: String,
    /// The XMPP domain for the stream `<open to=…>`. A MUC JID does not carry it (a room lives on the
    /// `conference.` component, not the server), so it is configured; absent, the endpoint host is
    /// used, which is right for the common single-host deployment.
    pub domain: Option<String>,
    /// SASL `PLAIN` username. Absent selects `ANONYMOUS` — the only mechanism JaaS offers, where
    /// authorization rides the endpoint URL instead.
    pub user: Option<String>,
    /// SASL `PLAIN` password. Never logged; redacted from `Debug`.
    pub password: Option<String>,
    /// The MUC's own room password (XEP-0045). Never logged; redacted from `Debug`.
    pub muc_password: Option<String>,
    /// Whether this endpoint may resolve to a private/loopback address. Off by default: a self-hosted
    /// prosody on the LAN is a deliberate operator decision, not a default.
    pub private_net: PrivateNetAllow,
    /// The XMPP ping interval. Never whitespace — see the module docs.
    pub keepalive: Duration,
    /// Per-step budget for the connect/SASL/bind/join handshake.
    pub handshake_timeout: Duration,
}

impl XmppConfig {
    /// A config for `room` at `url`, with the egress guard fully on and no credentials.
    pub fn new(url: impl Into<String>, room: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            room: room.into(),
            domain: None,
            user: None,
            password: None,
            muc_password: None,
            private_net: PrivateNetAllow::None,
            keepalive: DEFAULT_XMPP_KEEPALIVE,
            handshake_timeout: DEFAULT_XMPP_HANDSHAKE_TIMEOUT,
        }
    }

    /// Set the XMPP domain for the stream open.
    pub fn domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// Authenticate with SASL `PLAIN` as `user`. Without this the backend uses `ANONYMOUS`.
    pub fn credentials(mut self, user: impl Into<String>, password: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self.password = Some(password.into());
        self
    }

    /// The MUC's room password.
    pub fn muc_password(mut self, password: impl Into<String>) -> Self {
        self.muc_password = Some(password.into());
        self
    }

    /// Allow the endpoint to resolve to a private/loopback address — a self-hosted MUC, or a test
    /// double. This is the scoped grant the egress guard takes, not a bypass of it.
    pub fn allow_private_net(mut self, allow: bool) -> Self {
        self.private_net = PrivateNetAllow::from_legacy_bool(allow);
        self
    }

    /// Override the XMPP ping interval.
    pub fn keepalive(mut self, every: Duration) -> Self {
        self.keepalive = every;
        self
    }

    /// The domain to open the stream to: the configured one, else the endpoint's host.
    fn domain_or_host(&self) -> String {
        if let Some(domain) = &self.domain {
            return domain.clone();
        }
        url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    }
}

impl fmt::Debug for XmppConfig {
    /// Redacts every credential. A room config reaches logs and error paths, and a secret that
    /// reaches either is a secret that reaches the model.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XmppConfig")
            .field("url", &self.url)
            .field("room", &self.room)
            .field("domain", &self.domain)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field(
                "muc_password",
                &self.muc_password.as_ref().map(|_| "<redacted>"),
            )
            .field("private_net", &self.private_net)
            .field("keepalive", &self.keepalive)
            .field("handshake_timeout", &self.handshake_timeout)
            .finish()
    }
}

/// A generic XMPP MUC as a [`Room`]: prosody, ejabberd, or a JaaS tenant.
pub struct XmppMucRoom {
    config: XmppConfig,
    /// The address the declaration named.
    configured: RoomId,
    /// The address the **server** named, learned from our own presence. Set once, so [`Room::id`] can
    /// hand back a borrow that outlives the join.
    observed: OnceLock<RoomId>,
    /// The live session, `None` before joining and after leaving.
    session: Mutex<Option<session::Session>>,
    /// Occupants as presence has them, shared with the socket loop.
    occupants: Arc<Mutex<Vec<Occupant>>>,
}

impl XmppMucRoom {
    /// A room that will join `config.room` when [`Room::join`] is called. Nothing connects here.
    pub fn new(config: XmppConfig) -> Self {
        Self {
            configured: RoomId::new(config.room.clone()),
            config,
            observed: OnceLock::new(),
            session: Mutex::new(None),
            occupants: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The live session, cloned so the room's mutex is never held across an `await`. A plain error
    /// rather than a panic when nothing has joined.
    fn session(&self) -> Result<session::Session> {
        self.session
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::Other("xmpp: not joined".into()))
    }

    /// Address one outbound message stanza. Room text is untrusted input in both directions; the
    /// stanza writer escapes it.
    fn message(&self, to: &str, kind: &str, text: &str) -> Element {
        stanza("message")
            .attr("to", to)
            .attr("type", kind)
            .child(Element::new("body", NS_CLIENT).text(text))
    }
}

#[async_trait]
impl Room for XmppMucRoom {
    /// The room's address — the server's spelling once we have joined, the configured one before.
    fn id(&self) -> &RoomId {
        self.observed.get().unwrap_or(&self.configured)
    }

    async fn join(&self, identity: &RoomIdentity) -> Result<RoomStream> {
        if self.session.lock().unwrap().is_some() {
            return Err(Error::Other("xmpp: already joined".into()));
        }
        let (events, stream) = room_event_channel();
        let joined =
            session::connect_and_join(&self.config, identity, events, self.occupants.clone())
                .await?;
        // The server's spelling of the room, not one rebuilt from `config.room`.
        let _ = self.observed.set(RoomId::new(joined.room_jid));
        *self.session.lock().unwrap() = Some(joined.session);
        Ok(stream)
    }

    async fn occupants(&self) -> Result<Vec<Occupant>> {
        Ok(self.occupants.lock().unwrap().clone())
    }

    async fn say(&self, text: &str) -> Result<()> {
        let session = self.session()?;
        let stanza = self.message(self.id().as_str(), "groupchat", text);
        session.send(&stanza).await
    }

    async fn whisper(&self, to: &OccupantId, text: &str) -> Result<()> {
        let session = self.session()?;
        let stanza = self.message(to.as_str(), "chat", text);
        session.send(&stanza).await
    }

    async fn leave(&self) -> Result<()> {
        // Taken, not borrowed: leaving twice is not an error, and a session that has been ended must
        // not be usable for a `say` afterwards.
        let Some(session) = self.session.lock().unwrap().take() else {
            return Ok(());
        };
        let unavailable = stanza("presence")
            .attr("to", &session.self_jid)
            .attr("type", "unavailable");
        // Best effort: if the socket has already gone, we are out of the room either way. The trait
        // says leaving a room we are not in is not an error.
        let _ = session.send(&unavailable).await;
        session.end();
        self.occupants.lock().unwrap().retain(|o| !o.is_self);
        Ok(())
    }
}

/// A room's declaration mapped onto a config — used by the `room` channel adapter.
impl XmppConfig {
    /// Build from the channel declaration's settings. `url` is required for this backend: a MUC JID
    /// does not say where to connect.
    pub(crate) fn from_settings(settings: &crate::config::RoomSettings) -> StdResult<Self, String> {
        let url = settings
            .url
            .clone()
            .ok_or("the `xmpp` room backend needs a `url` (the RFC 7395 endpoint)")?;
        let mut config = Self::new(url, settings.room.clone());
        config.domain = settings.domain.clone();
        config.user = settings.user.clone();
        config.password = settings.password.clone();
        config.muc_password = settings.muc_password.clone();
        config.private_net = PrivateNetAllow::from_legacy_bool(settings.allow_private_net);
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debug_rendering_never_carries_a_credential() {
        let config = XmppConfig::new(
            "wss://example.org/xmpp-websocket",
            "standup@conference.example.org",
        )
        .credentials("agent", "hunter2")
        .muc_password("open-sesame");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(!rendered.contains("open-sesame"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("agent"), "the username is not a secret");
    }

    #[test]
    fn the_egress_guard_is_on_unless_it_is_explicitly_relaxed() {
        let config = XmppConfig::new("wss://example.org/x", "r@c");
        assert_eq!(config.private_net, PrivateNetAllow::None);
        assert_eq!(
            config.allow_private_net(true).private_net,
            PrivateNetAllow::Any
        );
    }

    #[test]
    fn the_stream_domain_falls_back_to_the_endpoint_host() {
        assert_eq!(
            XmppConfig::new(
                "wss://chat.example.org/xmpp-websocket",
                "r@conference.example.org"
            )
            .domain_or_host(),
            "chat.example.org"
        );
        assert_eq!(
            XmppConfig::new("wss://8x8.vc/tenant/xmpp-websocket", "r@c")
                .domain("example.org")
                .domain_or_host(),
            "example.org"
        );
    }

    #[tokio::test]
    async fn a_room_that_never_joined_reports_its_configured_address_and_refuses_to_speak() {
        let room = XmppMucRoom::new(XmppConfig::new(
            "wss://example.org/x",
            "StandUp@conference.example.org",
        ));
        assert_eq!(room.id().as_str(), "StandUp@conference.example.org");
        assert!(room.say("hi").await.is_err());
        assert!(room.occupants().await.unwrap().is_empty());
        assert!(room.leave().await.is_ok(), "leaving is idempotent");
    }
}

//! [`JaasRoom`] — the **vendor** [`Room`] backend (D-206): Brave Talk and any 8x8 JaaS tenant.
//!
//! This is not a second room implementation. Everything after the token is D-205's machinery: the
//! handshake, the MUC, the stanzas and the socket loop are [`XmppMucRoom`]'s, and this module owns
//! exactly the two things that are vendor-specific — **where the guest token comes from**, and
//! **what happens when it expires three hours later**.
//!
//! ## The handshake, as the 2026-07-30 spike measured it
//!
//! ```text
//! OPTIONS https://talk.brave.com/api/v1/rooms/<room>   → x-csrf-token + _gorilla_csrf cookie
//! PUT     https://talk.brave.com/api/v1/rooms/<room>   → 200 {"jwt": "…"}   (POST = create, gated)
//! POST    https://8x8.vc/<tenant>/conference-request/v1?room=<room>
//!         Authorization: Bearer <jwt>
//!                                                     → {"room": "<room-lowercased>@conference.…"}
//! wss://8x8.vc/<tenant>/xmpp-websocket?room=<room>&token=<jwt>   — SASL ANONYMOUS
//! ```
//!
//! Three consequences are load-bearing, and each is a test rather than a comment alone
//! (`tests/jaas_room.rs`):
//!
//! - **The MUC JID comes from the conference-request response**, never from the JWT: the response
//!   lowercases the room name and the `room` claim does not, so a locally-rebuilt address is wrong in
//!   exactly the way that is hard to see.
//! - **The token rides the endpoint URL and SASL is `ANONYMOUS`.** JaaS offers only `ANONYMOUS`, with
//!   or without the token, and refuses `PLAIN` with the JWT as the password (`<invalid-mechanism/>`).
//! - **The token expires in three hours and the session must not.** [`JaasRoom`] re-mints ahead of
//!   the expiry and re-joins underneath its consumer, which sees neither `Ended` nor a second
//!   `Joined` for anyone it already knows about.
//!
//! ## The network boundary
//!
//! [`JaasTokens`] is the seam every vendor HTTP call goes through — the same shape
//! `flux_plugin::pack::Fetcher` uses, and for the same reason: it is scoped to `(room, token)` rather
//! than to a caller-supplied URL, so tests inject a hermetic fixture and **no test in this repo ever
//! reaches Brave or 8x8**.
//!
//! ## Safety
//!
//! **A guest JWT is a secret, and it rides a URL.** [`GuestToken`]'s `Debug` redacts it, it is never
//! logged, and the XMPP backend renders an endpoint in an error message *without its query string*
//! (`xmpp::endpoint_for_display`) so a failed connect cannot publish it.
//!
//! Joining grants no authority: as everywhere else in this module, a room-sourced turn meets the
//! ordinary `Executor` + approver envelope.
//!
//! ## Acceptable use
//!
//! Brave's token endpoint is public and unauthenticated, and the spike used it exactly as the
//! open-source client does — against a room the author was invited to. **This backend is built for
//! own-room use**: it takes one configured room name and joins it. It does not enumerate rooms, does
//! not discover them, and has no batch or multi-room path. Anything beyond own-room use is a
//! different posture and needs Brave's acceptable-use policy read first — prefer an own JaaS tenant,
//! or D-205's generic backend. See `docs/designs/meeting-rooms.md`, "Open questions".

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use flux_core::{Error, Result};
use flux_system::net::PrivateNetAllow;

use super::xmpp::{
    XmppConfig, XmppMucRoom, DEFAULT_XMPP_HANDSHAKE_TIMEOUT, DEFAULT_XMPP_KEEPALIVE,
};
use super::{
    room_event_channel, Occupant, OccupantId, Room, RoomEvent, RoomEventSender, RoomId,
    RoomIdentity, RoomStream,
};

/// The 8x8 signalling host every JaaS tenant — Brave Talk's included — lives behind.
pub const DEFAULT_JAAS_SIGNALLING: &str = "wss://8x8.vc";

/// How far ahead of the token's expiry the re-mint happens. Five minutes out of a three-hour token:
/// far enough that a slow token service does not race the expiry, short enough that a re-join is
/// rare.
pub const DEFAULT_JAAS_REFRESH_LEAD: Duration = Duration::from_secs(300);

/// The floor on the re-mint interval. A token service that hands out already-expired tokens must not
/// turn the refresh loop into a request storm against the vendor.
pub const MIN_JAAS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// How long a failed re-mint waits before trying again. If the token expires first the server ends
/// the session and `Ended` reaches the consumer, which is the honest outcome — a room flux is no
/// longer authorized for is not a room it is in.
pub const JAAS_REFRESH_RETRY: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The token, and the seam it comes from
// ---------------------------------------------------------------------------

/// A JaaS guest token: **the JWT is a secret**, plus the two claims flux reads out of it.
///
/// flux never verifies the signature — that is the server's job. It needs the tenant (to build the
/// signalling URL) and the expiry (to know when to re-mint), and nothing else.
#[derive(Clone)]
pub struct GuestToken {
    jwt: String,
    tenant: String,
    room: Option<String>,
    expires_at: SystemTime,
}

/// The claims flux reads. Everything else the vendor sends (`context.features`, `moderator`, …) is
/// the server's business.
#[derive(Deserialize)]
struct GuestClaims {
    /// The JaaS tenant — `vpaas-magic-cookie-…`. Brave's own tenant for a Brave Talk token.
    sub: String,
    /// Unix expiry. The observed guest token is valid for 10800 s (3 h).
    exp: u64,
    /// The room, in the case the *caller* spelled it. The MUC JID lowercases it; this does not.
    #[serde(default)]
    room: Option<String>,
}

impl GuestToken {
    /// Read a minted JWT. Only the payload is decoded; the signature is neither parsed nor checked.
    pub fn parse(jwt: impl Into<String>) -> Result<Self> {
        let jwt = jwt.into();
        let payload = jwt.split('.').nth(1).ok_or_else(|| {
            Error::Other("jaas: the token service returned a malformed JWT".into())
        })?;
        // JWT payloads are base64url without padding; tolerate a padded one rather than refusing a
        // token that a vendor happens to pad. The token itself never reaches this error.
        let url_safe = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let bytes = url_safe
            .decode(payload)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
            .map_err(|_| Error::Other("jaas: the JWT payload is not base64url".into()))?;
        let claims: GuestClaims = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Other(format!("jaas: the JWT payload is not claims: {e}")))?;
        Ok(Self {
            jwt,
            tenant: claims.sub,
            room: claims.room,
            expires_at: UNIX_EPOCH + Duration::from_secs(claims.exp),
        })
    }

    /// The raw JWT. **A secret**: it goes into the endpoint's query string and an `Authorization`
    /// header, and nowhere else.
    pub fn jwt(&self) -> &str {
        &self.jwt
    }

    /// The JaaS tenant this token was minted for (`sub`).
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// The `room` claim — the room in the case it was asked for, *not* the MUC JID.
    pub fn room_claim(&self) -> Option<&str> {
        self.room.as_deref()
    }

    /// When the token stops being accepted.
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    /// How long the token still has. `ZERO` once it has expired.
    pub fn expires_in(&self) -> Duration {
        self.expires_at
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO)
    }
}

impl fmt::Debug for GuestToken {
    /// Redacts the JWT. A token reaches logs and error paths, and a secret that reaches either is a
    /// secret that reaches the model.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestToken")
            .field("jwt", &"<redacted>")
            .field("tenant", &self.tenant)
            .field("room", &self.room)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// What focus allocation answered — the part of it flux acts on.
#[derive(Debug, Clone)]
pub struct Conference {
    /// The MUC JID **as the server spelled it** (`<room-lowercased>@conference.<tenant>.8x8.vc`).
    /// Take this, never a locally-built one: the JWT's `room` claim keeps the original case.
    pub room_jid: String,
    /// The conference focus, as returned (`focus@auth.8x8.vc`). Carried for diagnostics; the MUC
    /// itself is what flux joins.
    pub focus_jid: Option<String>,
}

/// The vendor network boundary — **every** JaaS HTTP call goes through here.
///
/// Two operations, both scoped to a room and a token rather than to a caller-supplied URL, so the
/// own-room posture holds structurally: there is no shape of this trait that enumerates rooms.
/// Tests inject a hermetic fixture, which is why no test in this repo reaches Brave or 8x8.
#[async_trait]
pub trait JaasTokens: Send + Sync {
    /// Mint a guest token for `room` — Brave's `OPTIONS` + `PUT /api/v1/rooms/<room>` handshake, or
    /// an own-tenant JWT signed from the operator's JaaS API key.
    async fn guest_token(&self, room: &str) -> Result<GuestToken>;

    /// Allocate focus for `room` — `POST /<tenant>/conference-request/v1` with the JWT as Bearer.
    /// JaaS wants this *before* signalling, and its answer carries the MUC JID flux must use.
    async fn conference(&self, room: &str, token: &GuestToken) -> Result<Conference>;
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How to join one JaaS room. The token service is a [`JaasTokens`] rather than a field here: where
/// the JWT comes from is the *only* difference between Brave Talk and an own tenant.
#[derive(Debug, Clone)]
pub struct JaasConfig {
    /// The room, in the case a human spells it. Passed to the token service and carried on the
    /// endpoint URL; the MUC JID comes back lowercased from focus allocation.
    pub room: String,
    /// The signalling base — [`DEFAULT_JAAS_SIGNALLING`]. The `/<tenant>/xmpp-websocket` path is
    /// appended from the token's own `sub` claim.
    pub signalling: String,
    /// Whether the signalling endpoint may resolve to a private/loopback address. Off by default:
    /// this is the scoped grant `flux_system::net`'s egress guard takes, not a bypass of it.
    pub private_net: PrivateNetAllow,
    /// How far ahead of the expiry to re-mint — [`DEFAULT_JAAS_REFRESH_LEAD`].
    pub refresh_lead: Duration,
    /// The XMPP ping interval, handed to the backend underneath.
    pub keepalive: Duration,
    /// Per-step budget for the connect/SASL/bind/join handshake.
    pub handshake_timeout: Duration,
}

impl JaasConfig {
    /// A config for `room` against Brave Talk's tenant defaults, with the egress guard fully on.
    pub fn new(room: impl Into<String>) -> Self {
        Self {
            room: room.into(),
            signalling: DEFAULT_JAAS_SIGNALLING.to_string(),
            private_net: PrivateNetAllow::None,
            refresh_lead: DEFAULT_JAAS_REFRESH_LEAD,
            keepalive: DEFAULT_XMPP_KEEPALIVE,
            handshake_timeout: DEFAULT_XMPP_HANDSHAKE_TIMEOUT,
        }
    }

    /// Point signalling somewhere other than [`DEFAULT_JAAS_SIGNALLING`] — a self-hosted Jitsi, or a
    /// test double.
    pub fn signalling(mut self, base: impl Into<String>) -> Self {
        self.signalling = base.into();
        self
    }

    /// Allow the signalling endpoint to resolve to a private/loopback address. The egress guard's
    /// scoped grant, not a second guard.
    pub fn allow_private_net(mut self, allow: bool) -> Self {
        self.private_net = PrivateNetAllow::from_legacy_bool(allow);
        self
    }

    /// Override how far ahead of the expiry the token is re-minted.
    pub fn refresh_lead(mut self, lead: Duration) -> Self {
        self.refresh_lead = lead;
        self
    }

    /// Override the XMPP ping interval.
    pub fn keepalive(mut self, every: Duration) -> Self {
        self.keepalive = every;
        self
    }

    /// When to re-mint a token that expires in `remaining`, floored so a short-lived token cannot
    /// turn the refresh loop into a request storm.
    fn refresh_after(&self, remaining: Duration) -> Duration {
        remaining
            .saturating_sub(self.refresh_lead)
            .max(MIN_JAAS_REFRESH_INTERVAL)
    }
}

// ---------------------------------------------------------------------------
// The room
// ---------------------------------------------------------------------------

/// A Brave Talk / JaaS room as a [`Room`]: guest-token acquisition and refresh over D-205's MUC.
pub struct JaasRoom {
    config: JaasConfig,
    tokens: Arc<dyn JaasTokens>,
    /// The room name the declaration named — all [`Room::id`] can answer before focus allocation.
    configured: RoomId,
    /// The MUC JID **focus allocation** named. Set once, so [`Room::id`] hands back a borrow that
    /// outlives the join; a re-join is the same room, so it never changes.
    observed: OnceLock<RoomId>,
    /// The live inner session, swapped underneath by the refresh. `None` before joining and after
    /// leaving.
    inner: Arc<Mutex<Option<Arc<XmppMucRoom>>>>,
    /// Ends the forwarding/refresh task.
    cancel: Mutex<Option<CancellationToken>>,
}

impl JaasRoom {
    /// A room that will mint a token and join when [`Room::join`] is called. Nothing connects here,
    /// and no token is minted here.
    pub fn new(config: JaasConfig, tokens: Arc<dyn JaasTokens>) -> Self {
        Self {
            configured: RoomId::new(config.room.clone()),
            config,
            tokens,
            observed: OnceLock::new(),
            inner: Arc::new(Mutex::new(None)),
            cancel: Mutex::new(None),
        }
    }

    /// The live inner room, cloned so this room's mutex is never held across an `await`.
    fn current(&self) -> Result<Arc<XmppMucRoom>> {
        self.inner
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::Other("jaas: not joined".into()))
    }
}

#[async_trait]
impl Room for JaasRoom {
    /// The MUC JID focus allocation named once we have joined; the configured room name before.
    fn id(&self) -> &RoomId {
        self.observed.get().unwrap_or(&self.configured)
    }

    async fn join(&self, identity: &RoomIdentity) -> Result<RoomStream> {
        if self.inner.lock().unwrap().is_some() {
            return Err(Error::Other("jaas: already joined".into()));
        }
        let joined = mint_and_join(&self.config, &self.tokens, identity).await?;
        // The server's spelling of the room, from the conference-request response.
        let _ = self
            .observed
            .set(RoomId::new(joined.conference.room_jid.clone()));
        *self.inner.lock().unwrap() = Some(joined.room.clone());

        let (events, stream) = room_event_channel();
        let cancel = CancellationToken::new();
        *self.cancel.lock().unwrap() = Some(cancel.clone());
        tokio::spawn(
            SessionPump {
                config: self.config.clone(),
                tokens: self.tokens.clone(),
                identity: identity.clone(),
                inner: self.inner.clone(),
                events,
                cancel,
            }
            .run(joined.stream, joined.refresh_after),
        );
        Ok(stream)
    }

    async fn occupants(&self) -> Result<Vec<Occupant>> {
        // Cloned out of the mutex before the `await`: the guard must not live across one.
        let joined = { self.inner.lock().unwrap().clone() };
        match joined {
            Some(room) => room.occupants().await,
            None => Ok(Vec::new()),
        }
    }

    async fn say(&self, text: &str) -> Result<()> {
        self.current()?.say(text).await
    }

    async fn whisper(&self, to: &OccupantId, text: &str) -> Result<()> {
        self.current()?.whisper(to, text).await
    }

    async fn leave(&self) -> Result<()> {
        // The refresh task first: a re-mint racing our departure would rejoin a room we just left.
        if let Some(cancel) = self.cancel.lock().unwrap().take() {
            cancel.cancel();
        }
        // Taken, not borrowed: leaving twice is not an error.
        let Some(room) = self.inner.lock().unwrap().take() else {
            return Ok(());
        };
        room.leave().await
    }
}

// ---------------------------------------------------------------------------
// Minting, joining, and staying joined
// ---------------------------------------------------------------------------

/// One successful mint-and-join.
struct Joined {
    room: Arc<XmppMucRoom>,
    stream: RoomStream,
    conference: Conference,
    refresh_after: Duration,
}

/// Mint a guest token, allocate focus, and join the MUC the response named.
async fn mint_and_join(
    config: &JaasConfig,
    tokens: &Arc<dyn JaasTokens>,
    identity: &RoomIdentity,
) -> Result<Joined> {
    let token = tokens.guest_token(&config.room).await?;
    let conference = tokens.conference(&config.room, &token).await?;

    let mut xmpp = XmppConfig::new(endpoint(config, &token)?, conference.room_jid.clone());
    // Deliberately **no** `user`/`password`: that selects SASL `ANONYMOUS`, the only mechanism JaaS
    // offers. `PLAIN` with the JWT as the password is refused `<invalid-mechanism/>` — authorization
    // rides the endpoint URL and happens at focus.
    xmpp.domain = stream_domain(&conference.room_jid);
    xmpp.private_net = config.private_net.clone();
    xmpp.keepalive = config.keepalive;
    xmpp.handshake_timeout = config.handshake_timeout;

    let room = Arc::new(XmppMucRoom::new(xmpp));
    let stream = room.join(identity).await?;
    let refresh_after = config.refresh_after(token.expires_in());
    Ok(Joined {
        room,
        stream,
        conference,
        refresh_after,
    })
}

/// The signalling endpoint: `<base>/<tenant>/xmpp-websocket?room=…&token=…`.
///
/// **The returned URL carries the guest token.** It goes to the egress guard and the dialer, and is
/// never logged — `super::xmpp` renders an endpoint without its query when an error names it.
fn endpoint(config: &JaasConfig, token: &GuestToken) -> Result<String> {
    let base = config.signalling.trim_end_matches('/');
    let mut url = url::Url::parse(&format!("{base}/{}/xmpp-websocket", token.tenant()))
        .map_err(|e| Error::Other(format!("jaas: bad signalling base `{base}`: {e}")))?;
    url.query_pairs_mut()
        .append_pair("room", &config.room)
        .append_pair("token", token.jwt());
    Ok(url.into())
}

/// The XMPP domain to open the stream to, derived from the MUC JID focus allocation returned.
///
/// A MUC lives on a `conference.` component of its domain (`room@conference.<tenant>.8x8.vc` is a
/// room on `<tenant>.8x8.vc`), and the stream `<open to=…>` wants the domain, not the component.
fn stream_domain(room_jid: &str) -> Option<String> {
    let service = room_jid.split_once('@')?.1;
    Some(
        service
            .strip_prefix("conference.")
            .unwrap_or(service)
            .to_string(),
    )
}

/// Forwards the inner session's events to the consumer, and re-mints the guest token underneath it.
struct SessionPump {
    config: JaasConfig,
    tokens: Arc<dyn JaasTokens>,
    identity: RoomIdentity,
    inner: Arc<Mutex<Option<Arc<XmppMucRoom>>>>,
    events: RoomEventSender,
    cancel: CancellationToken,
}

impl SessionPump {
    /// Pump until the consumer goes away, the session ends, or we are told to leave.
    ///
    /// The refresh is *transparent* by construction: the new session's presence replay repeats every
    /// occupant already known, and a `Joined` for someone we already have is not news. The deliberate
    /// `leave` of the old session emits no `Ended`, so the consumer sees an uninterrupted room.
    async fn run(self, mut stream: RoomStream, mut refresh_after: Duration) {
        // Occupants the consumer has already been told about, so a re-join's replay is not a second
        // arrival. Note the honest limit: someone who left while we were between sessions is dropped
        // from `Room::occupants` (which reads the live session) but produces no `Left` here.
        let mut known: HashSet<OccupantId> = HashSet::new();
        loop {
            // Forward this session's events until its token is due for replacement. Every other way
            // out of this inner loop is a way out of the room.
            let refresh_at = tokio::time::Instant::now() + refresh_after;
            loop {
                tokio::select! {
                    _ = self.cancel.cancelled() => return,
                    _ = tokio::time::sleep_until(refresh_at) => break,
                    event = stream.recv() => {
                        // `None` is the inner session's transport gone for good; it has already sent
                        // `Ended` if that was a surprise.
                        let Some(event) = event else { return };
                        if !self.forward(event, &mut known).await {
                            return;
                        }
                    }
                }
            }

            match mint_and_join(&self.config, &self.tokens, &self.identity).await {
                Ok(joined) => {
                    // Swap first, leave second: the old session must not be the current one for any
                    // moment in which a `say` could reach it.
                    let previous = { self.inner.lock().unwrap().replace(joined.room) };
                    stream = joined.stream;
                    refresh_after = joined.refresh_after;
                    if let Some(previous) = previous {
                        let _ = previous.leave().await;
                    }
                }
                // The vendor is unreachable or refused. Keep the session we have — it is still valid
                // until the expiry — and try again. If it does expire first the server ends the
                // session and `Ended` reaches the consumer through the branch above.
                Err(e) => {
                    eprintln!("jaas: could not refresh the guest token, retrying: {e}");
                    refresh_after = JAAS_REFRESH_RETRY;
                }
            }
        }
    }

    /// Forward one event, suppressing a re-join's replayed arrivals. `false` once the consumer is
    /// gone.
    async fn forward(&self, event: RoomEvent, known: &mut HashSet<OccupantId>) -> bool {
        // Exhaustive on purpose (D-204): a new `RoomEvent` variant must fail to compile here rather
        // than be dropped on the floor by a wildcard.
        let event = match event {
            RoomEvent::Joined { occupant } => {
                if !known.insert(occupant.id.clone()) {
                    return true;
                }
                RoomEvent::Joined { occupant }
            }
            RoomEvent::Left { occupant } => {
                known.remove(&occupant);
                RoomEvent::Left { occupant }
            }
            message @ RoomEvent::Message { .. } => message,
            RoomEvent::Ended => RoomEvent::Ended,
        };
        self.events.send(event).await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token whose payload says what the arguments say. The signature is not checked by anything
    /// in flux — the server verifies it — so it is a placeholder.
    fn token(tenant: &str, room: &str, exp: u64) -> GuestToken {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let claims = serde_json::json!({ "sub": tenant, "room": room, "exp": exp });
        GuestToken::parse(format!(
            "{}.{}.{}",
            b64.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            b64.encode(serde_json::to_vec(&claims).unwrap()),
            b64.encode(b"signature")
        ))
        .unwrap()
    }

    #[test]
    fn the_tenant_and_expiry_are_read_out_of_the_jwt_and_the_jwt_stays_secret() {
        let t = token("vpaas-magic-cookie-a4818bd", "StandUp", 1_800_000_000);
        assert_eq!(t.tenant(), "vpaas-magic-cookie-a4818bd");
        assert_eq!(t.room_claim(), Some("StandUp"));
        assert_eq!(
            t.expires_at(),
            UNIX_EPOCH + Duration::from_secs(1_800_000_000)
        );
        let rendered = format!("{t:?}");
        assert!(!rendered.contains(t.jwt()), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn a_malformed_token_is_an_error_and_never_quotes_itself() {
        for bad in ["", "not-a-jwt", "a.!!!.c", "a.e30.c"] {
            let failed = GuestToken::parse(bad);
            // `a.e30.c` decodes to `{}` — claims without `sub`/`exp` are still malformed.
            let message = failed.expect_err(bad).to_string();
            assert!(!message.contains(bad) || bad.is_empty(), "{message}");
        }
    }

    #[test]
    fn the_endpoint_carries_the_tenant_the_room_and_the_token() {
        let t = token("vpaas-magic-cookie-a4818bd", "StandUp", 1_800_000_000);
        let url = endpoint(&JaasConfig::new("StandUp"), &t).unwrap();
        assert!(
            url.starts_with("wss://8x8.vc/vpaas-magic-cookie-a4818bd/xmpp-websocket?"),
            "{url}"
        );
        assert!(url.contains("room=StandUp"), "{url}");
        assert!(url.contains(&format!("token={}", t.jwt())), "{url}");

        // A room name with URL-significant characters is encoded, not concatenated.
        let odd = endpoint(&JaasConfig::new("a b&c=d"), &t).unwrap();
        assert!(odd.contains("room=a+b%26c%3Dd"), "{odd}");
    }

    #[test]
    fn the_stream_domain_drops_the_muc_component() {
        assert_eq!(
            stream_domain("standup@conference.vpaas-magic-cookie-a4818bd.8x8.vc").as_deref(),
            Some("vpaas-magic-cookie-a4818bd.8x8.vc")
        );
        // A service that does not use the conventional component still gets a usable domain.
        assert_eq!(
            stream_domain("standup@muc.example.org").as_deref(),
            Some("muc.example.org")
        );
        assert_eq!(stream_domain("not-a-jid"), None);
    }

    #[test]
    fn the_refresh_lands_ahead_of_the_expiry_and_never_hot_loops() {
        let config = JaasConfig::new("StandUp");
        // The measured case: a 3 h token, re-minted 5 minutes out.
        assert_eq!(
            config.refresh_after(Duration::from_secs(10800)),
            Duration::from_secs(10500)
        );
        // A token that is already inside the lead — or expired — is re-minted at the floor, not
        // instantly and not in a loop.
        assert_eq!(
            config.refresh_after(Duration::from_secs(60)),
            MIN_JAAS_REFRESH_INTERVAL
        );
        assert_eq!(
            config.refresh_after(Duration::ZERO),
            MIN_JAAS_REFRESH_INTERVAL
        );
    }

    #[test]
    fn the_egress_guard_is_on_unless_it_is_explicitly_relaxed() {
        assert_eq!(
            JaasConfig::new("StandUp").private_net,
            PrivateNetAllow::None
        );
        assert_eq!(
            JaasConfig::new("StandUp")
                .allow_private_net(true)
                .private_net,
            PrivateNetAllow::Any
        );
    }

    #[tokio::test]
    async fn a_room_that_never_joined_reports_its_configured_name_and_refuses_to_speak() {
        struct NoTokens;
        #[async_trait]
        impl JaasTokens for NoTokens {
            async fn guest_token(&self, _room: &str) -> Result<GuestToken> {
                Err(Error::Other("no".into()))
            }
            async fn conference(&self, _room: &str, _token: &GuestToken) -> Result<Conference> {
                Err(Error::Other("no".into()))
            }
        }
        let room = JaasRoom::new(JaasConfig::new("StandUp"), Arc::new(NoTokens));
        assert_eq!(room.id().as_str(), "StandUp");
        assert!(room.say("hi").await.is_err());
        assert!(room.occupants().await.unwrap().is_empty());
        assert!(room.leave().await.is_ok(), "leaving is idempotent");
    }
}

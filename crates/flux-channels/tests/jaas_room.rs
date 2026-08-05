//! `JaasRoom` against a **fake token service** and the in-process XMPP double (D-206).
//!
//! This suite never reaches Brave, 8x8, or any other vendor. The token service is behind the
//! [`JaasTokens`] seam — the same shape `flux_plugin::pack::Fetcher` uses — so the handshake's
//! *consequences* (which URL is dialled, which SASL mechanism is offered, which MUC JID the room
//! answers with, what happens at the 3 h expiry) are asserted against a double bound on loopback.
//!
//! Three of these assertions exist because the 2026-07-30 spike paid for them in wall-clock time,
//! and the design's Feasibility section records why:
//!
//! - **the token rides the WebSocket URL, and SASL is `ANONYMOUS`** — JaaS offers only `ANONYMOUS`,
//!   and `PLAIN` with the JWT as the password is refused `<invalid-mechanism/>`. Asserted so a future
//!   maintainer does not "fix" it the wrong way;
//! - **the MUC JID comes from the conference-request response** — that response lowercases the room
//!   name while the JWT's `room` claim keeps the original case;
//! - **the guest token is a secret and rides a URL** — so it must survive neither a `Debug` rendering
//!   nor a failed connect's error message.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use tokio::sync::Semaphore;

use flux_channels::rooms::{
    Conference, GuestToken, JaasConfig, JaasRoom, JaasTokens, Room, RoomEvent, RoomIdentity,
};
use flux_core::Result;

use support::xmpp_double::{XmppDouble, ROOM_JID_SERVER};

/// The room as a human spells it — mixed case, the way the JWT's `room` claim keeps it. The
/// conference-request response lowercases it, which is the asymmetry [`ROOM_JID_SERVER`] carries.
const ROOM: &str = "StandUp";

/// A JaaS tenant id, in the `vpaas-magic-cookie-…` shape the JWT's `sub` claim carries.
const TENANT: &str = "vpaas-magic-cookie-testtenant";

/// A guest JWT shaped like the one the spike observed: RS256 header, the claims the backend reads,
/// and a signature nothing here verifies (verification is the *server's* job — flux only needs the
/// tenant and the expiry out of it).
fn guest_jwt(room: &str, ttl: Duration) -> String {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let exp = (SystemTime::now() + ttl)
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let header = b64.encode(br#"{"alg":"RS256","typ":"JWT","kid":"vpaas-magic-cookie/abc"}"#);
    let claims = serde_json::json!({
        "aud": "jitsi",
        "iss": "chat",
        "sub": TENANT,
        "room": room,
        "exp": exp,
        "context": { "user": { "moderator": "false" } },
    });
    let payload = b64.encode(serde_json::to_vec(&claims).unwrap());
    // A distinct, obviously-fake signature per mint, so a test can tell two tokens apart.
    let signature = b64.encode(format!("not-a-signature-{exp}-{}", rand_ish()).as_bytes());
    format!("{header}.{payload}.{signature}")
}

/// A monotonically-increasing nonce. Two tokens minted in the same second must still differ, which
/// is how the refresh test proves the *second* connection carried a *new* token.
fn rand_ish() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// The token service, faked. Records every JWT it minted so a test can assert which one was dialled
/// with, and mints with a short TTL so the 3 h refresh path is exercised in seconds.
///
/// `gate`, when set, holds every mint *after the first* until a test releases it. That is how the
/// departure race is opened deliberately — a token service that answers instantly never opens it,
/// which is exactly why that race shipped unnoticed.
struct FakeTokens {
    ttl: Duration,
    minted: Mutex<Vec<String>>,
    gate: Option<Arc<Semaphore>>,
    /// How many mints have entered the gate. A test polls this to know the window is open.
    blocked: Arc<AtomicUsize>,
}

impl FakeTokens {
    fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            minted: Mutex::new(Vec::new()),
            gate: None,
            blocked: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Hold every mint after the first until the returned gate is released.
    fn gated(mut self) -> (Self, Arc<Semaphore>, Arc<AtomicUsize>) {
        let gate = Arc::new(Semaphore::new(0));
        self.gate = Some(gate.clone());
        let blocked = self.blocked.clone();
        (self, gate, blocked)
    }

    fn minted(&self) -> Vec<String> {
        self.minted.lock().unwrap().clone()
    }
}

#[async_trait]
impl JaasTokens for FakeTokens {
    async fn guest_token(&self, room: &str) -> Result<GuestToken> {
        if let Some(gate) = &self.gate {
            let first = self.minted.lock().unwrap().is_empty();
            if !first {
                self.blocked.fetch_add(1, Ordering::SeqCst);
                let _permit = gate.acquire().await.expect("the gate outlives the mint");
            }
        }
        let jwt = guest_jwt(room, self.ttl);
        self.minted.lock().unwrap().push(jwt.clone());
        GuestToken::parse(jwt)
    }

    async fn conference(&self, _room: &str, _token: &GuestToken) -> Result<Conference> {
        // The vendor's answer, including the lowercasing the JWT does not do.
        Ok(Conference {
            room_jid: ROOM_JID_SERVER.to_string(),
            focus_jid: Some("focus@auth.8x8.vc".to_string()),
        })
    }
}

/// A room pointed at `double`'s loopback endpoint, with a token that expires in `ttl` and a refresh
/// that fires `lead` before that.
fn room_for(double: &XmppDouble, tokens: Arc<FakeTokens>, lead: Duration) -> JaasRoom {
    JaasRoom::new(
        JaasConfig::new(ROOM)
            .signalling(double.ws_base())
            // The double is on loopback, which the egress guard blocks by default. A scoped grant is
            // the supported way to reach a private address — not a second, hand-rolled guard.
            .allow_private_net(true)
            .refresh_lead(lead),
        tokens,
    )
}

#[tokio::test]
async fn concurrent_initial_joins_construct_exactly_one_session() {
    let double = XmppDouble::start().await;
    double.hold_new_connections();
    let tokens = Arc::new(FakeTokens::with_ttl(Duration::from_secs(3600)));
    let room = Arc::new(room_for(&double, tokens.clone(), Duration::from_secs(300)));

    let first_room = room.clone();
    let first = tokio::spawn(async move { first_room.join(&RoomIdentity::agent("first")).await });
    double.wait_for_connections(1).await;

    let second_room = room.clone();
    let second =
        tokio::spawn(async move { second_room.join(&RoomIdentity::agent("second")).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        double.connections().len(),
        1,
        "the already-joining guard must fire before a second real session is constructed"
    );

    double.release();
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(first.is_ok() ^ second.is_ok());
    let refusal = first.err().or_else(|| second.err()).unwrap().to_string();
    assert!(refusal.contains("already joined"), "{refusal}");
    assert_eq!(tokens.minted().len(), 1, "only one session was prepared");
    assert_eq!(muc_joins(&double), 1, "only one session joined the MUC");
    room.leave().await.unwrap();
}

/// Drain the MUC presence replay: the occupant already in the room, and ourselves.
async fn drain_replay(stream: &mut flux_channels::rooms::RoomStream) {
    for _ in 0..2 {
        match stream.recv().await {
            Some(RoomEvent::Joined { .. }) => {}
            other => panic!("expected the presence replay, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn jaas_session_survives_token_expiry() {
    let double = XmppDouble::start().await;
    let tokens = Arc::new(FakeTokens::with_ttl(Duration::from_secs(3)));
    let room = room_for(&double, tokens.clone(), Duration::from_secs(2));

    let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
    drain_replay(&mut stream).await;
    assert_eq!(tokens.minted().len(), 1, "one token for the initial join");

    // Crossing the expiry: the backend re-mints and re-joins on its own.
    double.wait_for_connections(2).await;
    let minted = tokens.minted();
    assert_eq!(minted.len(), 2, "the expiring token was re-minted");
    assert_ne!(minted[0], minted[1], "a re-mint is a *new* token");

    let dialled = double.connections();
    assert!(
        dialled[0].contains(&minted[0]),
        "the first connection carried the first token: {}",
        dialled[0]
    );
    assert!(
        dialled[1].contains(&minted[1]),
        "the re-join carried the fresh token, not the expiring one: {}",
        dialled[1]
    );

    // Still joined: a message said after the expiry reaches the room over the new session. The
    // replacement is joined a moment after its socket appears — and `say` refuses rather than
    // writing into the gap, which is the documented behaviour — so this retries briefly.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match room.say("still here").await {
            Ok(()) => break,
            Err(e) if tokio::time::Instant::now() >= deadline => {
                panic!("the room never became usable again after the refresh: {e}")
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    double.wait_for(|f| f.contains("still here")).await;

    // And the consumer never noticed. A transparent refresh emits neither `Ended` nor a second
    // `Joined` for occupants it already knows about.
    let quiet = tokio::time::timeout(Duration::from_millis(200), stream.recv()).await;
    assert!(
        quiet.is_err(),
        "the refresh leaked an event to the consumer: {quiet:?}"
    );

    room.leave().await.unwrap();
}

/// Poll until `f` holds, or panic. Used for the states that are settled by a background task.
async fn until(what: &str, f: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !f() {
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// How many MUC joins the room has been asked for — one per session that got as far as presence.
fn muc_joins(double: &XmppDouble) -> usize {
    double
        .sent()
        .iter()
        .filter(|f| {
            f.starts_with("<presence")
                && f.contains("http://jabber.org/protocol/muc")
                && !f.contains("type='unavailable'")
        })
        .count()
}

/// Join, tolerating the transient `<conflict/>` a not-yet-processed departure causes — but **never**
/// `already joined`, which is the stranded-session bug and can never resolve by waiting.
async fn join_eventually(room: &JaasRoom, what: &str) -> flux_channels::rooms::RoomStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match room.join(&RoomIdentity::agent("flux")).await {
            Ok(stream) => return stream,
            Err(e) => {
                let message = e.to_string();
                assert!(
                    !message.contains("already joined"),
                    "{what}: a session was stranded in the room — {message}"
                );
                assert!(tokio::time::Instant::now() < deadline, "{what}: {message}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

#[tokio::test]
async fn leaving_while_a_token_mint_is_in_flight_abandons_it() {
    let double = XmppDouble::start().await;
    let (tokens, gate, blocked) = FakeTokens::with_ttl(Duration::from_secs(3)).gated();
    let tokens = Arc::new(tokens);
    let room = room_for(&double, tokens.clone(), Duration::from_secs(2));

    let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
    drain_replay(&mut stream).await;

    // The refresh has fired and is inside the token service, which is not going to answer yet.
    until("the refresh to reach the token service", || {
        blocked.load(Ordering::SeqCst) > 0
    })
    .await;

    // `leave()` here is `RoomTurnDriver`'s ordinary shutdown path: it breaks on cancellation and
    // then leaves the room.
    room.leave().await.unwrap();
    gate.add_permits(10);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A mint we no longer need is abandoned rather than joined with.
    assert_eq!(
        tokens.minted().len(),
        1,
        "the abandoned mint must not have produced a session"
    );
    assert_eq!(double.connections().len(), 1, "nothing else was dialled");
    let _ = join_eventually(&room, "after leaving mid-mint").await;
    room.leave().await.unwrap();
}

#[tokio::test]
async fn leaving_while_a_replacement_join_is_in_flight_does_not_strand_it() {
    let double = XmppDouble::start().await;
    let tokens = Arc::new(FakeTokens::with_ttl(Duration::from_secs(3)));
    let room = room_for(&double, tokens.clone(), Duration::from_secs(2));

    let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
    drain_replay(&mut stream).await;

    // The window the review found: the token has been minted, the old session released, and the
    // replacement join is in flight — held here, mid-handshake, so `leave()` lands inside it.
    double.hold_new_connections();
    until("the replacement join to reach the server", || {
        double.connections().len() == 2
    })
    .await;

    room.leave().await.unwrap();
    double.release();

    // Wait for the replacement to actually finish joining before asking anything of the room.
    // Without this the test races the pump: it could re-join *before* the stranded session was
    // installed, and pass while the bug is present.
    until("the replacement join to complete", || {
        muc_joins(&double) == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    // The replacement completes into a room that no longer wants it. It must be left, not installed:
    // installed, it would sit in `inner` never left, and every later `join` would answer
    // "already joined" — the room permanently un-rejoinable.
    let mut rejoined = join_eventually(&room, "after leaving mid-join").await;
    drain_replay(&mut rejoined).await;

    // And the nick really is ours again rather than held by something nobody left: a full
    // leave/join round-trip only completes if no stranded session is still occupying it.
    room.leave().await.unwrap();
    let _ = join_eventually(&room, "after a clean leave").await;
    room.leave().await.unwrap();
}

#[tokio::test]
async fn the_jaas_token_rides_the_websocket_url_and_sasl_is_anonymous() {
    let double = XmppDouble::start().await;
    let tokens = Arc::new(FakeTokens::with_ttl(Duration::from_secs(3600)));
    let room = room_for(&double, tokens.clone(), Duration::from_secs(300));

    let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
    drain_replay(&mut stream).await;

    let dialled = double.connections();
    let jwt = &tokens.minted()[0];
    assert!(
        dialled[0].contains(&format!("token={jwt}")),
        "the token rides the URL: {}",
        dialled[0]
    );
    assert!(
        dialled[0].contains(&format!("room={ROOM}")),
        "the room rides the URL in the case the token claims it: {}",
        dialled[0]
    );
    assert!(
        dialled[0].contains(TENANT),
        "the tenant comes out of the token's `sub` claim: {}",
        dialled[0]
    );

    // JaaS offers **only** `ANONYMOUS`, with or without the token in the URL, and refuses `PLAIN`
    // with the JWT as the password (`<invalid-mechanism/>`). Authorization happens at focus and via
    // the URL-borne token, never in SASL — asserted so this is not "fixed" the wrong way later.
    let auth = double.wait_for(|f| f.starts_with("<auth")).await;
    assert!(auth.contains("mechanism='ANONYMOUS'"), "{auth}");
    assert!(!auth.contains("PLAIN"), "{auth}");
    assert!(
        !auth.contains(jwt.as_str()),
        "the token must not be smuggled into the SASL payload"
    );

    room.leave().await.unwrap();
}

#[tokio::test]
async fn the_muc_jid_comes_from_the_conference_response_not_the_token() {
    let double = XmppDouble::start().await;
    let tokens = Arc::new(FakeTokens::with_ttl(Duration::from_secs(3600)));
    let room = room_for(&double, tokens.clone(), Duration::from_secs(300));

    // Before joining, the room can only answer with what was configured.
    assert_eq!(room.id().as_str(), ROOM);

    let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
    drain_replay(&mut stream).await;

    // The JWT keeps the original case; the conference-request response lowercases it. The room
    // answers with the server's spelling, which is the one a stanza has to be addressed to.
    let token = GuestToken::parse(tokens.minted()[0].clone()).unwrap();
    assert_eq!(token.room_claim(), Some(ROOM));
    assert_eq!(token.tenant(), TENANT);
    assert_eq!(room.id().as_str(), ROOM_JID_SERVER);
    assert_ne!(room.id().as_str(), ROOM);

    room.say("hello room").await.unwrap();
    let said = double.wait_for(|f| f.contains("hello room")).await;
    assert!(
        said.contains(&format!("to='{ROOM_JID_SERVER}'")),
        "groupchat is addressed to the server's spelling: {said}"
    );

    room.leave().await.unwrap();
}

#[tokio::test]
async fn the_guest_token_never_appears_in_a_debug_rendering_or_a_connect_error() {
    let jwt = guest_jwt(ROOM, Duration::from_secs(3600));
    let token = GuestToken::parse(jwt.clone()).unwrap();
    let rendered = format!("{token:?}");
    assert!(!rendered.contains(&jwt), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");

    // A guest token rides the *query string* of the endpoint, so any error that names the endpoint
    // would otherwise publish the token to a log. Nothing listens on port 1.
    let tokens = Arc::new(FakeTokens::with_ttl(Duration::from_secs(3600)));
    let room = JaasRoom::new(
        JaasConfig::new(ROOM)
            .signalling("ws://127.0.0.1:1")
            .allow_private_net(true),
        tokens.clone(),
    );
    // `RoomStream` is not `Debug` (it is a receiver), so the error is unwrapped by hand.
    let message = match room.join(&RoomIdentity::agent("flux")).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("nothing is listening on port 1"),
    };
    let minted = &tokens.minted()[0];
    assert!(!message.contains(minted.as_str()), "{message}");
    assert!(
        message.contains("127.0.0.1:1"),
        "the endpoint is still named, minus its query: {message}"
    );
}

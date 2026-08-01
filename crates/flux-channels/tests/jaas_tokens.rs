//! `BraveTalkTokens` — the vendor HTTP half of D-206 — against an **in-process token service**.
//!
//! No test here reaches `talk.brave.com` or `8x8.vc`. The double bound on loopback answers the three
//! requests the 2026-07-30 spike observed and is strict about what makes them a handshake: the `PUT`
//! refuses without both the CSRF header and the cookie the `OPTIONS` handed out, and the
//! conference-request refuses without the exact JWT as a Bearer. A backend that skipped either would
//! pass a permissive double and fail against the vendor.
//!
//! The last test drives the *whole* of D-206 — HTTP token acquisition, focus allocation, and D-205's
//! MUC join — with the vendor faked at both seams and nothing on the network.

mod support;

use std::sync::Arc;
use std::time::Duration;

use flux_channels::build_channels;
use flux_channels::rooms::{
    BraveTalkTokens, GuestToken, JaasConfig, JaasRoom, JaasTokens, Room, RoomEvent, RoomIdentity,
};
use flux_lang::program::ChannelDecl;

use support::jaas_double::{JaasDouble, CSRF_COOKIE, CSRF_TOKEN, TENANT};
use support::xmpp_double::{XmppDouble, ROOM_JID_SERVER};

/// The room as a human spells it. The conference-request response lowercases it; the JWT does not.
const ROOM: &str = "StandUp";

/// A token source pointed at `double`, with the scoped grant loopback needs.
fn tokens_for(double: &JaasDouble) -> BraveTalkTokens {
    BraveTalkTokens::new()
        .token_service(double.base())
        .conference_service(double.base())
        // The double is on loopback, which the egress guard blocks by default. A scoped grant is the
        // supported way to reach a private address — not a second, hand-rolled guard.
        .allow_private_net(true)
}

#[tokio::test]
async fn the_guest_token_handshake_sends_the_csrf_header_and_the_cookie_jar() {
    let double = JaasDouble::start(Duration::from_secs(3600)).await;
    let token = tokens_for(&double).guest_token(ROOM).await.unwrap();

    // The token the vendor minted, read back through the claims flux acts on.
    assert_eq!(token.jwt(), double.minted()[0]);
    assert_eq!(token.tenant(), TENANT);
    assert_eq!(token.room_claim(), Some(ROOM), "the JWT keeps the case");

    // The wire, not just the outcome: a preflight, then a PUT carrying what the preflight issued.
    let preflight = double.seen_method("OPTIONS");
    assert_eq!(preflight.len(), 1, "{preflight:?}");
    assert_eq!(preflight[0].path, format!("/api/v1/rooms/{ROOM}"));

    let put = double.seen_method("PUT");
    assert_eq!(put.len(), 1, "{put:?}");
    assert_eq!(put[0].path, format!("/api/v1/rooms/{ROOM}"));
    assert_eq!(put[0].csrf.as_deref(), Some(CSRF_TOKEN));
    assert!(
        put[0]
            .cookie
            .as_deref()
            .is_some_and(|c| c.contains(CSRF_COOKIE)),
        "the cookie the preflight set rides the PUT: {:?}",
        put[0].cookie
    );

    // `PUT` joins an existing room; `POST` creates one and needs a subscriber cookie we do not have.
    assert!(
        double.seen_method("POST").is_empty(),
        "minting must never create a room"
    );
}

#[tokio::test]
async fn focus_allocation_sends_the_jwt_as_bearer_and_answers_with_the_lowercased_muc_jid() {
    let double = JaasDouble::start(Duration::from_secs(3600)).await;
    let tokens = tokens_for(&double);
    let token = tokens.guest_token(ROOM).await.unwrap();
    let conference = tokens.conference(ROOM, &token).await.unwrap();

    assert_eq!(conference.room_jid, ROOM_JID_SERVER);
    assert_ne!(
        conference.room_jid, ROOM,
        "the response lowercases the room; the JWT does not"
    );
    assert_eq!(conference.focus_jid.as_deref(), Some("focus@auth.8x8.vc"));

    let post = double.seen_method("POST");
    assert_eq!(post.len(), 1, "{post:?}");
    assert_eq!(post[0].path, format!("/{TENANT}/conference-request/v1"));
    assert_eq!(post[0].query, format!("room={ROOM}"));
    assert_eq!(
        post[0].authorization.as_deref(),
        Some(format!("Bearer {}", token.jwt()).as_str())
    );
}

#[tokio::test]
async fn focus_allocation_waits_while_the_conference_is_not_ready() {
    // Two `ready: false` answers, then the real one.
    let double = JaasDouble::start_with(Duration::from_secs(3600), 2).await;
    let tokens = tokens_for(&double);
    let token = tokens.guest_token(ROOM).await.unwrap();

    let conference = tokens.conference(ROOM, &token).await.unwrap();
    assert_eq!(conference.room_jid, ROOM_JID_SERVER);
    assert_eq!(
        double.seen_method("POST").len(),
        3,
        "a not-ready conference is retried, not failed"
    );
}

#[tokio::test]
async fn the_vendor_endpoints_go_through_the_egress_guard() {
    let double = JaasDouble::start(Duration::from_secs(3600)).await;
    // Same loopback double, but without the scoped grant: the guard must refuse it.
    let ungranted = BraveTalkTokens::new()
        .token_service(double.base())
        .conference_service(double.base());

    let refused = match ungranted.guest_token(ROOM).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("the egress guard admitted a loopback endpoint without a grant"),
    };
    assert!(
        refused.contains("private/loopback") || refused.contains("refusing"),
        "the refusal comes from the one egress guard: {refused}"
    );
    assert!(
        double.seen().is_empty(),
        "nothing was dialled: {:?}",
        double.seen()
    );
}

#[tokio::test]
async fn a_failed_handshake_never_quotes_a_token_or_a_csrf_value() {
    let double = JaasDouble::start(Duration::from_secs(3600)).await;
    let tokens = tokens_for(&double);
    let token = tokens.guest_token(ROOM).await.unwrap();

    // A token the double never minted: the conference-request refuses it.
    let forged = GuestToken::parse(format!("{}x", token.jwt())).unwrap();
    let message = match tokens.conference(ROOM, &forged).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("the double accepted a token it never minted"),
    };
    assert!(!message.contains(forged.jwt()), "{message}");
    assert!(!message.contains(token.jwt()), "{message}");
    assert!(!message.contains(CSRF_TOKEN), "{message}");
    assert!(
        message.contains("401") || message.to_lowercase().contains("unauthorized"),
        "the status still says what happened: {message}"
    );
}

#[test]
fn the_jaas_backend_is_declarable_and_needs_no_credential_field() {
    // The whole declaration a Brave Talk room takes: which room, and what to call ourselves. The
    // guest path is unauthenticated — there is no field here that accepts a JWT, an API key or a
    // private key, which is the strongest form of "never a literal in a `.flux` file".
    let built = build_channels(&[ChannelDecl {
        name: "standup".into(),
        kind: "room".into(),
        settings: serde_json::json!({
            "backend": "jaas",
            "room": ROOM,
            "nick": "flux",
        }),
    }])
    .expect("a `jaas` room builds through build_channels");
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].name(), "standup");

    // The operator can move both vendor endpoints — an own JaaS tenant fronting its own minter.
    build_channels(&[ChannelDecl {
        name: "standup".into(),
        kind: "room".into(),
        settings: serde_json::json!({
            "backend": "jaas",
            "room": ROOM,
            "token_service": "https://talk.example.com",
            "conference_service": "https://jaas.example.com",
            "url": "wss://jaas.example.com",
        }),
    }])
    .expect("the vendor endpoints are operator configuration");
}

#[tokio::test]
async fn a_jaas_room_joins_over_the_real_handshake_with_the_vendor_faked_at_both_seams() {
    let vendor = JaasDouble::start(Duration::from_secs(3600)).await;
    let signalling = XmppDouble::start().await;

    let room = JaasRoom::new(
        JaasConfig::new(ROOM)
            .signalling(signalling.ws_base())
            .allow_private_net(true),
        Arc::new(tokens_for(&vendor)),
    );

    let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
    for _ in 0..2 {
        match stream.recv().await {
            Some(RoomEvent::Joined { .. }) => {}
            other => panic!("expected the presence replay, got {other:?}"),
        }
    }

    // The MUC JID came from the conference-request response, not from the JWT's `room` claim.
    assert_eq!(room.id().as_str(), ROOM_JID_SERVER);

    // And the token minted over HTTP is the one that rode the WebSocket URL.
    let jwt = &vendor.minted()[0];
    assert!(
        signalling.connections()[0].contains(jwt.as_str()),
        "{}",
        signalling.connections()[0]
    );

    room.say("hello from the whole handshake").await.unwrap();
    signalling
        .wait_for(|f| f.contains("hello from the whole handshake"))
        .await;
    room.leave().await.unwrap();
}

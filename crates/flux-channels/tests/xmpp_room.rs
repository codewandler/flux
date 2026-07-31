//! `XmppMucRoom` against an **in-process XMPP double** (D-205).
//!
//! Every test here runs against a WebSocket server bound on loopback inside this process: no browser,
//! no vendor SDK, no network. That is the design's invariant 6 ("text needs no browser") asserted
//! rather than asserted-about.
//!
//! Three of these tests exist because the 2026-07-30 spike paid for them in wall-clock time, and the
//! design's Feasibility section records why:
//!
//! - **every stanza must be `jabber:client`-qualified** — prosody answers an unqualified one with
//!   `<unsupported-stanza-type/>` and closes the stream;
//! - **the keepalive must be an XMPP ping IQ** — a whitespace frame is illegal on the WebSocket
//!   binding and is closed with `1007 Invalid payload start character`;
//! - **the room JID's case comes from the server** — JaaS lowercases the room in the MUC JID while the
//!   JWT keeps the original case, so a locally-rebuilt address is wrong in a way that is hard to see.

mod support;

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use flux_channels::rooms::{
    MessageScope, OccupantId, Room, RoomEvent, RoomIdentity, RoomTurnDriver, XmppConfig,
    XmppMucRoom,
};
use flux_flow::voice::{Speaker, VoiceReply, VoiceTurnHandler};

use support::xmpp_double::{XmppDouble, ROOM_JID_SERVER};

/// A config pointed at `double`, joining the mixed-case room the double lowercases.
fn config_for(double: &XmppDouble) -> XmppConfig {
    XmppConfig::new(double.url(), support::xmpp_double::ROOM_JID_CONFIGURED)
        // The double is on loopback, which the egress guard blocks by default. A scoped grant is the
        // supported way to reach a private address — not a second, hand-rolled guard.
        .allow_private_net(true)
        // The XMPP domain is not derivable from a MUC JID (`conference.example.org` is a component of
        // `example.org`), so it is configured.
        .domain("example.org")
}

#[tokio::test]
async fn xmpp_room_joins_and_exchanges_text() {
    let double = XmppDouble::start().await;
    let room = XmppMucRoom::new(config_for(&double));

    let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();

    // The MUC presence replay arrives as `Joined`, ours among it — a consumer needs no priming call.
    let mut joined = Vec::new();
    for _ in 0..2 {
        match stream.recv().await {
            Some(RoomEvent::Joined { occupant }) => joined.push(occupant),
            other => panic!("expected the presence replay, got {other:?}"),
        }
    }
    assert!(
        joined.iter().any(|o| o.nick == "timo" && !o.is_self),
        "the occupant already in the room is tracked from presence: {joined:?}"
    );

    let occupants = room.occupants().await.unwrap();
    let me = occupants
        .iter()
        .find(|o| o.is_self)
        .expect("occupants contains ourselves");
    assert_eq!(me.nick, "flux");
    assert_eq!(
        me.id,
        OccupantId::new(format!("{ROOM_JID_SERVER}/flux")),
        "our occupant id is the one the server assigned"
    );

    // `say` emits a groupchat stanza.
    room.say("morning all").await.unwrap();
    let stanza = double
        .wait_for(|f| f.contains("type='groupchat'") || f.contains("type=\"groupchat\""))
        .await;
    assert!(
        stanza.contains("<body>morning all</body>"),
        "the groupchat stanza carries the text: {stanza}"
    );
    assert!(
        stanza.contains(ROOM_JID_SERVER),
        "addressed to the room the server named: {stanza}"
    );

    // An inbound groupchat stanza surfaces as an attributed message.
    double
        .push(format!(
            "<message xmlns='jabber:client' type='groupchat' from='{ROOM_JID_SERVER}/timo'>\
             <body>morning</body></message>"
        ))
        .await;
    assert_eq!(
        stream.recv().await,
        Some(RoomEvent::Message {
            from: OccupantId::new(format!("{ROOM_JID_SERVER}/timo")),
            text: "morning".into(),
            scope: MessageScope::Groupchat,
        })
    );

    // A private message keeps its scope, so a reply can go back the way it came.
    double
        .push(format!(
            "<message xmlns='jabber:client' type='chat' from='{ROOM_JID_SERVER}/timo'>\
             <body>just us</body></message>"
        ))
        .await;
    assert_eq!(
        stream.recv().await,
        Some(RoomEvent::Message {
            from: OccupantId::new(format!("{ROOM_JID_SERVER}/timo")),
            text: "just us".into(),
            scope: MessageScope::Private,
        })
    );
    room.whisper(&OccupantId::new(format!("{ROOM_JID_SERVER}/timo")), "hi")
        .await
        .unwrap();
    let private = double
        .wait_for(|f| f.contains("type='chat'") && f.contains("<body>hi</body>"))
        .await;
    assert!(
        private.contains(&format!("to='{ROOM_JID_SERVER}/timo'")),
        "a whisper is addressed to the occupant, not the room: {private}"
    );

    // Leaving sends unavailable presence and ends the consumer's stream.
    room.leave().await.unwrap();
    double
        .wait_for(|f| f.starts_with("<presence") && f.contains("type='unavailable'"))
        .await;
    assert_eq!(
        stream.recv().await,
        None,
        "a left room delivers nothing more"
    );
}

#[tokio::test]
async fn every_stanza_the_xmpp_backend_emits_is_jabber_client_qualified() {
    // The spike's first trap: prosody answers an unqualified stanza with `<unsupported-stanza-type/>`
    // and kills the stream. Assert it on the wire — a helper that *usually* adds the namespace is
    // exactly the shape of bug this test exists to catch.
    let double = XmppDouble::start().await;
    let room = XmppMucRoom::new(config_for(&double));
    let _stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();

    room.say("hello").await.unwrap();
    room.whisper(&OccupantId::new(format!("{ROOM_JID_SERVER}/timo")), "psst")
        .await
        .unwrap();
    room.leave().await.unwrap();
    // A frame is recorded when the *server* reads it, so wait for the last one before collecting.
    double
        .wait_for(|f| f.starts_with("<presence") && f.contains("type='unavailable'"))
        .await;

    let frames = double.sent();
    let stanzas: Vec<&String> = frames
        .iter()
        .filter(|f| f.starts_with("<message") || f.starts_with("<presence") || f.starts_with("<iq"))
        .collect();
    assert!(
        stanzas.len() >= 4,
        "bind IQ, join presence, message, whisper, unavailable presence: {frames:?}"
    );
    for stanza in stanzas {
        assert!(
            stanza.contains("xmlns='jabber:client'"),
            "unqualified stanza would be refused by prosody: {stanza}"
        );
    }
}

#[tokio::test]
async fn the_xmpp_keepalive_is_a_ping_iq_and_never_whitespace() {
    // The spike's second trap: a `" "` keepalive frame is closed by the server with
    // `1007 Invalid payload start character`.
    let double = XmppDouble::start().await;
    let room = XmppMucRoom::new(config_for(&double).keepalive(Duration::from_millis(20)));
    let _stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();

    let ping = double.wait_for(|f| f.contains("urn:xmpp:ping")).await;
    assert!(
        ping.starts_with("<iq") && ping.contains("xmlns='jabber:client'"),
        "the keepalive is a qualified ping IQ: {ping}"
    );
    room.leave().await.unwrap();

    for frame in double.sent() {
        assert!(
            !frame.trim().is_empty(),
            "a whitespace-only frame closes the stream with 1007; frames were {:?}",
            double.sent()
        );
    }
}

#[tokio::test]
async fn the_room_jid_case_comes_from_the_server() {
    // JaaS lowercases the room in the MUC JID while the token's `room` claim keeps the original case.
    // A locally-rebuilt address is subtly wrong, so the server's spelling has to win.
    let double = XmppDouble::start().await;
    let room = XmppMucRoom::new(config_for(&double));
    assert_eq!(
        room.id().as_str(),
        support::xmpp_double::ROOM_JID_CONFIGURED,
        "before joining, all we have is what was configured"
    );

    let _stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();

    assert_eq!(
        room.id().as_str(),
        ROOM_JID_SERVER,
        "after joining, the server's spelling wins"
    );
    assert!(
        room.occupants()
            .await
            .unwrap()
            .iter()
            .all(|o| o.id.as_str().starts_with(ROOM_JID_SERVER)),
        "occupant ids are the server's, not rebuilt from the configured JID"
    );
}

#[tokio::test]
async fn exactly_one_occupant_is_self_and_the_agent_never_answers_its_own_echo() {
    // `Occupant::new` defaults `is_self` to false, so a backend that forgets to mark itself compiles
    // fine and then answers its own echoed groupchat message forever — an unbounded loop that costs
    // real provider money. Pin both halves: the backend marks exactly one occupant, and the driver
    // uses that mark to drop our own echo.
    let double = XmppDouble::start().await;
    let room = Arc::new(XmppMucRoom::new(config_for(&double)));

    {
        let _stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
        let occupants = room.occupants().await.unwrap();
        assert_eq!(
            occupants.iter().filter(|o| o.is_self).count(),
            1,
            "exactly one occupant is us: {occupants:?}"
        );
        room.leave().await.unwrap();
    }

    /// Answers everything, and counts how often it was asked.
    #[derive(Default)]
    struct Counter(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl VoiceTurnHandler for Counter {
        async fn turn(&self, _speaker: &Speaker, _text: &str) -> VoiceReply {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            VoiceReply::Continue("echo".into())
        }
    }

    let double = XmppDouble::start().await;
    let room = Arc::new(XmppMucRoom::new(config_for(&double)));
    let handler = Arc::new(Counter::default());
    let cancel = CancellationToken::new();

    let driver_room = room.clone();
    let driver_handler = handler.clone();
    let driver_cancel = cancel.clone();
    let driver = tokio::spawn(async move {
        RoomTurnDriver::new(driver_room, RoomIdentity::agent("flux"))
            .run(driver_handler.as_ref(), &driver_cancel)
            .await
    });

    // A MUC reflects our own groupchat message back at us. Push one that is *from us*.
    double.wait_for(|f| f.starts_with("<presence")).await;
    double
        .push(format!(
            "<message xmlns='jabber:client' type='groupchat' from='{ROOM_JID_SERVER}/flux'>\
             <body>echo</body></message>"
        ))
        .await;
    // …and one that is from somebody else, so the test can tell "suppressed" from "not delivered yet".
    double
        .push(format!(
            "<message xmlns='jabber:client' type='groupchat' from='{ROOM_JID_SERVER}/timo'>\
             <body>morning</body></message>"
        ))
        .await;

    double
        .wait_for(|f| f.contains("<body>echo</body>") && f.contains("type='groupchat'"))
        .await;
    cancel.cancel();
    driver.await.unwrap().unwrap();

    assert_eq!(
        handler.0.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "our own echoed message is not a user turn; only timo's is"
    );
}

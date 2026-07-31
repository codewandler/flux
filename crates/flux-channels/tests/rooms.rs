//! D-204 — the `Room` port: attributed inbound text, the `room` channel kind, and the safety
//! envelope a room-sourced turn keeps.
//!
//! A room is **untrusted multi-party input**: any occupant can type, and anyone holding the link can
//! put a client in the room. So these tests pin two things — that every inbound event names *who*
//! spoke (the precondition for D-207's address rule), and that a room-sourced turn buys no authority
//! it would not have from a CLI prompt.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_app::{App, JourneyRun};
use flux_channels::rooms::{
    MessageScope, MockRoom, Occupant, OccupantKind, RoomEvent, RoomIdentity, RoomTurnDriver,
};
use flux_channels::{build_channels, AppDeliverer, Channel, Deliverer, RoomChannel};
use flux_flow::voice::{Speaker, VoiceReply, VoiceTurnHandler};
use flux_lang::program::{ChannelDecl, Module, Program};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// A [`VoiceTurnHandler`] that records who spoke each turn — the room analogue of the `Deliverer`
/// recording double the other channel tests use. Says nothing back, so the room stays quiet.
#[derive(Default)]
struct TurnLog {
    turns: Mutex<Vec<(String, Option<String>, String)>>, // (speaker id, display name, text)
}

#[async_trait]
impl VoiceTurnHandler for TurnLog {
    async fn turn(&self, speaker: &Speaker, user_text: &str) -> VoiceReply {
        self.turns.lock().unwrap().push((
            speaker.id().to_string(),
            speaker.display_name().map(str::to_string),
            user_text.to_string(),
        ));
        VoiceReply::Continue(String::new())
    }
}

fn occupants() -> (Occupant, Occupant) {
    (
        Occupant::new("standup@rooms.example/timo", "timo", OccupantKind::Human),
        Occupant::new("standup@rooms.example/ada", "ada", OccupantKind::Human),
    )
}

fn said(from: &Occupant, text: &str) -> RoomEvent {
    RoomEvent::Message {
        from: from.id.clone(),
        text: text.to_string(),
        scope: MessageScope::Groupchat,
    }
}

#[tokio::test]
async fn room_message_carries_speaker() {
    // Two occupants, one message each. The 1:1 turn seam had no speaker at all; a room must produce
    // one turn per message, each attributed to the occupant who actually spoke.
    let (timo, ada) = occupants();
    let room = Arc::new(
        MockRoom::new("standup@rooms.example")
            .with_occupant(timo.clone())
            .with_occupant(ada.clone())
            .script(vec![
                said(&timo, "who is on call tonight?"),
                said(&ada, "i am, until midnight"),
                RoomEvent::Ended,
            ]),
    );

    let handler = TurnLog::default();
    let driver = RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"));
    driver
        .run(&handler, &CancellationToken::new())
        .await
        .expect("the room session runs to Ended");

    let turns = handler.turns.lock().unwrap().clone();
    assert_eq!(turns.len(), 2, "two messages produce two turns: {turns:?}");
    assert_ne!(
        turns[0].0, turns[1].0,
        "the two turns carry distinct speakers: {turns:?}"
    );
    assert_eq!(turns[0].0, timo.id.as_str());
    assert_eq!(turns[1].0, ada.id.as_str());
    // The nick from the occupant list rides along, so an answer can name the human.
    assert_eq!(turns[0].1.as_deref(), Some("timo"));
    assert_eq!(turns[1].1.as_deref(), Some("ada"));
    assert_eq!(turns[0].2, "who is on call tonight?");
    assert_eq!(turns[1].2, "i am, until midnight");
}

#[tokio::test]
async fn every_inbound_room_event_names_an_occupant() {
    // The port's whole point: attribution is not optional. Only `Ended` — the room lifecycle
    // terminator, which nobody speaks — has no occupant.
    let (timo, _) = occupants();
    for event in [
        RoomEvent::Joined {
            occupant: timo.clone(),
        },
        RoomEvent::Left {
            occupant: timo.id.clone(),
        },
        said(&timo, "hello"),
        RoomEvent::Message {
            from: timo.id.clone(),
            text: "just between us".into(),
            scope: MessageScope::Private,
        },
    ] {
        assert_eq!(
            event.occupant().map(|id| id.as_str()),
            Some(timo.id.as_str()),
            "{event:?} must name its occupant"
        );
    }
    assert!(RoomEvent::Ended.occupant().is_none());
}

#[tokio::test]
async fn room_turn_reply_goes_back_into_the_room_and_leaves_on_completion() {
    // A `Complete` reply ends the session the way the voice driver does: speak the final line, then
    // leave. `Continue` keeps the agent in the room.
    struct Replier;
    #[async_trait]
    impl VoiceTurnHandler for Replier {
        async fn turn(&self, speaker: &Speaker, _text: &str) -> VoiceReply {
            VoiceReply::Complete(format!("bye {}", speaker.label()))
        }
    }

    let (timo, _) = occupants();
    let room = Arc::new(
        MockRoom::new("standup@rooms.example")
            .with_occupant(timo.clone())
            .script(vec![said(&timo, "we're done")]),
    );
    RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"))
        .run(&Replier, &CancellationToken::new())
        .await
        .expect("the room session completes");

    assert_eq!(room.said(), vec!["bye timo".to_string()]);
    assert!(room.has_left(), "a completed session leaves the room");
}

#[tokio::test]
async fn the_agent_never_answers_its_own_room_message() {
    // A groupchat message is echoed back to its sender by every MUC. Without self-suppression the
    // agent answers itself forever.
    let me = MockRoom::self_occupant("standup@rooms.example", "flux");
    let room = Arc::new(
        MockRoom::new("standup@rooms.example")
            .script(vec![said(&me, "anything else?"), RoomEvent::Ended]),
    );

    let handler = TurnLog::default();
    RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"))
        .run(&handler, &CancellationToken::new())
        .await
        .expect("the room session runs to Ended");
    assert!(
        handler.turns.lock().unwrap().is_empty(),
        "our own echo is not a user turn"
    );
}

// --- the `room` channel kind ---------------------------------------------------------------------

fn decl(settings: Value) -> ChannelDecl {
    ChannelDecl {
        name: "standup".into(),
        kind: "room".into(),
        settings,
    }
}

#[test]
fn room_channel_builds_from_a_decl_and_rejects_an_unknown_backend() {
    let built = build_channels(&[decl(json!({
        "backend": "mock",
        "room": "standup@rooms.example",
        "nick": "flux",
        "address_rule": "mention",
    }))])
    .expect("a `room` channel builds through build_channels");
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].name(), "standup");

    // An unrecognized backend is a load error, exactly like an unrecognized channel `kind`.
    let err = build_error(decl(json!({
        "backend": "telepathy",
        "room": "standup@rooms.example",
    })));
    assert!(err.contains("telepathy"), "names the backend: {err}");
    assert!(err.contains("standup"), "names the channel: {err}");

    // `backend` and `room` are both required — a half-declared room fails at load, not at join.
    let err = build_error(decl(json!({ "backend": "mock" })));
    assert!(err.contains("standup"), "names the channel: {err}");
    assert!(err.contains("room"), "names the missing field: {err}");
}

/// `build_channels`' error for one declaration. `Vec<Box<dyn Channel>>` is not `Debug`, so the `Ok`
/// side cannot go through `expect_err`.
fn build_error(decl: ChannelDecl) -> String {
    match build_channels(&[decl]) {
        Ok(_) => panic!("expected a load error"),
        Err(e) => e.to_string(),
    }
}

/// Wraps the real `AppDeliverer` and records each delivery's outcome, so a test can see the error a
/// denied op produced without that error tearing the channel down.
struct Tee {
    inner: AppDeliverer,
    outcomes: Mutex<Vec<Result<Vec<JourneyRun>, String>>>,
}

#[async_trait]
impl Deliverer for Tee {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        let out = self.inner.deliver(label, payload).await;
        self.outcomes.lock().unwrap().push(match &out {
            Ok(runs) => Ok(runs.clone()),
            Err(e) => Err(e.to_string()),
        });
        out
    }
}

fn program(src: &str) -> Program {
    match Module::parse_str(src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    }
}

/// Deliver one room message through a `RoomChannel` into a real `App` at the given approval posture,
/// and return what the delivery produced.
async fn room_message_into_app(auto_approve: bool) -> Vec<Result<Vec<JourneyRun>, String>> {
    let src = r#"channel standup
  kind "room"
  backend "mock"
  room "standup@rooms.example"

trigger t
  on "standup"
  run clock

journey clock
  flow
    return now()
"#;
    let (timo, _) = occupants();
    let room = Arc::new(
        MockRoom::new("standup@rooms.example")
            .with_occupant(timo.clone())
            .script(vec![said(&timo, "what time is it?"), RoomEvent::Ended]),
    );
    let program = program(src);
    let decl = program.channels[0].clone();
    let app = Arc::new(App::with_options(program, None, "mock", auto_approve));
    let tee = Arc::new(Tee {
        inner: AppDeliverer::new(app),
        outcomes: Mutex::new(Vec::new()),
    });

    let channel = RoomChannel::with_room(&decl, room).expect("a room channel over the mock room");
    let d: Arc<dyn Deliverer> = tee.clone();
    channel
        .start(d, CancellationToken::new())
        .await
        .expect("a denied op is not a fatal channel error");

    let outcomes = tee.outcomes.lock().unwrap().clone();
    outcomes
}

#[tokio::test]
async fn a_room_sourced_turn_dispatches_through_the_executor_and_approver() {
    // D-213 / meeting-rooms invariant 1: joining a room grants NO authority. The op the journey calls
    // needs approval, so with no approver consent it is denied — the same envelope a CLI turn meets.
    let denied = room_message_into_app(false).await;
    assert_eq!(denied.len(), 1, "one message, one delivery: {denied:?}");
    let err = denied[0]
        .as_ref()
        .expect_err("an unapproved op must be denied for a room-sourced turn");
    assert!(err.contains("now"), "the denial names the op: {err}");

    // And the approver is genuinely the thing in the path: consent, and the very same room message
    // runs the very same op.
    let approved = room_message_into_app(true).await;
    let runs = approved[0]
        .as_ref()
        .expect("the same room message succeeds once approved");
    assert!(
        runs.iter()
            .any(|r| r.journey == "clock" && !r.result.is_empty()),
        "the room-sourced turn ran the journey: {runs:?}"
    );
}

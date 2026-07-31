//! [`RoomTurnDriver`] — a [`Room`](super::Room) driving the L3 turn seam, one turn per attributed
//! message.
//!
//! This is the room analogue of `flux_flow::voice::VoiceSessionDriver::run_flow_turns`: the transport
//! owns presence and text, the handler owns the logic, and the driver is the small piece that maps
//! one to the other. What it adds over the voice driver is the thing a room has and a phone line does
//! not — a **speaker** on every turn.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use flux_core::Result;
use flux_flow::voice::{Speaker, VoiceReply, VoiceTurnHandler};

use super::{MessageScope, Occupant, OccupantId, Room, RoomEvent, RoomIdentity};

/// Joins a room and turns each inbound message into one handler turn, attributed to the occupant who
/// said it.
///
/// The driver does **not** decide whether the agent should answer — that is D-207's address rule, and
/// putting a half-rule here would be worse than none. It does suppress our *own* echoed messages: a
/// MUC reflects every groupchat message back to its sender, so without that a handler answers itself
/// forever. That is loop prevention, not addressing.
pub struct RoomTurnDriver {
    room: Arc<dyn Room>,
    identity: RoomIdentity,
}

impl RoomTurnDriver {
    /// Drive `room`, joining as `identity`.
    pub fn new(room: Arc<dyn Room>, identity: RoomIdentity) -> Self {
        Self { room, identity }
    }

    /// Join, then run until the room ends, the transport dies, `cancel` fires, or the handler
    /// returns [`VoiceReply::Complete`] — in which case the final line is said and the room is left,
    /// mirroring the voice driver's hangup.
    ///
    /// A handler reply is said back with the same scope it answers: a whisper is answered privately,
    /// public text publicly. An empty reply says nothing at all — that is how a handler stays silent
    /// without the driver posting blank lines into a room full of people.
    pub async fn run(
        &self,
        handler: &dyn VoiceTurnHandler,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let mut stream = self.room.join(&self.identity).await?;
        // Nick lookup for the turn's `Speaker`, and the set of ids that are *us*.
        let mut known: HashMap<OccupantId, Occupant> = HashMap::new();

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => break,
                event = stream.recv() => match event {
                    Some(event) => event,
                    // The transport is gone; leaving is still the right thing to attempt.
                    None => break,
                },
            };

            match event {
                RoomEvent::Joined { occupant } => {
                    known.insert(occupant.id.clone(), occupant);
                }
                RoomEvent::Left { occupant } => {
                    known.remove(&occupant);
                }
                RoomEvent::Message { from, text, scope } => {
                    // Our own echo is not a user turn.
                    if known.get(&from).is_some_and(|o| o.is_self) {
                        continue;
                    }
                    let reply = handler
                        .turn(&speaker_for(&from, known.get(&from)), &text)
                        .await;
                    let (line, complete) = match reply {
                        VoiceReply::Continue(line) => (line, false),
                        VoiceReply::Complete(line) => (line, true),
                    };
                    if !line.trim().is_empty() {
                        match scope {
                            MessageScope::Groupchat => self.room.say(&line).await?,
                            MessageScope::Private => self.room.whisper(&from, &line).await?,
                        }
                    }
                    if complete {
                        break;
                    }
                }
                RoomEvent::Ended => break,
                // No wildcard arm on purpose: `RoomEvent` is `#[non_exhaustive]` for *downstream*
                // crates, but in here an added variant (the media ones, D-208…D-211) must fail to
                // compile and force a decision rather than being silently dropped on the floor.
            }
        }

        self.room.leave().await
    }
}

/// The turn's speaker: the occupant id always, plus the nick when presence has told us one. An id we
/// have never seen a `Joined` for still produces a speaker — an unattributed turn is not an option.
fn speaker_for(id: &OccupantId, occupant: Option<&Occupant>) -> Speaker {
    let speaker = Speaker::new(id.as_str());
    match occupant {
        Some(o) if !o.nick.is_empty() => speaker.with_display_name(&o.nick),
        _ => speaker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::{MockRoom, OccupantKind};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Answers every turn with a fixed line.
    struct Parrot(&'static str);

    #[async_trait]
    impl VoiceTurnHandler for Parrot {
        async fn turn(&self, _speaker: &Speaker, _text: &str) -> VoiceReply {
            VoiceReply::Continue(self.0.to_string())
        }
    }

    #[tokio::test]
    async fn an_unknown_speaker_still_produces_an_attributed_turn() {
        // Presence can lag a message (or we joined mid-sentence). The id is what matters; the nick is
        // a nicety, and its absence must not cost us attribution.
        #[derive(Default)]
        struct Log(Mutex<Vec<Speaker>>);
        #[async_trait]
        impl VoiceTurnHandler for Log {
            async fn turn(&self, speaker: &Speaker, _text: &str) -> VoiceReply {
                self.0.lock().unwrap().push(speaker.clone());
                VoiceReply::Continue(String::new())
            }
        }

        let room = Arc::new(MockRoom::new("standup@x").script(vec![
            RoomEvent::Message {
                from: OccupantId::new("standup@x/ghost"),
                text: "who am i".into(),
                scope: MessageScope::Groupchat,
            },
            RoomEvent::Ended,
        ]));
        let log = Log::default();
        RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"))
            .run(&log, &CancellationToken::new())
            .await
            .unwrap();

        let seen = log.0.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id(), "standup@x/ghost");
        assert_eq!(seen[0].display_name(), None);
        assert_eq!(seen[0].label(), "standup@x/ghost", "the id is the fallback");
        assert!(room.said().is_empty(), "an empty reply says nothing");
    }

    #[tokio::test]
    async fn a_whisper_is_answered_privately() {
        let timo = Occupant::new("standup@x/timo", "timo", OccupantKind::Human);
        let room = Arc::new(
            MockRoom::new("standup@x")
                .with_occupant(timo.clone())
                .script(vec![
                    RoomEvent::Message {
                        from: timo.id.clone(),
                        text: "just us".into(),
                        scope: MessageScope::Private,
                    },
                    RoomEvent::Ended,
                ]),
        );
        RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"))
            .run(&Parrot("of course"), &CancellationToken::new())
            .await
            .unwrap();

        assert!(
            room.said().is_empty(),
            "a private question is not answered in public"
        );
        assert_eq!(
            room.whispered(),
            vec![(timo.id.clone(), "of course".to_string())]
        );
    }

    #[tokio::test]
    async fn cancelling_leaves_the_room() {
        // No `Ended` in the script: the stream stays open, and only the token ends the session.
        let room = Arc::new(MockRoom::new("standup@x"));
        let cancel = CancellationToken::new();
        cancel.cancel();
        RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"))
            .run(&Parrot("hi"), &cancel)
            .await
            .unwrap();
        assert!(room.has_left(), "a cancelled session leaves the room");
    }
}

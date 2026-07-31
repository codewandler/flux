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

use flux_core::{Error, Result};
use flux_flow::voice::{Speaker, VoiceReply, VoiceTurnHandler};

use super::{MessageScope, Occupant, OccupantId, Room, RoomEvent, RoomIdentity, RoomStream};

/// How a driven room session ended — the distinction a channel needs in order to decide whether a
/// failure is the *operator's* to fix or the *room's* to lose.
///
/// The two are not the same severity, and D-205 is where that started to matter: the host treats a
/// channel error as fatal to the whole process ("until a channel *errors* (fatal)",
/// [`crate::serve`]), which is right for a room that could never be joined — a wrong endpoint or a
/// refused credential is silently broken otherwise — and disproportionate for a socket that died
/// mid-meeting, which must not take a schedule and a webhook down with it. A join failure is
/// therefore [`RoomTurnDriver::run`]'s `Err`; anything after it is [`Self::Failed`].
#[derive(Debug)]
#[non_exhaustive]
pub enum RoomSessionEnd {
    /// The room ran and then finished: `Ended`, a dead transport, cancellation, or a handler's
    /// `Complete`.
    Ended,
    /// The session ran and then failed — a reply that could not reach the room, or a `leave` that
    /// could not complete. The room is gone; nothing else needs to be.
    Failed(Error),
}

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
    ///
    /// **However the session ends, the room is left** (D-205). Until a real backend landed, `say` was
    /// infallible and the difference could not be observed; on a live MUC a failed send used to return
    /// early past `leave`, leaving flux a visible occupant of a room it is no longer listening to.
    ///
    /// `Err` means the **join** failed and there was never a session — see [`RoomSessionEnd`] for why
    /// that is the one failure the caller should treat as fatal.
    pub async fn run(
        &self,
        handler: &dyn VoiceTurnHandler,
        cancel: &CancellationToken,
    ) -> Result<RoomSessionEnd> {
        let mut stream = self.room.join(&self.identity).await?;
        let session = self.session(handler, cancel, &mut stream).await;
        let left = self.room.leave().await;
        // The session's own failure is the more informative one; a `leave` that also failed on an
        // already-dead transport must not mask it.
        Ok(match session.and(left) {
            Ok(()) => RoomSessionEnd::Ended,
            Err(e) => RoomSessionEnd::Failed(e),
        })
    }

    /// The session proper, from the first event to the last. Split out so [`Self::run`] can leave the
    /// room on *every* path out of it, including the `?` on a failed send.
    async fn session(
        &self,
        handler: &dyn VoiceTurnHandler,
        cancel: &CancellationToken,
        stream: &mut RoomStream,
    ) -> Result<()> {
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
                    // Our own echo is not a user turn. Two independent signals, because getting this
                    // wrong is an unbounded loop that costs real provider money: the backend's
                    // `is_self` (authoritative — MUC self-presence necessarily precedes our own echo),
                    // and the nick we joined under, which covers a backend that built the occupant
                    // through `Occupant::new` and never decided (it defaults `is_self` to `false`).
                    // Nicks are unique within a MUC, so the second signal cannot misfire on someone
                    // else; a service that reassigned our nick is caught by the first.
                    if known.get(&from).is_some_and(|o| self.is_us(o)) {
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

        Ok(())
    }

    /// Whether an occupant is the one this session joined as — see the loop-prevention note at the
    /// call site for why this is two signals and not one.
    fn is_us(&self, occupant: &Occupant) -> bool {
        occupant.is_self || occupant.nick == self.identity.nick
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
    async fn a_failed_send_still_leaves_the_room() {
        // D-205's inherited defect: `say(…).await?` used to return past `leave`, so one failed send on
        // a real backend left flux a visible occupant of a room it had stopped listening to. Only a
        // fallible backend can observe it, and `MockRoom` is infallible — hence this double.
        struct FailingSend(MockRoom);
        #[async_trait]
        impl Room for FailingSend {
            fn id(&self) -> &crate::rooms::RoomId {
                self.0.id()
            }
            async fn join(
                &self,
                identity: &RoomIdentity,
            ) -> flux_core::Result<crate::rooms::RoomStream> {
                self.0.join(identity).await
            }
            async fn occupants(&self) -> flux_core::Result<Vec<Occupant>> {
                self.0.occupants().await
            }
            async fn say(&self, _text: &str) -> flux_core::Result<()> {
                Err(flux_core::Error::Other("the socket is gone".into()))
            }
            async fn whisper(&self, _to: &OccupantId, _text: &str) -> flux_core::Result<()> {
                Err(flux_core::Error::Other("the socket is gone".into()))
            }
            async fn leave(&self) -> flux_core::Result<()> {
                self.0.leave().await
            }
        }

        let timo = Occupant::new("standup@x/timo", "timo", OccupantKind::Human);
        let room = Arc::new(FailingSend(
            MockRoom::new("standup@x")
                .with_occupant(timo.clone())
                .script(vec![RoomEvent::Message {
                    from: timo.id.clone(),
                    text: "morning".into(),
                    scope: MessageScope::Groupchat,
                }]),
        ));
        let end = RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"))
            .run(&Parrot("hello"), &CancellationToken::new())
            .await
            .expect("a send that failed after joining is not a join failure");

        match end {
            RoomSessionEnd::Failed(e) => {
                assert!(e.to_string().contains("the socket is gone"), "{e}")
            }
            other => panic!("a dead transport is reported, not swallowed: {other:?}"),
        }
        assert!(
            room.0.has_left(),
            "the room is left even when the send that ended the session failed"
        );
    }

    #[tokio::test]
    async fn an_occupant_whose_backend_forgot_is_self_is_still_not_answered() {
        // `Occupant::new` defaults `is_self` to false, so a backend built through it is never forced
        // to decide — and a driver that trusts the flag alone then answers its own echoed groupchat
        // message forever. The nick we joined under is the second, independent signal. `MockRoom`
        // cannot stand in here: it marks itself correctly, which is exactly the bug's absence.
        struct Forgetful {
            id: crate::rooms::RoomId,
            said: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl Room for Forgetful {
            fn id(&self) -> &crate::rooms::RoomId {
                &self.id
            }
            async fn join(&self, _i: &RoomIdentity) -> flux_core::Result<RoomStream> {
                let (tx, stream) = crate::rooms::room_event_channel();
                let me = Occupant::new("standup@x/flux", "flux", OccupantKind::Agent);
                assert!(!me.is_self, "the defaulted flag this test is about");
                tokio::spawn(async move {
                    let _ = tx.send(RoomEvent::Joined { occupant: me }).await;
                    let _ = tx
                        .send(RoomEvent::Message {
                            from: OccupantId::new("standup@x/flux"),
                            text: "hello".into(),
                            scope: MessageScope::Groupchat,
                        })
                        .await;
                    let _ = tx.send(RoomEvent::Ended).await;
                });
                Ok(stream)
            }
            async fn occupants(&self) -> flux_core::Result<Vec<Occupant>> {
                Ok(Vec::new())
            }
            async fn say(&self, text: &str) -> flux_core::Result<()> {
                self.said.lock().unwrap().push(text.to_string());
                Ok(())
            }
            async fn whisper(&self, _to: &OccupantId, _text: &str) -> flux_core::Result<()> {
                Ok(())
            }
            async fn leave(&self) -> flux_core::Result<()> {
                Ok(())
            }
        }

        let room = Arc::new(Forgetful {
            id: crate::rooms::RoomId::new("standup@x"),
            said: Mutex::new(Vec::new()),
        });
        RoomTurnDriver::new(room.clone(), RoomIdentity::agent("flux"))
            .run(&Parrot("hello"), &CancellationToken::new())
            .await
            .unwrap();

        assert!(
            room.said.lock().unwrap().is_empty(),
            "answering our own echo is an unbounded loop, not a turn"
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

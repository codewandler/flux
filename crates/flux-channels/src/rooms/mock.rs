//! [`MockRoom`] — the in-process [`Room`] backend.
//!
//! The `mock` provider (`flux run -m mock`) is the precedent: a real, compiled-in implementation of a
//! port whose job is to make the layers above it exercisable with no network and no vendor. This one
//! records what flux said and replays a scripted transcript, so a room consumer can be tested for
//! attribution, addressing and reply behavior offline — and so the `room` channel kind has a working
//! backend before D-205 lands the XMPP one.

use std::sync::Mutex;

use async_trait::async_trait;

use flux_core::{Error, Result};

use super::{
    room_event_channel, Occupant, OccupantId, OccupantKind, Room, RoomEvent, RoomEventSender,
    RoomId, RoomIdentity, RoomStream,
};

/// An in-process room: a scripted inbound transcript and a recording of everything flux said.
pub struct MockRoom {
    id: RoomId,
    /// Occupants present before we join, replayed on [`Room::join`] as `Joined`.
    seeded: Vec<Occupant>,
    /// Events delivered after the presence replay, in order.
    script: Mutex<Vec<RoomEvent>>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Present occupants, ours included once joined.
    occupants: Vec<Occupant>,
    /// Every `say`, in order.
    said: Vec<String>,
    /// Every `whisper`, in order.
    whispered: Vec<(OccupantId, String)>,
    left: bool,
    /// Kept alive so a test can `emit` after the scripted events are drained; dropped by `leave`,
    /// which is what ends the consumer's stream.
    sender: Option<RoomEventSender>,
}

impl MockRoom {
    /// A room at `id` with nobody in it and nothing scripted.
    pub fn new(id: impl Into<RoomId>) -> Self {
        Self {
            id: id.into(),
            seeded: Vec::new(),
            script: Mutex::new(Vec::new()),
            state: Mutex::new(State::default()),
        }
    }

    /// Seed an occupant that is already present when flux joins.
    pub fn with_occupant(mut self, occupant: Occupant) -> Self {
        self.seeded.push(occupant);
        self
    }

    /// Script the events delivered after the presence replay, in order. A trailing
    /// [`RoomEvent::Ended`] ends the consumer's session; without one the stream stays open until the
    /// consumer leaves or cancels.
    pub fn script(self, events: Vec<RoomEvent>) -> Self {
        *self.script.lock().unwrap() = events;
        self
    }

    /// The occupant flux will appear as when it joins `room` under `nick`. Follows the MUC occupant-JID
    /// convention (`<room>/<nick>`) so a test can script our own echoed message without having to
    /// join first — a real MUC reflects every groupchat message back to its sender.
    pub fn self_occupant(room: &str, nick: &str) -> Occupant {
        Occupant::new(format!("{room}/{nick}"), nick, OccupantKind::Agent).as_self()
    }

    /// Deliver one more event to a joined consumer. `Err` when nobody is listening (not joined, or
    /// already left).
    pub async fn emit(&self, event: RoomEvent) -> Result<()> {
        let sender = self.state.lock().unwrap().sender.clone();
        let sender = sender.ok_or_else(|| Error::Other("mock room: not joined".into()))?;
        sender
            .send(event)
            .await
            .map_err(|_| Error::Other("mock room: the consumer is gone".into()))
    }

    /// Everything flux said to the whole room, in order.
    pub fn said(&self) -> Vec<String> {
        self.state.lock().unwrap().said.clone()
    }

    /// Every private message flux sent, in order.
    pub fn whispered(&self) -> Vec<(OccupantId, String)> {
        self.state.lock().unwrap().whispered.clone()
    }

    /// Whether flux left the room.
    pub fn has_left(&self) -> bool {
        self.state.lock().unwrap().left
    }
}

#[async_trait]
impl Room for MockRoom {
    fn id(&self) -> &RoomId {
        &self.id
    }

    async fn join(&self, identity: &RoomIdentity) -> Result<RoomStream> {
        let me = Self::self_occupant(self.id.as_str(), &identity.nick);
        let me = Occupant {
            kind: identity.kind,
            ..me
        };
        let scripted = std::mem::take(&mut *self.script.lock().unwrap());
        let (tx, stream) = room_event_channel();
        {
            let mut state = self.state.lock().unwrap();
            state.occupants = self.seeded.clone();
            state.occupants.push(me.clone());
            state.left = false;
            state.sender = Some(tx.clone());
        }

        // Presence replay first (ours included), then the script — the order a real MUC produces:
        // the server sends the existing occupants' presence before any message reaches us.
        let mut replay: Vec<RoomEvent> = self
            .seeded
            .iter()
            .cloned()
            .chain(std::iter::once(me))
            .map(|occupant| RoomEvent::Joined { occupant })
            .collect();
        replay.extend(scripted);
        // A bounded channel plus a consumer that has not started reading yet would deadlock a
        // synchronous fill, so the replay is a task. Dropping its sender when the script is drained
        // would end the stream, so it holds `tx` until the room is left.
        tokio::spawn(async move {
            for event in replay {
                if tx.send(event).await.is_err() {
                    return;
                }
            }
        });
        Ok(stream)
    }

    async fn occupants(&self) -> Result<Vec<Occupant>> {
        Ok(self.state.lock().unwrap().occupants.clone())
    }

    async fn say(&self, text: &str) -> Result<()> {
        self.state.lock().unwrap().said.push(text.to_string());
        Ok(())
    }

    async fn whisper(&self, to: &OccupantId, text: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .whispered
            .push((to.clone(), text.to_string()));
        Ok(())
    }

    async fn leave(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.left = true;
        state.occupants.retain(|o| !o.is_self);
        // Dropping the producing half ends the consumer's stream — a left room delivers nothing more.
        state.sender = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::MessageScope;

    #[tokio::test]
    async fn joining_replays_presence_including_ourselves() {
        let timo = Occupant::new("standup@x/timo", "timo", OccupantKind::Human);
        let room = MockRoom::new("standup@x").with_occupant(timo.clone());
        let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();

        assert_eq!(
            stream.recv().await,
            Some(RoomEvent::Joined {
                occupant: timo.clone()
            })
        );
        let me = match stream.recv().await {
            Some(RoomEvent::Joined { occupant }) => occupant,
            other => panic!("expected our own presence, got {other:?}"),
        };
        assert!(me.is_self, "we can recognize ourselves in the room");
        assert_eq!(me.id.as_str(), "standup@x/flux");
        assert_eq!(me.kind, OccupantKind::Agent);

        let occupants = room.occupants().await.unwrap();
        assert_eq!(occupants.len(), 2);
        assert!(occupants.iter().any(|o| o.is_self));
    }

    #[tokio::test]
    async fn leaving_ends_the_stream_and_records_what_was_said() {
        let room = MockRoom::new("standup@x");
        let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();
        assert!(matches!(
            stream.recv().await,
            Some(RoomEvent::Joined { .. })
        ));

        room.say("hello room").await.unwrap();
        room.whisper(&OccupantId::new("standup@x/timo"), "psst")
            .await
            .unwrap();
        room.leave().await.unwrap();

        assert_eq!(room.said(), vec!["hello room".to_string()]);
        assert_eq!(
            room.whispered(),
            vec![(OccupantId::new("standup@x/timo"), "psst".to_string())]
        );
        assert!(room.has_left());
        assert_eq!(
            stream.recv().await,
            None,
            "a left room delivers nothing more"
        );
        assert!(room.emit(RoomEvent::Ended).await.is_err());
    }

    #[tokio::test]
    async fn scripted_events_arrive_after_the_presence_replay() {
        let timo = Occupant::new("standup@x/timo", "timo", OccupantKind::Human);
        let room = MockRoom::new("standup@x")
            .with_occupant(timo.clone())
            .script(vec![RoomEvent::Message {
                from: timo.id.clone(),
                text: "morning".into(),
                scope: MessageScope::Groupchat,
            }]);
        let mut stream = room.join(&RoomIdentity::agent("flux")).await.unwrap();

        let kinds: Vec<RoomEvent> = vec![
            stream.recv().await.unwrap(),
            stream.recv().await.unwrap(),
            stream.recv().await.unwrap(),
        ];
        assert!(matches!(kinds[0], RoomEvent::Joined { .. }));
        assert!(matches!(kinds[1], RoomEvent::Joined { .. }));
        assert!(matches!(kinds[2], RoomEvent::Message { .. }));
    }
}

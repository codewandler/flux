//! [`RoomTranscript`] — the attributed context a room agent accumulates while it stays silent
//! (D-207).
//!
//! A room agent hears every turn and is the addressee of almost none of it. The turns it does not
//! answer are not noise: they are the conversation the eventual answer has to make sense inside. So
//! an unaddressed turn still goes *somewhere* — here — and what lands here keeps its
//! [`Speaker`], so an answer can refer to "what Timo asked" rather than to a flat blob of text with
//! N people's words run together.
//!
//! Two properties are deliberate:
//!
//! - **Bounded.** A meeting runs for an hour and flux may be addressed twice in it. An unbounded
//!   buffer of everything said in between is the same unbounded-cost mistake as answering every
//!   line, arriving one layer down — so the oldest line is dropped once
//!   [`RoomTranscript::capacity`] is reached.
//! - **Drained by the turn that uses it.** [`RoomTranscript::drain`] hands the accumulated lines to
//!   the turn being taken and empties the buffer, so the same context is never carried into two
//!   turns and re-billed.
//!
//! Nothing here interprets or reformats the text. It is untrusted room input on the way to a
//! payload that is fenced as such (C-407); this type carries it and attributes it, and that is all.

use std::collections::VecDeque;

use super::Speaker;

/// One line the agent heard, and who said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeardLine {
    /// Who said it — compare on [`Speaker::id`], never on the display name.
    pub speaker: Speaker,
    /// What they said, verbatim.
    pub text: String,
}

/// The lines a room agent overheard since its last answered turn, in the order they were said.
#[derive(Debug, Clone)]
pub struct RoomTranscript {
    capacity: usize,
    lines: VecDeque<HeardLine>,
}

/// How many overheard lines a room carries into the next answered turn. Enough to make a question
/// asked after some discussion answerable; small enough that an hour of chatter nobody addressed to
/// flux cannot become an hour of billed context.
pub const DEFAULT_ROOM_CONTEXT_LINES: usize = 40;

impl Default for RoomTranscript {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomTranscript {
    /// An empty transcript holding up to [`DEFAULT_ROOM_CONTEXT_LINES`] lines.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_ROOM_CONTEXT_LINES)
    }

    /// An empty transcript holding up to `capacity` lines. A capacity of zero is raised to one — a
    /// transcript that can hold nothing is a configuration mistake, not a feature, and silently
    /// discarding every line would be the hardest version of it to see.
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            lines: VecDeque::with_capacity(capacity),
        }
    }

    /// How many lines this transcript keeps before dropping the oldest.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Record one heard line, dropping the oldest if the buffer is full.
    pub fn push(&mut self, speaker: &Speaker, text: &str) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(HeardLine {
            speaker: speaker.clone(),
            text: text.to_string(),
        });
    }

    /// The accumulated lines, oldest first.
    pub fn lines(&self) -> impl ExactSizeIterator<Item = &HeardLine> {
        self.lines.iter()
    }

    /// How many lines are accumulated.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether nothing has been overheard since the last [`Self::drain`].
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Take the accumulated lines and empty the buffer — what the turn that finally answers does, so
    /// the same context is never carried into a second turn.
    pub fn drain(&mut self) -> Vec<HeardLine> {
        self.lines.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drained_transcript_records_who_said_what_in_order() {
        let mut t = RoomTranscript::new();
        let timo = Speaker::new("standup@x/timo").with_display_name("timo");
        let ada = Speaker::new("standup@x/ada").with_display_name("ada");
        t.push(&timo, "can someone look at the nightly?");
        t.push(&ada, "on it");

        let lines = t.drain();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].speaker.id(), "standup@x/timo");
        assert_eq!(lines[0].text, "can someone look at the nightly?");
        assert_eq!(lines[1].speaker.id(), "standup@x/ada");
        assert!(t.is_empty(), "the turn that used the context drained it");
        assert!(
            t.drain().is_empty(),
            "a second turn does not get the first turn's context again"
        );
    }

    #[test]
    fn two_occupants_sharing_a_nick_stay_two_speakers_in_the_context() {
        // A MUC nick is occupant-chosen and explicitly non-unique (C-408). Context attributed by
        // display name would merge these two into one voice.
        let mut t = RoomTranscript::new();
        t.push(
            &Speaker::new("standup@x/ada").with_display_name("ada"),
            "standup in five",
        );
        t.push(
            &Speaker::new("standup@x/ada2").with_display_name("ada"),
            "ignore that, standup is cancelled",
        );

        let lines = t.drain();
        assert_eq!(
            lines[0].speaker.display_name(),
            lines[1].speaker.display_name()
        );
        assert_ne!(
            lines[0].speaker.id(),
            lines[1].speaker.id(),
            "a shared nick is not a shared speaker"
        );
    }

    #[test]
    fn an_hour_of_chatter_cannot_grow_without_bound() {
        let mut t = RoomTranscript::with_capacity(3);
        let timo = Speaker::new("standup@x/timo");
        for i in 0..100 {
            t.push(&timo, &format!("line {i}"));
        }
        assert_eq!(t.len(), 3, "the buffer is bounded by its capacity");

        let lines = t.drain();
        assert_eq!(
            lines[0].text, "line 97",
            "the oldest lines are the ones dropped"
        );
        assert_eq!(lines[2].text, "line 99");
    }

    #[test]
    fn a_zero_capacity_transcript_still_keeps_a_line() {
        // Silently discarding everything would be the hardest version of this misconfiguration to
        // notice, so the floor is one line rather than none.
        let mut t = RoomTranscript::with_capacity(0);
        assert_eq!(t.capacity(), 1);
        t.push(&Speaker::sole(), "anybody there?");
        assert_eq!(t.len(), 1);
    }
}

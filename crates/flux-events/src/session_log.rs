//! The typed session-log handle (A-100) — a turn's lifecycle as a state machine at the write seam.
//!
//! [`shape`](crate::shape) (A-99) made the session-shape rules *types*; this module makes the
//! **transitions** types too. A turn's two writes — the user message that opens it and the
//! assistant message that closes it — happen ~750 lines apart in `flux-flow`'s engine, with nothing
//! pairing them. Every past break of the session-shape invariant was a newly added
//! turn-termination path that performed the first write and returned without the second, leaving
//! the log ending on a `user` message; the *next* turn's opening write then produced
//! `user`-after-`user`, and the provider 400'd.
//!
//! [`SessionLog`] closes that by construction. It carries the log's [`Tail`] — what the projected
//! conversation currently ends on — and exposes only transitions that preserve the invariant:
//! opening a turn that is already open is [`ShapeError::TurnAlreadyOpen`], not a silent append.
//!
//! Two properties are worth stating out loud, because they are what make the handle trustworthy
//! rather than merely convenient:
//!
//! - **`Tail` is a cache of the store's truth, never a second source of it.** [`open`](SessionLog::open)
//!   re-derives it from the log every time, so a crash mid-turn, a `serve` daemon writing the same
//!   `events.db`, or a handle held across an `await` cannot leave anything claiming a turn is
//!   closed when the log says otherwise.
//! - **Every append is conditional on the tail it was decided against.** The check and the insert
//!   run inside one `BEGIN IMMEDIATE` transaction, so two handles racing `open_turn` on one stream
//!   leave exactly one user message — the loser appends nothing and is told the turn is open.

use flux_core::{Message, Role};

use crate::kind::NewEvent;
use crate::projection;
use crate::shape::{AssistantMessage, ShapeError, ValidHistory};
use crate::store::EventStore;

/// How many times a transition re-derives its tail and retries after losing a compare-and-append.
///
/// A miss means another writer appended between this handle's derivation and its write. Re-deriving
/// usually turns the retry into a *legal-transition* answer instead (the racing writer opened the
/// turn, so this caller learns `TurnAlreadyOpen`); the bound exists only so a pathological stream of
/// concurrent writers ends in a loud error rather than an unbounded spin.
const CONTENDED_ATTEMPTS: usize = 4;

/// Where a session log's projected conversation currently ends — the turn lifecycle as one type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// No messages yet: a fresh session, or one whose last compaction snapshot was empty.
    Empty,
    /// The last message is the `user` half of a turn — an assistant answer is owed.
    AwaitingAssistant,
    /// The last message is an assistant answer — the turn is closed.
    Closed,
}

/// What went wrong at the write seam.
///
/// Split so a caller can tell "this write would have broken the session shape" (a bug in the
/// caller's control flow, actionable, and nothing was written) from "the store failed" (IO). Both
/// convert into [`flux_core::Error`], so a call site that only wants to `?` can.
#[derive(Debug)]
pub enum LogError {
    /// The write would have broken a session-shape invariant. **Nothing was appended.**
    Shape(ShapeError),
    /// The underlying event store failed.
    Store(flux_core::Error),
}

impl LogError {
    /// The shape violation, when that is what this is — the matcher a caller wants.
    pub fn shape(&self) -> Option<&ShapeError> {
        match self {
            Self::Shape(e) => Some(e),
            Self::Store(_) => None,
        }
    }
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape(e) => write!(f, "session log: {e}"),
            Self::Store(e) => write!(f, "session log: {e}"),
        }
    }
}

impl std::error::Error for LogError {}

impl From<ShapeError> for LogError {
    fn from(e: ShapeError) -> Self {
        Self::Shape(e)
    }
}

impl From<flux_core::Error> for LogError {
    fn from(e: flux_core::Error) -> Self {
        Self::Store(e)
    }
}

impl From<LogError> for flux_core::Error {
    fn from(e: LogError) -> Self {
        match e {
            LogError::Shape(inner) => flux_core::Error::Other(format!("session log: {inner}")),
            LogError::Store(inner) => inner,
        }
    }
}

/// A typed handle on one session's persisted conversation.
///
/// Obtained per stream through [`open`](Self::open), which derives the current [`Tail`] from the
/// log. Hold it for a turn, not for a process: it is cheap to re-open, and re-opening is what keeps
/// the tail honest.
pub struct SessionLog<'a> {
    store: &'a EventStore,
    stream: String,
    tail: Tail,
    /// The `stream_seq` of the newest message-affecting event that [`Self::tail`] was derived from
    /// (`-1` when the stream has none). Every append is conditional on it — see the module header.
    head: i64,
}

impl<'a> SessionLog<'a> {
    /// Open the log for `stream`, deriving its [`Tail`] from the store.
    ///
    /// The derivation reads the kind-filtered conversation projection (`message`/`compacted`), not
    /// the whole stream — a long session's plan/run/usage payloads are never fetched or decoded.
    pub fn open(store: &'a EventStore, stream: &str) -> Result<Self, LogError> {
        let mut log = Self {
            store,
            stream: stream.to_string(),
            tail: Tail::Empty,
            head: -1,
        };
        log.refresh()?;
        Ok(log)
    }

    /// The stream this handle writes to.
    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// What the log currently ends on, as of the last derivation.
    pub fn tail(&self) -> Tail {
        self.tail
    }

    /// Open a turn with the user's message. Legal from [`Tail::Empty`] and [`Tail::Closed`].
    ///
    /// From [`Tail::AwaitingAssistant`] this is [`ShapeError::TurnAlreadyOpen`] and **nothing is
    /// appended** — the `user`-after-`user` that a raw append would have produced.
    pub fn open_turn(&mut self, user: Message) -> Result<(), LogError> {
        if user.role != Role::User {
            return Err(ShapeError::NotAUserMessage { role: user.role }.into());
        }
        self.commit(
            NewEvent::message(user),
            Tail::AwaitingAssistant,
            |tail| match tail {
                Tail::AwaitingAssistant => Some(ShapeError::TurnAlreadyOpen),
                Tail::Empty | Tail::Closed => None,
            },
        )
    }

    /// Close the open turn with the assistant's answer. Legal only from
    /// [`Tail::AwaitingAssistant`]; otherwise [`ShapeError::NoTurnOpen`].
    ///
    /// It takes an [`AssistantMessage`] (A-99), so an empty answer cannot reach the log: there is
    /// no way to construct the argument.
    pub fn close_turn(&mut self, answer: AssistantMessage) -> Result<(), LogError> {
        self.commit(
            NewEvent::message(answer.into_message()),
            Tail::Closed,
            |tail| match tail {
                Tail::AwaitingAssistant => None,
                Tail::Empty | Tail::Closed => Some(ShapeError::NoTurnOpen),
            },
        )
    }

    /// Replace the whole projected history with `history`, as one `Compacted` event.
    ///
    /// Legal from any tail — this is compaction, fork, and replay, whose whole job is to install a
    /// different history. The *input* is what carries the guarantee: a [`ValidHistory`] has already
    /// been checked for empty assistants, split tool pairs, and broken alternation, so the log's
    /// new tail is whatever that checked sequence ends on.
    pub fn rewrite(&mut self, history: ValidHistory) -> Result<(), LogError> {
        let next = tail_of(history.as_slice());
        self.commit(NewEvent::compacted(history.into_inner()), next, |_| None)
    }

    /// Re-derive [`Self::tail`] and [`Self::head`] from the store — the only place either is set.
    fn refresh(&mut self) -> Result<(), LogError> {
        let events = self.store.conversation_delta(&self.stream, -1)?;
        self.head = events.last().map(|e| e.stream_seq).unwrap_or(-1);
        self.tail = tail_of(&projection::conversation(&events));
        Ok(())
    }

    /// The one write path: reject the transition if `illegal` says so, then append **conditional on
    /// the tail still being the one that decision was made against**. A guard miss means a
    /// concurrent writer moved the conversation, so re-derive and decide again — the racing writer
    /// may have made this transition illegal, which is precisely the answer the caller needs.
    fn commit(
        &mut self,
        ev: NewEvent,
        next: Tail,
        illegal: fn(Tail) -> Option<ShapeError>,
    ) -> Result<(), LogError> {
        for _ in 0..CONTENDED_ATTEMPTS {
            if let Some(e) = illegal(self.tail) {
                return Err(e.into());
            }
            if let Some(stored) =
                self.store
                    .append_if_conversation_head(&self.stream, ev.clone(), self.head)?
            {
                self.head = stored.stream_seq;
                self.tail = next;
                return Ok(());
            }
            self.refresh()?;
        }
        Err(LogError::Store(flux_core::Error::Other(format!(
            "stream {} stayed contended across {CONTENDED_ATTEMPTS} append attempts",
            self.stream
        ))))
    }
}

/// The tail a projected conversation ends on.
///
/// A `system` message cannot be written through this handle and is rejected by [`ValidHistory`], so
/// it only ever appears in a legacy log; it is read as `AwaitingAssistant` — the conservative arm,
/// which refuses to append a `user` message after it.
fn tail_of(messages: &[Message]) -> Tail {
    match messages.last().map(|m| m.role) {
        None => Tail::Empty,
        Some(Role::Assistant) => Tail::Closed,
        Some(_) => Tail::AwaitingAssistant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::decoded_kinds_during;
    use flux_core::Usage;
    use flux_lang::ast::RunEvent;
    use std::sync::{Arc, Barrier};

    fn store_with_session() -> (EventStore, String) {
        let store = EventStore::in_memory().unwrap();
        let s = store.create_session("m").unwrap();
        (store, s)
    }

    fn roles(store: &EventStore, s: &str) -> Vec<Role> {
        store
            .conversation(s)
            .unwrap()
            .iter()
            .map(|m| m.role)
            .collect()
    }

    // ---- the transition the raw API cannot refuse -----------------------------------------

    /// The failing-first test. `record_message` accepts any message unexamined, so the equivalent
    /// double-append through it **silently** produces `user`-after-`user` — the exact shape a
    /// provider rejects with a 400 on the next turn. Through the handle it is an `Err` with nothing
    /// appended.
    #[test]
    fn double_open_turn_is_rejected_where_record_message_silently_breaks_shape() {
        // What today's unguarded seam does with the same two writes.
        let (store, raw) = store_with_session();
        store
            .record_message(&raw, &Message::user_text("first"))
            .unwrap();
        store
            .record_message(&raw, &Message::user_text("second"))
            .unwrap();
        assert_eq!(
            roles(&store, &raw),
            vec![Role::User, Role::User],
            "the raw API appends a second user message without a word"
        );
        assert!(
            ValidHistory::new(store.conversation(&raw).unwrap()).is_err(),
            "and what it wrote is not a valid provider history"
        );

        // What the typed handle does.
        let s = store.create_session("m").unwrap();
        let mut log = SessionLog::open(&store, &s).unwrap();
        log.open_turn(Message::user_text("first")).unwrap();
        assert_eq!(log.tail(), Tail::AwaitingAssistant);

        let err = log.open_turn(Message::user_text("second")).unwrap_err();
        assert_eq!(err.shape(), Some(&ShapeError::TurnAlreadyOpen));
        assert_eq!(
            roles(&store, &s),
            vec![Role::User],
            "the rejected transition appended nothing"
        );
        assert_eq!(log.tail(), Tail::AwaitingAssistant, "and moved nothing");
    }

    #[test]
    fn open_turn_refuses_a_message_that_is_not_from_the_user() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();
        let err = log
            .open_turn(Message::assistant_text("not mine"))
            .unwrap_err();
        assert_eq!(
            err.shape(),
            Some(&ShapeError::NotAUserMessage {
                role: Role::Assistant
            })
        );
        assert!(store.conversation(&s).unwrap().is_empty());
    }

    #[test]
    fn a_turn_opens_and_closes() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();
        assert_eq!(log.tail(), Tail::Empty);
        log.open_turn(Message::user_text("hi")).unwrap();
        log.close_turn(AssistantMessage::text("hello").unwrap())
            .unwrap();
        assert_eq!(log.tail(), Tail::Closed);
        log.open_turn(Message::user_text("more")).unwrap();
        log.close_turn(AssistantMessage::text("ok").unwrap())
            .unwrap();
        assert_eq!(
            roles(&store, &s),
            vec![Role::User, Role::Assistant, Role::User, Role::Assistant]
        );
        assert!(ValidHistory::new(store.conversation(&s).unwrap()).is_ok());
    }

    #[test]
    fn close_turn_needs_an_open_turn() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();

        // From `Empty`.
        let err = log
            .close_turn(AssistantMessage::text("hi").unwrap())
            .unwrap_err();
        assert_eq!(err.shape(), Some(&ShapeError::NoTurnOpen));
        assert!(store.conversation(&s).unwrap().is_empty());

        // From `Closed`.
        log.open_turn(Message::user_text("q")).unwrap();
        log.close_turn(AssistantMessage::text("a").unwrap())
            .unwrap();
        let err = log
            .close_turn(AssistantMessage::text("again").unwrap())
            .unwrap_err();
        assert_eq!(err.shape(), Some(&ShapeError::NoTurnOpen));
        assert_eq!(roles(&store, &s), vec![Role::User, Role::Assistant]);
    }

    /// The empty-assistant shape never reaches a decision inside `close_turn`: its argument cannot
    /// be built. This is the A-99 constructor doing A-100's work for it.
    #[test]
    fn an_empty_answer_cannot_be_handed_to_close_turn() {
        assert_eq!(
            AssistantMessage::text("   "),
            Err(ShapeError::EmptyAssistant { index: 0 })
        );
    }

    // ---- the tail comes from the store, always ---------------------------------------------

    /// The property that makes `Tail` a cache rather than a second source of truth: a writer this
    /// handle never saw changed the log, and the next `open` says so.
    #[test]
    fn open_derives_the_tail_from_the_store_not_from_a_cached_handle() {
        let (store, s) = store_with_session();
        let mut first = SessionLog::open(&store, &s).unwrap();
        first.open_turn(Message::user_text("hi")).unwrap();
        assert_eq!(first.tail(), Tail::AwaitingAssistant);

        // A second handle, opened after that write, sees the turn as open.
        let second = SessionLog::open(&store, &s).unwrap();
        assert_eq!(second.tail(), Tail::AwaitingAssistant);

        // And after the answer lands, a fresh open sees a closed turn — while `second`'s own
        // cached tail is now stale, which is exactly why nothing trusts a held handle.
        first
            .close_turn(AssistantMessage::text("hello").unwrap())
            .unwrap();
        assert_eq!(second.tail(), Tail::AwaitingAssistant, "stale, as expected");
        assert_eq!(
            SessionLog::open(&store, &s).unwrap().tail(),
            Tail::Closed,
            "re-derived from the log"
        );
    }

    /// A stale handle cannot write against a tail that has moved: the append is conditional on the
    /// derivation, so the transition is re-decided against the log rather than trusted.
    #[test]
    fn a_stale_handle_cannot_append_against_a_tail_that_moved() {
        let (store, s) = store_with_session();
        let mut stale = SessionLog::open(&store, &s).unwrap();
        assert_eq!(stale.tail(), Tail::Empty);

        // Someone else opens the turn while `stale` still believes the log is empty.
        let mut other = SessionLog::open(&store, &s).unwrap();
        other.open_turn(Message::user_text("theirs")).unwrap();

        let err = stale.open_turn(Message::user_text("mine")).unwrap_err();
        assert_eq!(err.shape(), Some(&ShapeError::TurnAlreadyOpen));
        assert_eq!(roles(&store, &s), vec![Role::User]);
        assert_eq!(store.conversation(&s).unwrap()[0].text(), "theirs");
    }

    /// The tail derivation must survive a compaction: `Compacted` replaces the whole projected
    /// history, so the tail is whatever the *snapshot* ends on, not whatever preceded it.
    #[test]
    fn the_tail_follows_a_compaction_snapshot() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();
        log.open_turn(Message::user_text("hi")).unwrap();
        log.close_turn(AssistantMessage::text("hello").unwrap())
            .unwrap();

        // A snapshot ending on a user message leaves the turn open — for this handle and for the
        // next one to open the log.
        log.rewrite(ValidHistory::new(vec![Message::user_text("only me")]).unwrap())
            .unwrap();
        assert_eq!(log.tail(), Tail::AwaitingAssistant);
        assert_eq!(
            SessionLog::open(&store, &s).unwrap().tail(),
            Tail::AwaitingAssistant
        );
        // ...so the answer is what the log will accept, and a second user message is not.
        assert_eq!(
            log.open_turn(Message::user_text("no")).unwrap_err().shape(),
            Some(&ShapeError::TurnAlreadyOpen)
        );
        log.close_turn(AssistantMessage::text("answer").unwrap())
            .unwrap();
        assert_eq!(roles(&store, &s), vec![Role::User, Role::Assistant]);
    }

    // ---- rewrite --------------------------------------------------------------------------

    #[test]
    fn rewrite_replaces_the_history_with_one_compacted_event() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();
        for i in 0..3 {
            log.open_turn(Message::user_text(format!("u{i}"))).unwrap();
            log.close_turn(AssistantMessage::text(format!("a{i}")).unwrap())
                .unwrap();
        }
        let before = store.load_by_kind(&s, "compacted").unwrap().len();

        let kept = ValidHistory::new(vec![
            Message::user_text("[summary of earlier conversation]"),
            Message::assistant_text("a2"),
        ])
        .unwrap();
        log.rewrite(kept).unwrap();

        assert_eq!(
            store
                .conversation(&s)
                .unwrap()
                .iter()
                .map(|m| m.text())
                .collect::<Vec<_>>(),
            vec!["[summary of earlier conversation]", "a2"]
        );
        assert_eq!(
            store.load_by_kind(&s, "compacted").unwrap().len() - before,
            1,
            "one Compacted event, not one append per kept message"
        );
        assert_eq!(log.tail(), Tail::Closed);
        // The rewritten log is immediately writable again through the same handle.
        log.open_turn(Message::user_text("next")).unwrap();
        assert!(ValidHistory::new(store.conversation(&s).unwrap()).is_ok());
    }

    #[test]
    fn rewrite_to_an_empty_history_reopens_the_log_as_empty() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();
        log.open_turn(Message::user_text("hi")).unwrap();
        log.rewrite(ValidHistory::new(vec![]).unwrap()).unwrap();
        assert_eq!(log.tail(), Tail::Empty);
        assert_eq!(SessionLog::open(&store, &s).unwrap().tail(), Tail::Empty);
        // `Empty` accepts a fresh turn — the rewrite really did reset the lifecycle.
        log.open_turn(Message::user_text("again")).unwrap();
        assert_eq!(roles(&store, &s), vec![Role::User]);
    }

    // ---- the derivation is kind-filtered ---------------------------------------------------

    /// `open` must not scan the stream. A long session is mostly run/usage/observation payloads,
    /// and paying to decode them on every handle open would make the typed seam a per-turn tax —
    /// so the derivation goes through the kind-filtered conversation read, and this asserts it by
    /// observing what actually reached the decoder.
    #[test]
    fn open_decodes_only_message_kind_events() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();
        log.open_turn(Message::user_text("hi")).unwrap();
        log.close_turn(AssistantMessage::text("hello").unwrap())
            .unwrap();

        let turn = store.begin_turn(&s, "go", "m").unwrap();
        for i in 0..200 {
            store
                .record_run_event(
                    &s,
                    &RunEvent::StepSucceeded {
                        step: format!("s{i}").into(),
                        output: "v".into(),
                    },
                )
                .unwrap();
            store
                .record_call_usage(
                    &s,
                    turn,
                    "m",
                    Usage {
                        input_tokens: 5,
                        ..Default::default()
                    },
                )
                .unwrap();
        }

        let (opened, decoded) = decoded_kinds_during(|| SessionLog::open(&store, &s).unwrap());
        assert_eq!(opened.tail(), Tail::Closed);
        assert_eq!(
            decoded,
            vec!["message", "message"],
            "open decoded {} events on a stream of 400+ non-message facts",
            decoded.len()
        );
    }

    // ---- concurrency ------------------------------------------------------------------------

    /// Two handles racing `open_turn` on one stream leave **exactly one** user message. Both derive
    /// `Empty`, both consider the transition legal, and only the compare-and-append inside the
    /// write transaction can break the tie — a check-then-append would let both through.
    #[test]
    fn racing_open_turn_leaves_exactly_one_user_message() {
        let (store, s) = store_with_session();
        let store = Arc::new(store);
        let gate = Arc::new(Barrier::new(2));

        let outcomes: Vec<Result<(), LogError>> = std::thread::scope(|scope| {
            let handles: Vec<_> = ["a", "b"]
                .into_iter()
                .map(|who| {
                    let (store, gate, s) = (Arc::clone(&store), Arc::clone(&gate), s.clone());
                    scope.spawn(move || {
                        let mut log = SessionLog::open(&store, &s).unwrap();
                        gate.wait();
                        log.open_turn(Message::user_text(who))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        assert_eq!(
            outcomes.iter().filter(|r| r.is_ok()).count(),
            1,
            "exactly one writer opened the turn"
        );
        let loser = outcomes.into_iter().find_map(|r| r.err()).unwrap();
        assert_eq!(loser.shape(), Some(&ShapeError::TurnAlreadyOpen));
        assert_eq!(roles(&store, &s), vec![Role::User]);
        assert!(ValidHistory::new(store.conversation(&s).unwrap()).is_ok());
    }

    /// The same race one step later: a closing writer and an opening writer. Whoever loses is told
    /// what the log actually is, and the log stays a valid alternation either way.
    #[test]
    fn racing_close_and_open_leave_a_valid_history() {
        let (store, s) = store_with_session();
        SessionLog::open(&store, &s)
            .unwrap()
            .open_turn(Message::user_text("q"))
            .unwrap();
        let store = Arc::new(store);
        let gate = Arc::new(Barrier::new(2));

        std::thread::scope(|scope| {
            let closer = {
                let (store, gate, s) = (Arc::clone(&store), Arc::clone(&gate), s.clone());
                scope.spawn(move || {
                    let mut log = SessionLog::open(&store, &s).unwrap();
                    gate.wait();
                    log.close_turn(AssistantMessage::text("a").unwrap())
                })
            };
            let opener = {
                let (store, gate, s) = (Arc::clone(&store), Arc::clone(&gate), s.clone());
                scope.spawn(move || {
                    let mut log = SessionLog::open(&store, &s).unwrap();
                    gate.wait();
                    log.open_turn(Message::user_text("interrupting"))
                })
            };
            closer.join().unwrap().unwrap();
            // The opener saw an open turn either way: it derived `AwaitingAssistant`, or its
            // compare-and-append lost and it re-derived the same answer.
            let err = opener.join().unwrap().unwrap_err();
            assert_eq!(err.shape(), Some(&ShapeError::TurnAlreadyOpen));
        });

        assert_eq!(roles(&store, &s), vec![Role::User, Role::Assistant]);
        assert!(ValidHistory::new(store.conversation(&s).unwrap()).is_ok());
    }

    // ---- errors -----------------------------------------------------------------------------

    #[test]
    fn a_shape_rejection_renders_the_invariant_it_protects() {
        let (store, s) = store_with_session();
        let mut log = SessionLog::open(&store, &s).unwrap();
        log.open_turn(Message::user_text("hi")).unwrap();
        let rendered = log
            .open_turn(Message::user_text("again"))
            .unwrap_err()
            .to_string();
        assert!(rendered.contains("already open"), "{rendered}");
        // And it converts into the crate-wide error type for call sites that only want `?`.
        let as_core: flux_core::Error = LogError::Shape(ShapeError::TurnAlreadyOpen).into();
        assert!(as_core.to_string().contains("already open"));
    }
}

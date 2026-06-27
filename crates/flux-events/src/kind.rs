//! Event kinds and the append/read envelopes.
//!
//! [`EventKind`] is the **closed set** of facts flux logs. A single Rust enum (not an
//! open type registry) is the right model for a single closed binary: serde gives free
//! (de)serialization and an exhaustive `match` forces every projection to handle every
//! kind at compile time. It is **adjacently tagged** (`{"kind": …, "data": …}`) because
//! two variants wrap another enum/struct ([`EventKind::Run`] wraps [`RunEvent`],
//! [`EventKind::Message`] wraps [`Message`]) — internal tagging cannot flatten a nested
//! enum, and the split mirrors the DB's `kind`-column-plus-`payload`-JSON layout.

use serde::{Deserialize, Serialize};

use flux_core::Message;
use flux_lang::ast::RunEvent;

/// The closed set of event kinds in flux's unified log. Adding a kind of fact is one
/// new variant here plus one projection arm — never a new table and new methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    /// A session was created with `model`. The first event in every stream.
    SessionStarted { model: String },
    /// The session's model was switched mid-stream (`/model`).
    ModelChanged { model: String },

    /// One appended provider message. Folding these in stream order rebuilds the
    /// conversation — the projection that replaces `SessionStore::load_messages`.
    Message(Message),
    /// Context compaction: the live conversation was rewritten to `messages`. Append-only,
    /// so the superseded [`EventKind::Message`] events stay in the log; the conversation
    /// projection resets to this snapshot and continues. Replaces the destructive
    /// `SessionStore::rewrite_messages` (which did `DELETE FROM messages`).
    Compacted { messages: Vec<Message> },

    /// A flow run-trace event (replaces the `run_events` table). The inner type is reused
    /// verbatim from `flux-lang` rather than re-defined.
    Run(RunEvent),

    /// A user turn started. The `global_seq` of this event is the `turn_id` that scopes the
    /// turn's [`EventKind::PlanAttempted`] / [`EventKind::TurnEnded`] events.
    TurnStarted { user_input: String, model: String },
    /// One planning attempt within a turn. `outcome` is one of `"accepted"`, `"chat"`,
    /// `"compile_error"`; `error` carries the diagnostic when it is `"compile_error"`.
    PlanAttempted {
        step: u32,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A turn closed with its final `outcome`, iteration count, and assistant `answer`.
    TurnEnded {
        outcome: String,
        iterations: u32,
        answer: String,
    },
}

impl EventKind {
    /// The cheap `&'static str` discriminator written to the indexed `kind` column, so SQL
    /// filters compare a stable, allocation-free tag. Kept identical to serde's tag value.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            EventKind::SessionStarted { .. } => "session_started",
            EventKind::ModelChanged { .. } => "model_changed",
            EventKind::Message(_) => "message",
            EventKind::Compacted { .. } => "compacted",
            EventKind::Run(_) => "run",
            EventKind::TurnStarted { .. } => "turn_started",
            EventKind::PlanAttempted { .. } => "plan_attempted",
            EventKind::TurnEnded { .. } => "turn_ended",
        }
    }
}

/// An event to append. The store mints the `id` (when `None`), assigns `stream_seq` and
/// `global_seq`, and stamps `ts` — so callers only describe *what* happened.
#[derive(Debug, Clone)]
pub struct NewEvent {
    /// A caller-supplied id makes the append idempotent (a retry with the same id is a
    /// no-op returning the prior event). The store mints a ULID when this is `None`.
    pub id: Option<String>,
    /// What happened.
    pub kind: EventKind,
    /// The turn this event belongs to (the `global_seq` of its `TurnStarted`), if any.
    pub turn_id: Option<i64>,
    /// Payload schema version (defaults to 1).
    pub schema_version: u32,
}

impl NewEvent {
    /// A bare event with a store-minted id, no turn scope, schema version 1.
    pub fn new(kind: EventKind) -> Self {
        Self {
            id: None,
            kind,
            turn_id: None,
            schema_version: 1,
        }
    }

    /// Supply a stable id for at-most-once semantics.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Scope this event to a turn (a `TurnStarted`'s `global_seq`).
    pub fn in_turn(mut self, turn_id: i64) -> Self {
        self.turn_id = Some(turn_id);
        self
    }

    /// A conversation message event.
    pub fn message(m: Message) -> Self {
        Self::new(EventKind::Message(m))
    }

    /// A compaction snapshot event.
    pub fn compacted(messages: Vec<Message>) -> Self {
        Self::new(EventKind::Compacted { messages })
    }

    /// A flow run-trace event.
    pub fn run(ev: RunEvent) -> Self {
        Self::new(EventKind::Run(ev))
    }
}

/// An event read back from the log, with its `payload` decoded into [`StoredEvent::kind`].
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    /// Total order across all streams (the table's autoincrement rowid).
    pub global_seq: i64,
    /// The stream (session id, e.g. `"s_42"`) this event belongs to.
    pub stream: String,
    /// 0-based order within the stream.
    pub stream_seq: i64,
    /// The event's stable id (ULID unless caller-supplied).
    pub id: String,
    /// The turn this event belongs to, if any.
    pub turn_id: Option<i64>,
    /// Payload schema version.
    pub schema_version: u32,
    /// Wall-clock timestamp, unix milliseconds.
    pub ts_ms: i64,
    /// The decoded event.
    pub kind: EventKind,
}

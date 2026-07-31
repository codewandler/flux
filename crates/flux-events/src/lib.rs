//! `flux-events` — flux's unified, append-only event store.
//!
//! One ordered log holds every durable fact: conversation messages, flow run-trace events,
//! and per-turn telemetry. The store never mutates or deletes history (except pruning a
//! session that recorded nothing). The views we actually consume — the **conversation**, the
//! **run trace**, the **turn metrics** — are *projections*: pure folds over the log
//! ([`mod@projection`]). Adding a new kind of fact is one new [`EventKind`] variant plus one
//! projection arm; it never grows a new table or a new bespoke method.
//!
//! This replaces the old `flux-session` message log *and* `flux-flow`'s `run_events` /
//! `turn_log` / `plan_attempts` tables — three separate append-only logs collapsed into one.
//!
//! Conversation messages are written only through [`SessionLog`] (A-100/A-102), the typed handle
//! that makes the session-shape invariant hold by construction — there is no unguarded
//! "append a message" helper on the store to reach for instead.
//!
//! ```
//! use flux_events::{AssistantMessage, EventStore, SessionLog};
//! use flux_core::Message;
//!
//! let store = EventStore::in_memory().unwrap();
//! let s = store.create_session("claude-sonnet-4-6").unwrap();
//! let mut log = SessionLog::open(&store, &s).unwrap();
//! log.open_turn(Message::user_text("hi")).unwrap();
//! log.close_turn(AssistantMessage::text("hello").unwrap()).unwrap();
//! assert_eq!(store.conversation(&s).unwrap().len(), 2);
//! ```

mod context;
mod kind;
pub mod memory;
#[cfg(feature = "otel")]
pub mod otel;
mod projection;
pub mod retention;
mod session_log;
mod shape;
mod store;

pub use context::EventContext;
pub use kind::{EventKind, NewEvent, StoredEvent};
pub use memory::{GitPin, MemoryEntry, MemoryNote, MemoryScope, MemoryTombstone, Receipt};
pub use projection::{
    conversation, cost_summary, efficiency_summary, memory_entries, observations, pending_wakeups,
    render_run_diff, run_diff, run_trace, stmt_rows, stmt_texts, turns, DiffLineKind, DiffRow,
    EfficiencySummary, ModelCost, PendingWakeup, PlanAttempt, RunDiff, StmtRow, TurnSummary,
};
pub use retention::{
    is_retained_from_adhoc_prune, AdhocRetention, AdhocStreamFamily, ADHOC_STREAM_FAMILIES,
};
pub use session_log::{LogError, SessionLog, Tail};
pub use shape::{AssistantMessage, ShapeError, ValidHistory};
pub use store::{EventStore, SessionFilter, SessionInfo, SessionSummary};

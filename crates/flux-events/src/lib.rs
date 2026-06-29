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
//! ```
//! use flux_events::EventStore;
//! use flux_core::Message;
//!
//! let store = EventStore::in_memory().unwrap();
//! let s = store.create_session("claude-sonnet-4-6").unwrap();
//! store.record_message(&s, &Message::user_text("hi")).unwrap();
//! store.record_message(&s, &Message::assistant_text("hello")).unwrap();
//! assert_eq!(store.conversation(&s).unwrap().len(), 2);
//! ```

mod context;
mod kind;
mod projection;
mod store;

pub use context::EventContext;
pub use kind::{EventKind, NewEvent, StoredEvent};
pub use projection::{conversation, run_trace, turns, PlanAttempt, TurnSummary};
pub use store::{EventStore, SessionInfo, SessionSummary};

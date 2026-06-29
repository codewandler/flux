//! Read models derived by folding the event log.
//!
//! These are pure functions over `&[StoredEvent]` (so they're trivially testable). The
//! [`EventStore`](crate::EventStore) wraps each one with a load step for ergonomic call
//! sites. The conversation projection is the headline: the "conversations view" we mainly
//! used the session store for is now *derived* from the log rather than stored directly.

use std::collections::BTreeMap;

use flux_core::{Message, Usage};
use flux_lang::ast::RunEvent;

use crate::kind::{EventKind, StoredEvent};

/// Rebuild the conversation by replaying message-kind events in stream order. A
/// [`EventKind::Compacted`] snapshot resets the fold (the superseded messages stay on
/// disk but no longer surface) — this is the append-only equivalent of the old
/// destructive `rewrite_messages`. Reproduces `SessionStore::load_messages`.
pub fn conversation(events: &[StoredEvent]) -> Vec<Message> {
    let mut out = Vec::new();
    for e in events {
        match &e.kind {
            EventKind::Message(m) => out.push(m.clone()),
            EventKind::Compacted { messages } => {
                out.clear();
                out.extend(messages.iter().cloned());
            }
            // lifecycle / run / turn events don't touch the conversation
            _ => {}
        }
    }
    out
}

/// The flow run-trace for a stream, in order. Reproduces `FlowStore::events`.
pub fn run_trace(events: &[StoredEvent]) -> Vec<RunEvent> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Run(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

/// One planning attempt within a turn (the old `plan_attempts` row).
#[derive(Debug, Clone, PartialEq)]
pub struct PlanAttempt {
    pub step: u32,
    pub outcome: String,
    pub error: Option<String>,
}

/// A turn's telemetry, folded from its `TurnStarted` / `PlanAttempted` / `TurnEnded`
/// events (the old `turn_log` row plus its `plan_attempts`). `ended_at_ms` is `None` and
/// `outcome` stays `"pending"` for a turn that never closed.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    pub turn_id: i64,
    pub user_input: String,
    pub model: String,
    pub outcome: String,
    pub iterations: u32,
    pub answer: Option<String>,
    pub plan_attempts: Vec<PlanAttempt>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    /// The turn's accumulated token usage, when recorded (`None` for older logs / no provider usage).
    pub usage: Option<Usage>,
}

/// Fold turn telemetry, keyed (and ordered) by `turn_id` = the `TurnStarted`'s `global_seq`.
/// Reproduces what the `turn_log` + `plan_attempts` tables were for.
pub fn turns(events: &[StoredEvent]) -> Vec<TurnSummary> {
    let mut by_turn: BTreeMap<i64, TurnSummary> = BTreeMap::new();
    for e in events {
        match &e.kind {
            EventKind::TurnStarted { user_input, model } => {
                by_turn.insert(
                    e.global_seq,
                    TurnSummary {
                        turn_id: e.global_seq,
                        user_input: user_input.clone(),
                        model: model.clone(),
                        outcome: "pending".to_string(),
                        iterations: 0,
                        answer: None,
                        plan_attempts: Vec::new(),
                        started_at_ms: e.ts_ms,
                        ended_at_ms: None,
                        usage: None,
                    },
                );
            }
            EventKind::PlanAttempted {
                step,
                outcome,
                error,
            } => {
                if let Some(t) = e.turn_id.and_then(|tid| by_turn.get_mut(&tid)) {
                    t.plan_attempts.push(PlanAttempt {
                        step: *step,
                        outcome: outcome.clone(),
                        error: error.clone(),
                    });
                }
            }
            EventKind::TurnEnded {
                outcome,
                iterations,
                answer,
                usage,
            } => {
                if let Some(t) = e.turn_id.and_then(|tid| by_turn.get_mut(&tid)) {
                    t.outcome = outcome.clone();
                    t.iterations = *iterations;
                    t.answer = Some(answer.clone());
                    t.ended_at_ms = Some(e.ts_ms);
                    t.usage = usage.clone();
                }
            }
            _ => {}
        }
    }
    by_turn.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::EventKind;
    use flux_core::Message;

    /// Build a minimal StoredEvent for projection unit tests.
    fn ev(global_seq: i64, stream_seq: i64, turn_id: Option<i64>, kind: EventKind) -> StoredEvent {
        StoredEvent {
            global_seq,
            stream: "s_1".to_string(),
            stream_seq,
            id: format!("e{global_seq}"),
            turn_id,
            schema_version: 1,
            ts_ms: 1000 + global_seq,
            kind,
        }
    }

    #[test]
    fn conversation_folds_messages_in_order() {
        let events = vec![
            ev(1, 0, None, EventKind::SessionStarted { model: "m".into() }),
            ev(2, 1, None, EventKind::Message(Message::user_text("hi"))),
            ev(
                3,
                2,
                None,
                EventKind::Run(RunEvent::FlowReturned {
                    value: "v_1".into(),
                }),
            ),
            ev(
                4,
                3,
                None,
                EventKind::Message(Message::assistant_text("hello")),
            ),
        ];
        let convo = conversation(&events);
        assert_eq!(convo.len(), 2);
        assert_eq!(convo[0].text(), "hi");
        assert_eq!(convo[1].text(), "hello");
    }

    #[test]
    fn compaction_resets_the_fold_then_continues() {
        let events = vec![
            ev(1, 0, None, EventKind::Message(Message::user_text("a"))),
            ev(2, 1, None, EventKind::Message(Message::user_text("b"))),
            ev(
                3,
                2,
                None,
                EventKind::Compacted {
                    messages: vec![Message::user_text("summary"), Message::user_text("recent")],
                },
            ),
            ev(4, 3, None, EventKind::Message(Message::user_text("more"))),
        ];
        let convo = conversation(&events);
        assert_eq!(
            convo.iter().map(|m| m.text()).collect::<Vec<_>>(),
            vec!["summary", "recent", "more"]
        );
    }

    #[test]
    fn multiple_compactions_keep_only_the_latest_snapshot() {
        let events = vec![
            ev(1, 0, None, EventKind::Message(Message::user_text("a"))),
            ev(
                2,
                1,
                None,
                EventKind::Compacted {
                    messages: vec![Message::user_text("first")],
                },
            ),
            ev(
                3,
                2,
                None,
                EventKind::Compacted {
                    messages: vec![Message::user_text("second")],
                },
            ),
        ];
        let convo = conversation(&events);
        assert_eq!(convo.len(), 1);
        assert_eq!(convo[0].text(), "second");
    }

    #[test]
    fn run_trace_keeps_only_run_events_in_order() {
        let events = vec![
            ev(1, 0, None, EventKind::Message(Message::user_text("hi"))),
            ev(
                2,
                1,
                None,
                EventKind::Run(RunEvent::StepSucceeded {
                    step: "s1".into(),
                    output: "v_1".into(),
                }),
            ),
            ev(
                3,
                2,
                None,
                EventKind::Run(RunEvent::FlowReturned {
                    value: "v_1".into(),
                }),
            ),
        ];
        let trace = run_trace(&events);
        assert_eq!(trace.len(), 2);
        assert!(matches!(trace[0], RunEvent::StepSucceeded { .. }));
        assert!(matches!(trace[1], RunEvent::FlowReturned { .. }));
    }

    #[test]
    fn turns_fold_telemetry_by_turn_id() {
        let events = vec![
            ev(
                10,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "do it".into(),
                    model: "m".into(),
                },
            ),
            ev(
                11,
                1,
                Some(10),
                EventKind::PlanAttempted {
                    step: 0,
                    outcome: "compile_error".into(),
                    error: Some("boom".into()),
                },
            ),
            ev(
                12,
                2,
                Some(10),
                EventKind::PlanAttempted {
                    step: 1,
                    outcome: "accepted".into(),
                    error: None,
                },
            ),
            ev(
                13,
                3,
                Some(10),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 2,
                    answer: "done".into(),
                    usage: Some(Usage {
                        input_tokens: 100,
                        output_tokens: 20,
                        ..Default::default()
                    }),
                },
            ),
        ];
        let turns = turns(&events);
        assert_eq!(turns.len(), 1);
        let t = &turns[0];
        assert_eq!(t.turn_id, 10);
        assert_eq!(t.user_input, "do it");
        assert_eq!(t.outcome, "accepted");
        assert_eq!(t.iterations, 2);
        assert_eq!(t.answer.as_deref(), Some("done"));
        assert_eq!(t.plan_attempts.len(), 2);
        assert_eq!(t.plan_attempts[0].outcome, "compile_error");
        assert_eq!(t.plan_attempts[0].error.as_deref(), Some("boom"));
        assert!(t.ended_at_ms.is_some());
        assert_eq!(t.usage.as_ref().map(|u| u.total()), Some(120));
    }

    #[test]
    fn unclosed_turn_stays_pending() {
        let events = vec![ev(
            10,
            0,
            None,
            EventKind::TurnStarted {
                user_input: "hi".into(),
                model: "m".into(),
            },
        )];
        let turns = turns(&events);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "pending");
        assert!(turns[0].ended_at_ms.is_none());
    }
}

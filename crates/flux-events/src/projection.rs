//! Read models derived by folding the event log.
//!
//! These are pure functions over `&[StoredEvent]` (so they're trivially testable). The
//! [`EventStore`](crate::EventStore) wraps each one with a load step for ergonomic call
//! sites. The conversation projection is the headline: the "conversations view" we mainly
//! used the session store for is now *derived* from the log rather than stored directly.

use std::collections::BTreeMap;

use flux_core::{Message, Money, PricingTable, Usage};
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

/// One planning attempt within a turn (the old `plan_attempts` row). Also the WRITE shape
/// [`EventStore::record_plan_attempt`](crate::EventStore::record_plan_attempt) takes — one struct,
/// no field drift between what is recorded and what the fold reads back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanAttempt {
    pub step: u32,
    pub outcome: String,
    pub error: Option<String>,
    /// SHA-256 of the accepted plan AST's canonical JSON (the loop guard's identity) — `None` for
    /// non-plan outcomes and pre-C-14 logs.
    pub fingerprint: Option<String>,
    /// The human-auditable rendered plan graph (`render_pretty`, capped) — the durable "a turn is a
    /// readable graph" record. `None` for non-plan outcomes and pre-C-14 logs.
    pub plan_text: Option<String>,
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
                fingerprint,
                plan_text,
            } => {
                if let Some(t) = e.turn_id.and_then(|tid| by_turn.get_mut(&tid)) {
                    t.plan_attempts.push(PlanAttempt {
                        step: *step,
                        outcome: outcome.clone(),
                        error: error.clone(),
                        fingerprint: fingerprint.clone(),
                        plan_text: plan_text.clone(),
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

/// The durable evidence trail: every persisted observation, in stream order (C-14). This is the
/// offline/programmatic read of what `/evidence` shows live from the in-memory log — plan the
/// `tool_call` markers, `turn.iteration` rounds, `groups.active` (+ signals), skill activations,
/// and flow-emitted `observe(…)` records land here via the engine's per-turn watermark flush.
pub fn observations(events: &[StoredEvent]) -> Vec<flux_evidence::Observation> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Observation(o) => Some(o.clone()),
            _ => None,
        })
        .collect()
}

/// One model's rolled-up token spend + cost — a row of [`cost_summary`].
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCost {
    /// The model id as it was recorded on the event (bare id or `provider/model`, whichever the
    /// caller stamped — see [`EventKind::CallUsage`]).
    pub model: String,
    /// Field-wise sum across every call attributed to this model, ALL tiers included —
    /// `reasoning_tokens` too, unlike [`Usage::total`]/[`Usage::accumulate`], since [`cost_summary`]
    /// exists to price spend and [`flux_core::pricing::cost`] bills reasoning as its own tier.
    pub usage: Usage,
    /// The number of calls (or turns, for the old-log fallback) folded into `usage`.
    pub calls: u64,
    /// The priced cost of `usage` under `model`, when the model is known to the pricing table.
    /// `None` for a model the table has no rates for (never a panic).
    pub cost: Option<Money>,
}

/// Field-wise-sum one call's usage into a running total. Every tier is summed here — including
/// `reasoning_tokens` — because this is a cost rollup, not a context-window occupancy figure: each
/// call's reasoning tokens are a real (if usually zero-priced) cost line, and folding them keeps
/// [`ModelCost::usage`] a faithful total across every tier `pricing::cost` can charge for. This is
/// deliberately NOT [`Usage::accumulate`] (which replaces the input/cache side per call — correct for
/// a live turn's context-window occupancy, wrong for summing many independent calls' spend).
fn sum_usage(acc: &mut Usage, call: &Usage) {
    acc.input_tokens += call.input_tokens;
    acc.output_tokens += call.output_tokens;
    acc.cache_creation_input_tokens += call.cache_creation_input_tokens;
    acc.cache_read_input_tokens += call.cache_read_input_tokens;
    acc.reasoning_tokens += call.reasoning_tokens;
}

/// Roll up token spend + cost by model, folding a stream's events. Prefers the per-call
/// [`EventKind::CallUsage`] attribution (C-06); a stream with none recorded (an older log, written
/// before per-call attribution existed) falls back to summing each turn's [`EventKind::TurnEnded`]
/// total, attributed to that turn's [`EventKind::TurnStarted`] model — coarser (turn-level, not
/// call-level), but every old log still rolls up rather than reporting nothing. The two sources are
/// never mixed within one stream: a stream that recorded even one `CallUsage` uses ONLY those (the
/// turn totals they came from would double-count the same spend). Rows are sorted by model id for a
/// stable, diffable report.
pub fn cost_summary(events: &[StoredEvent], pricing: &PricingTable) -> Vec<ModelCost> {
    let mut per_model: BTreeMap<String, (Usage, u64)> = BTreeMap::new();
    let mut any_call_usage = false;

    for e in events {
        if let EventKind::CallUsage { model, usage } = &e.kind {
            any_call_usage = true;
            let entry = per_model.entry(model.clone()).or_default();
            sum_usage(&mut entry.0, usage);
            entry.1 += 1;
        }
    }

    if !any_call_usage {
        // Fallback: attribute each turn's total to the model active when that turn STARTED (the
        // `turns()` join point) — the best attribution an old log (no `CallUsage`) can give.
        for t in turns(events) {
            if let Some(usage) = &t.usage {
                let entry = per_model.entry(t.model.clone()).or_default();
                sum_usage(&mut entry.0, usage);
                entry.1 += 1;
            }
        }
    }

    per_model
        .into_iter()
        .map(|(model, (usage, calls))| {
            let cost = pricing.cost(&usage, &model);
            ModelCost {
                model,
                usage,
                calls,
                cost,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::EventContext;
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
            context: EventContext::default(),
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
                    fingerprint: None,
                    plan_text: None,
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
                    fingerprint: Some("abc123".into()),
                    plan_text: Some("$x = read(\"a\")".into()),
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

    fn usage_with(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        }
    }

    /// C-06 attribution: a turn that switches model mid-flight (`/model`) must attribute each
    /// `CallUsage` to the model that was ACTIVE for that call, not to the turn's `TurnStarted` model
    /// nor to whichever model is active by the time the fold finishes. Fixture: one turn started on
    /// `model-a`, a `CallUsage` under `model-a`, then a `ModelChanged` to `model-b` mid-turn, then a
    /// second `CallUsage` under `model-b` before the turn ends.
    #[test]
    fn usage_attributed_per_model_after_switch() {
        let events = vec![
            ev(
                1,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "do the thing".into(),
                    model: "model-a".into(),
                },
            ),
            ev(
                2,
                1,
                Some(1),
                EventKind::CallUsage {
                    model: "model-a".into(),
                    usage: usage_with(100, 10),
                },
            ),
            // Mid-turn model switch (the REPL `/model` command).
            ev(
                3,
                2,
                None,
                EventKind::ModelChanged {
                    model: "model-b".into(),
                },
            ),
            ev(
                4,
                3,
                Some(1),
                EventKind::CallUsage {
                    model: "model-b".into(),
                    usage: usage_with(50, 5),
                },
            ),
            ev(
                5,
                4,
                Some(1),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 2,
                    answer: "done".into(),
                    usage: Some(usage_with(150, 15)),
                },
            ),
        ];

        let pricing = PricingTable::builtin();
        let summary = cost_summary(&events, &pricing);
        assert_eq!(summary.len(), 2, "two distinct models: {summary:?}");

        let a = summary.iter().find(|m| m.model == "model-a").unwrap();
        assert_eq!(a.usage.input_tokens, 100);
        assert_eq!(a.usage.output_tokens, 10);
        assert_eq!(a.calls, 1);

        let b = summary.iter().find(|m| m.model == "model-b").unwrap();
        assert_eq!(b.usage.input_tokens, 50);
        assert_eq!(b.usage.output_tokens, 5);
        assert_eq!(b.calls, 1);

        // Neither model's slice is double-counted or merged into the other — the switch didn't
        // smear model-a's tokens onto model-b (or vice versa) despite sharing one `TurnStarted`.
        assert_eq!(a.usage.total() + b.usage.total(), 165);
    }

    /// `cost_summary` rolls up multiple turns, multiple models, and cache tiers, folding
    /// `CallUsage` events by model and pricing the total via the built-in table.
    #[test]
    fn cost_summary_rolls_up_session() {
        let events = vec![
            ev(
                1,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "first".into(),
                    model: "claude-sonnet-4-6".into(),
                },
            ),
            ev(
                2,
                1,
                Some(1),
                EventKind::CallUsage {
                    model: "claude-sonnet-4-6".into(),
                    usage: Usage {
                        input_tokens: 1_000_000,
                        output_tokens: 100_000,
                        cache_creation_input_tokens: 200_000,
                        cache_read_input_tokens: 500_000,
                        reasoning_tokens: 0,
                    },
                },
            ),
            ev(
                3,
                2,
                Some(1),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(1_000_000, 100_000)),
                },
            ),
            // A second turn, same model — its usage must be SUMMED with the first, not replaced.
            ev(
                4,
                3,
                None,
                EventKind::TurnStarted {
                    user_input: "second".into(),
                    model: "claude-sonnet-4-6".into(),
                },
            ),
            ev(
                5,
                4,
                Some(4),
                EventKind::CallUsage {
                    model: "claude-sonnet-4-6".into(),
                    usage: Usage {
                        input_tokens: 500_000,
                        output_tokens: 50_000,
                        ..Default::default()
                    },
                },
            ),
            ev(
                6,
                5,
                Some(4),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(500_000, 50_000)),
                },
            ),
            // A third turn on a different model entirely.
            ev(
                7,
                6,
                None,
                EventKind::TurnStarted {
                    user_input: "third".into(),
                    model: "gpt-5.5".into(),
                },
            ),
            ev(
                8,
                7,
                Some(7),
                EventKind::CallUsage {
                    model: "gpt-5.5".into(),
                    usage: usage_with(1_000_000, 1_000_000),
                },
            ),
            ev(
                9,
                8,
                Some(7),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(1_000_000, 1_000_000)),
                },
            ),
        ];

        let pricing = PricingTable::builtin();
        let summary = cost_summary(&events, &pricing);
        assert_eq!(summary.len(), 2, "two models rolled up: {summary:?}");

        // claude-sonnet-4-6: two calls summed field-wise (1.5M input, 150K output, 200K cache
        // write, 500K cache read) — NOT replaced (would lose the first turn's cache tiers).
        let sonnet = summary
            .iter()
            .find(|m| m.model == "claude-sonnet-4-6")
            .unwrap();
        assert_eq!(sonnet.calls, 2);
        assert_eq!(sonnet.usage.input_tokens, 1_500_000);
        assert_eq!(sonnet.usage.output_tokens, 150_000);
        assert_eq!(sonnet.usage.cache_creation_input_tokens, 200_000);
        assert_eq!(sonnet.usage.cache_read_input_tokens, 500_000);
        // cost = 1.5·3 + 0.15·15 + 0.2·3.75 + 0.5·0.30 = 4.5 + 2.25 + 0.75 + 0.15 = 7.65
        let sonnet_cost = sonnet.cost.expect("claude-sonnet-4-6 is a priced model");
        assert!(
            (sonnet_cost.usd - 7.65).abs() < 1e-9,
            "got {}",
            sonnet_cost.usd
        );
        assert!(
            !sonnet_cost.subscription,
            "bare model id is not a subscription spec"
        );

        let gpt = summary.iter().find(|m| m.model == "gpt-5.5").unwrap();
        assert_eq!(gpt.calls, 1);
        assert_eq!(gpt.usage.input_tokens, 1_000_000);
        assert_eq!(gpt.usage.output_tokens, 1_000_000);
        // cost = 1·1.25 + 1·10 = 11.25
        let gpt_cost = gpt.cost.expect("gpt-5.5 is a priced model");
        assert!((gpt_cost.usd - 11.25).abs() < 1e-9, "got {}", gpt_cost.usd);
    }

    /// Old logs recorded before per-call attribution existed carry no `CallUsage` events at all —
    /// only `TurnStarted.model` + `TurnEnded.usage`. `cost_summary` must still roll them up (via the
    /// turn-level fallback), attributing each turn's total to that turn's `TurnStarted` model, rather
    /// than silently reporting zero spend for every session ever recorded before this feature shipped.
    #[test]
    fn cost_summary_falls_back_to_turn_totals_for_old_logs_without_call_usage() {
        let events = vec![
            ev(
                1,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "hi".into(),
                    model: "claude-opus-4-8".into(),
                },
            ),
            // No CallUsage event — an old log written before C-06.
            ev(
                2,
                1,
                Some(1),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(1_000_000, 1_000_000)),
                },
            ),
        ];

        let pricing = PricingTable::builtin();
        let summary = cost_summary(&events, &pricing);
        assert_eq!(summary.len(), 1);
        let row = &summary[0];
        assert_eq!(row.model, "claude-opus-4-8");
        assert_eq!(row.usage.input_tokens, 1_000_000);
        assert_eq!(row.usage.output_tokens, 1_000_000);
        assert_eq!(row.calls, 1, "one turn folded via the fallback");
        // opus: 1·5 + 1·25 = 30
        let cost = row.cost.expect("claude-opus-4-8 is a priced model");
        assert!((cost.usd - 30.0).abs() < 1e-9, "got {}", cost.usd);
    }
}

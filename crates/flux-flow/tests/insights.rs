use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_core::{Chunk, Usage};
use flux_events::{EventContext, EventStore, NewEvent};
use flux_evidence::{Observation, Phase};
use flux_flow::insights::{collect_facts, narrate, InsightScope, INSIGHT_PACKET_MAX_BYTES};
use flux_provider::{ChunkStream, Provider, Request};
use flux_secret::Redactor;
use futures::stream;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct CaptureProvider {
    requests: Arc<Mutex<Vec<Request>>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    fn name(&self) -> &str {
        "capture"
    }

    async fn stream(&self, request: Request) -> flux_core::Result<ChunkStream> {
        self.requests.lock().unwrap().push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(Chunk::TextDelta("Grounded summary".into())),
            Ok(Chunk::Usage(Usage {
                input_tokens: 10,
                output_tokens: 2,
                ..Usage::default()
            })),
        ])))
    }
}

fn recorded_session() -> (EventStore, String) {
    let events = EventStore::ephemeral();
    let session = events.create_session("capture/model").unwrap();
    let turn = events
        .begin_turn(&session, "Fix the parser", "capture/model")
        .unwrap();
    events
        .record_observation(
            &session,
            turn,
            &Observation::new(
                "tool_call",
                Phase::Turn,
                json!({"tool": "read", "subjects": ["src/parser.rs"]}),
            ),
        )
        .unwrap();
    events
        .record_observation(
            &session,
            turn,
            &Observation::new(
                "tool_call",
                Phase::Turn,
                json!({"tool": "detect_intent", "subjects": []}),
            ),
        )
        .unwrap();
    events
        .record_call_usage(
            &session,
            turn,
            "capture/model",
            Usage {
                input_tokens: 20,
                output_tokens: 5,
                ..Usage::default()
            },
        )
        .unwrap();
    events
        .end_turn(&session, turn, "completed", 2, "Parser fixed", None)
        .unwrap();
    (events, session)
}

#[test]
fn facts_are_derived_from_the_selected_session() {
    let (events, session) = recorded_session();
    let facts = collect_facts(
        &events,
        &InsightScope::Session {
            root: session,
            label: "current session".into(),
        },
        &flux_core::PricingTable::builtin(),
        &Redactor::new(),
    )
    .unwrap();

    assert_eq!(facts.root_sessions, 1);
    assert_eq!(facts.turns, 1);
    assert_eq!(facts.iterations, 2);
    assert_eq!(facts.model_calls, 1);
    assert_eq!(facts.tool_calls, 1);
    assert_eq!(facts.operations[0].name, "read");
    assert!(facts.render().contains("src/parser.rs"));
    assert!(facts.packet().contains("Fix the parser"));
}

#[test]
fn an_empty_interval_is_a_programmatic_no_call_result() {
    let (events, _) = recorded_session();
    let facts = collect_facts(
        &events,
        &InsightScope::Interval {
            start_ms: i64::MIN,
            end_ms: 0,
            label: "empty day".into(),
        },
        &flux_core::PricingTable::builtin(),
        &Redactor::new(),
    )
    .unwrap();
    assert!(facts.is_empty());
    assert_eq!(facts.root_sessions, 0);
}

#[test]
fn packet_is_utf8_safe_bounded_and_redacted() {
    let events = EventStore::ephemeral();
    let session = events.create_session("capture/model").unwrap();
    let secret = ["insight", "fixture", "credential"].join("-");
    let input = format!("{secret} {}", "é".repeat(40_000));
    let turn = events
        .begin_turn(&session, &input, "capture/model")
        .unwrap();
    events
        .end_turn(&session, turn, "completed", 1, "done", None)
        .unwrap();
    let redactor = Redactor::new();
    redactor.add_secret(secret.clone());

    let facts = collect_facts(
        &events,
        &InsightScope::Session {
            root: session,
            label: "current session".into(),
        },
        &flux_core::PricingTable::builtin(),
        &redactor,
    )
    .unwrap();
    let packet = facts.packet();

    assert!(packet.len() <= INSIGHT_PACKET_MAX_BYTES);
    assert!(!packet.contains(&secret));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&packet).unwrap()["omitted_turns"],
        1
    );
}

#[test]
fn correlated_child_detail_is_included_without_counting_child_usage_twice() {
    let events = EventStore::ephemeral();
    let parent = events.create_session("parent-model").unwrap();
    let parent_turn = events
        .begin_turn(&parent, "delegate", "parent-model")
        .unwrap();
    events
        .record_call_usage(
            &parent,
            parent_turn,
            "child-model",
            Usage {
                input_tokens: 50,
                ..Usage::default()
            },
        )
        .unwrap();
    events
        .end_turn(&parent, parent_turn, "ok", 1, "delegated", None)
        .unwrap();

    let child = events
        .create_session_with_context(
            "child-model",
            &EventContext {
                agent_id: Some("subagent:scout".into()),
                correlation_id: Some(parent.clone()),
                ..EventContext::default()
            },
        )
        .unwrap();
    let child_turn = events.begin_turn(&child, "inspect", "child-model").unwrap();
    events
        .record_observation(
            &child,
            child_turn,
            &Observation::new(
                "tool_call",
                Phase::Turn,
                json!({"tool": "read", "subjects": ["src/lib.rs"]}),
            ),
        )
        .unwrap();
    events
        .record_call_usage(
            &child,
            child_turn,
            "child-model",
            Usage {
                input_tokens: 50,
                ..Usage::default()
            },
        )
        .unwrap();
    events
        .end_turn(&child, child_turn, "ok", 1, "found it", None)
        .unwrap();

    let facts = collect_facts(
        &events,
        &InsightScope::Session {
            root: parent,
            label: "current session".into(),
        },
        &flux_core::PricingTable::builtin(),
        &Redactor::new(),
    )
    .unwrap();

    assert_eq!(facts.root_sessions, 1);
    assert_eq!(facts.delegated_sessions, 1);
    assert_eq!(facts.turns, 2);
    assert_eq!(facts.delegated_turns, 1);
    assert_eq!(facts.model_calls, 1);
    assert_eq!(facts.usage.input_tokens, 50);
    assert_eq!(facts.tool_calls, 1);
    assert!(facts.turn_details.iter().any(|turn| turn.delegated));
}

#[test]
fn compaction_does_not_hide_durable_turn_facts() {
    let events = EventStore::ephemeral();
    let session = events.create_session("capture/model").unwrap();
    for input in ["before compaction", "after compaction"] {
        let turn = events.begin_turn(&session, input, "capture/model").unwrap();
        events
            .end_turn(&session, turn, "ok", 1, "done", None)
            .unwrap();
        if input == "before compaction" {
            events
                .append(
                    &session,
                    NewEvent::compacted(vec![flux_core::Message::user_text("summary")]),
                )
                .unwrap();
        }
    }

    let facts = collect_facts(
        &events,
        &InsightScope::Session {
            root: session,
            label: "current session".into(),
        },
        &flux_core::PricingTable::builtin(),
        &Redactor::new(),
    )
    .unwrap();

    assert_eq!(facts.turns, 2);
    assert_eq!(facts.iterations, 2);
    assert!(facts
        .turn_details
        .iter()
        .any(|turn| turn.user_input == "before compaction"));
}

#[tokio::test]
async fn narration_is_exactly_one_tool_free_grounded_request() {
    let (events, session) = recorded_session();
    let facts = collect_facts(
        &events,
        &InsightScope::Session {
            root: session,
            label: "current session".into(),
        },
        &flux_core::PricingTable::builtin(),
        &Redactor::new(),
    )
    .unwrap();
    let provider = CaptureProvider::default();

    let (summary, usage) = narrate(
        &provider,
        "model",
        &facts,
        Some("focus on blockers"),
        &Redactor::new(),
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(summary.unwrap(), "Grounded summary");
    assert_eq!(usage.input_tokens, 10);
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.is_empty());
    assert!(!requests[0].thinking);
    assert_eq!(requests[0].max_tokens, 1024);
    assert!(requests[0].messages[0].text().contains("focus on blockers"));
}

#[test]
fn unscoped_insight_usage_does_not_replace_legacy_turn_totals() {
    let events = EventStore::ephemeral();
    let session = events.create_session("legacy-model").unwrap();
    let turn = events
        .begin_turn(&session, "old turn", "legacy-model")
        .unwrap();
    events
        .end_turn(
            &session,
            turn,
            "completed",
            1,
            "done",
            Some(Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..Usage::default()
            }),
        )
        .unwrap();
    events
        .record_unscoped_call_usage(
            &session,
            "summary-model",
            Usage {
                input_tokens: 10,
                output_tokens: 2,
                ..Usage::default()
            },
        )
        .unwrap();

    let rows = events
        .cost_summary(&session, &flux_core::PricingTable::builtin())
        .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.usage.input_tokens).sum::<u64>(),
        110
    );
    assert!(events.conversation(&session).unwrap().is_empty());
}

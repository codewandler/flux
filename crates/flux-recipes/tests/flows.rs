//! Integration tests for the recipe builders: each one is built, `analyze`d, and `execute`d through a
//! real `FlowClient` with mocked adapter ops (registered stub `Tool`s) and a never-called provider —
//! hermetic, no API key. These assert the recipes wire to the runtime semantics they document.

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_recipes::dsl::*;
use flux_recipes::{batch, lookup, routing};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_sdk::{ExecutionResult, FlowClient};
use flux_spec::ToolSpec;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Test harness: a never-called provider + a generic deterministic mock op.
// ---------------------------------------------------------------------------

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &str {
        "unused"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// A deterministic stub op: one string input parameter, a pure `&str -> String` response.
struct MockOp {
    name: &'static str,
    param: &'static str,
    respond: fn(&str) -> String,
}

#[async_trait]
impl Tool for MockOp {
    fn spec(&self) -> ToolSpec {
        let mut props = serde_json::Map::new();
        props.insert(self.param.to_string(), json!({ "type": "string" }));
        ToolSpec::read_only(
            self.name,
            "mock adapter (deterministic)",
            json!({ "type": "object", "properties": props, "required": [self.param] }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let arg = params
            .get(self.param)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(ToolResult::ok((self.respond)(arg)))
    }
}

fn op(name: &'static str, param: &'static str, respond: fn(&str) -> String) -> Arc<dyn Tool> {
    Arc::new(MockOp {
        name,
        param,
        respond,
    })
}

fn make_client(ops: Vec<Arc<dyn Tool>>) -> FlowClient {
    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(StubProvider), ".")
        .expect("build FlowClient");
    for o in ops {
        client.register_op(o);
    }
    client
}

async fn run(client: &FlowClient, flow: &DraftAst) -> ExecutionResult {
    if let Err(d) = client.analyze(flow) {
        panic!("analyze failed: {d:?}");
    }
    client.execute(flow).await.expect("execute")
}

// ---------------------------------------------------------------------------
// routing::route_intent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_intent_dispatches_matched_label_and_falls_back() {
    let client = make_client(vec![
        op("intent.classify", "utterance", |u| {
            if u.contains("book") {
                "book".to_string()
            } else {
                "other".to_string()
            }
        }),
        op("booking.create", "utterance", |_| "created".to_string()),
        op("support.ticket", "utterance", |_| "ticketed".to_string()),
    ]);

    // A matching label routes to its handler.
    let hit = routing::route_intent(
        "intent.classify",
        lit("please book a flight"),
        &[("book", "booking.create")],
        "support.ticket",
    );
    let out = run(&client, &hit).await;
    assert_eq!(out.tool_calls, vec!["intent.classify", "booking.create"]);
    assert_eq!(out.result, "created");

    // An unmatched label falls through to the default handler.
    let miss = routing::route_intent(
        "intent.classify",
        lit("what are your opening hours?"),
        &[("book", "booking.create")],
        "support.ticket",
    );
    let out = run(&client, &miss).await;
    assert_eq!(out.tool_calls, vec!["intent.classify", "support.ticket"]);
    assert_eq!(out.result, "ticketed");
}

// ---------------------------------------------------------------------------
// lookup::answer_with_fallback
// ---------------------------------------------------------------------------

fn synth(retrieved: &str) -> String {
    let answer = if retrieved.starts_with("ESCALATE") {
        json!({
            "status": "unanswered",
            "summary": "No entry matched; escalated to a human.",
            "gaps": ["question not covered by the knowledge base"],
        })
    } else {
        json!({ "status": "answered", "summary": retrieved })
    };
    serde_json::to_string(&answer).expect("Answer serializes")
}

#[tokio::test]
async fn answer_with_fallback_hits_then_degrades() {
    let client = make_client(vec![
        op("kb.search", "question", |q| {
            if q.contains("password") {
                "Reset it under Account → Security.".to_string()
            } else {
                String::new() // a miss → fallback passes over this branch
            }
        }),
        op("kb.escalate", "question", |_| {
            "ESCALATE: routed to a human agent.".to_string()
        }),
        op("faq.synth", "retrieved", synth),
    ]);

    // Hit: the KB branch wins; the synth op produces an `answered` Answer.
    let hit = lookup::answer_with_fallback(
        "kb.search",
        "kb.escalate",
        "faq.synth",
        lit("How do I reset my password?"),
    );
    let out = run(&client, &hit).await;
    assert_eq!(out.tool_calls, vec!["kb.search", "faq.synth"]);
    let answer = out.answer().expect("result parses as an Answer");
    assert_eq!(answer.status, "answered");
    assert_eq!(answer.summary, "Reset it under Account → Security.");
    assert!(answer.gaps.is_empty());

    // Miss: kb.search returns empty → fallback degrades to kb.escalate → an `unanswered` Answer + gap.
    let miss = lookup::answer_with_fallback(
        "kb.search",
        "kb.escalate",
        "faq.synth",
        lit("Do you ship to Antarctica?"),
    );
    let out = run(&client, &miss).await;
    assert_eq!(
        out.tool_calls,
        vec!["kb.search", "kb.escalate", "faq.synth"]
    );
    let answer = out.answer().expect("result parses as an Answer");
    assert_eq!(answer.status, "unanswered");
    assert!(!answer.gaps.is_empty());
}

// ---------------------------------------------------------------------------
// batch::map_each
// ---------------------------------------------------------------------------

#[tokio::test]
async fn map_each_collects_per_item_results() {
    let client = make_client(vec![op("upper", "x", |x| x.to_uppercase())]);

    let flow = batch::map_each("x", lit(json!(["a", "b", "c"])), "upper", "out");
    let out = run(&client, &flow).await;

    // One op call per element, in order.
    assert_eq!(out.tool_calls, vec!["upper", "upper", "upper"]);
    // The collected list is the result; every transformed element is present.
    for expected in ["A", "B", "C"] {
        assert!(
            out.result.contains(expected),
            "result {:?} missing {expected}",
            out.result
        );
    }
}

// ---------------------------------------------------------------------------
// batch::repeat_until
// ---------------------------------------------------------------------------

#[tokio::test]
async fn repeat_until_stops_early_when_condition_holds() {
    let client = make_client(vec![op("tick", "input", |_| "ready".to_string())]);

    // `until = var("r")`: a truthy result after the first iteration stops the loop immediately.
    let flow = batch::repeat_until(5, "tick", lit("go"), "r", var("r"));
    let out = run(&client, &flow).await;

    assert_eq!(out.tool_calls, vec!["tick"], "should run exactly once");
    assert_eq!(out.result, "ready");
}

#[tokio::test]
async fn repeat_until_runs_to_max_when_condition_never_holds() {
    // A falsey ("") result keeps `until = var("r")` false, so the loop exhausts its bound.
    let client = make_client(vec![op("tick", "input", |_| String::new())]);

    let flow = batch::repeat_until(4, "tick", lit("go"), "r", var("r"));
    let out = run(&client, &flow).await;

    assert_eq!(out.tool_calls.len(), 4, "should run the full bound");
    assert!(out.tool_calls.iter().all(|c| c == "tick"));
}

// ---------------------------------------------------------------------------
// batch::poll_for
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_for_runs_within_the_time_budget() {
    let client = make_client(vec![op("ping", "input", |_| "pong".to_string())]);

    // Small budget keeps the test fast; the body runs at least once.
    let flow = batch::poll_for(20, 5, "ping", lit("x"));
    let out = run(&client, &flow).await;

    assert!(
        !out.tool_calls.is_empty(),
        "the loop body runs at least once"
    );
    assert!(out.tool_calls.iter().all(|c| c == "ping"));
    assert_eq!(out.result, "pong", "result is the last attempt's output");
}

// ---------------------------------------------------------------------------
// batch::race_first
// ---------------------------------------------------------------------------

#[tokio::test]
async fn race_first_returns_a_winning_branch() {
    // Both branches return the same value so the assertion is order-independent.
    let client = make_client(vec![
        op("fast", "input", |_| "WON".to_string()),
        op("slow", "input", |_| "WON".to_string()),
    ]);

    let flow = batch::race_first(50, &["fast", "slow"], lit("x"), "winner");
    let out = run(&client, &flow).await;

    assert_eq!(out.result, "WON");
}

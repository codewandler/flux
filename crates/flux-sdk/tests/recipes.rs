//! Integration tests for the recipe builders: each one is built, `analyze`d, and `execute`d through a
//! real `FlowClient` with mocked adapter ops (registered stub `Tool`s) and a never-called provider —
//! hermetic, no API key. These assert the recipes wire to the runtime semantics they document.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_sdk::dsl::*;
use flux_sdk::recipes::{batch, compose, dispatch, fanout, lookup, resilience, routing};
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

/// Analyze then execute, surfacing an error from either stage — for failure-path tests.
async fn try_run(client: &FlowClient, flow: &DraftAst) -> Result<ExecutionResult> {
    client
        .analyze(flow)
        .map_err(|d| flux_core::Error::Other(format!("analyze: {d:?}")))?;
    client.execute(flow).await
}

/// A stateful op that fails its first `fails_before` calls (erroring tool result), then succeeds with
/// `ok`. `fails_before = usize::MAX` always fails. Drives `retry`/`try`.
struct FlakyOp {
    name: &'static str,
    param: &'static str,
    fails_before: usize,
    calls: AtomicUsize,
    ok: &'static str,
}

#[async_trait]
impl Tool for FlakyOp {
    fn spec(&self) -> ToolSpec {
        let mut props = serde_json::Map::new();
        props.insert(self.param.to_string(), json!({ "type": "string" }));
        ToolSpec::read_only(
            self.name,
            "flaky mock adapter",
            json!({ "type": "object", "properties": props, "required": [self.param] }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fails_before {
            Ok(ToolResult::error(format!(
                "transient failure (attempt {n})"
            )))
        } else {
            Ok(ToolResult::ok(self.ok))
        }
    }
}

fn flaky(
    name: &'static str,
    param: &'static str,
    fails_before: usize,
    ok: &'static str,
) -> Arc<dyn Tool> {
    Arc::new(FlakyOp {
        name,
        param,
        fails_before,
        calls: AtomicUsize::new(0),
        ok,
    })
}

/// An op that sleeps `sleep_ms` before succeeding — drives a real `timeout` deadline.
struct SlowOp {
    name: &'static str,
    param: &'static str,
    sleep_ms: u64,
    out: &'static str,
}

#[async_trait]
impl Tool for SlowOp {
    fn spec(&self) -> ToolSpec {
        let mut props = serde_json::Map::new();
        props.insert(self.param.to_string(), json!({ "type": "string" }));
        ToolSpec::read_only(
            self.name,
            "slow mock adapter",
            json!({ "type": "object", "properties": props, "required": [self.param] }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
        Ok(ToolResult::ok(self.out))
    }
}

fn slow(
    name: &'static str,
    param: &'static str,
    sleep_ms: u64,
    out: &'static str,
) -> Arc<dyn Tool> {
    Arc::new(SlowOp {
        name,
        param,
        sleep_ms,
        out,
    })
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

// ---------------------------------------------------------------------------
// resilience::retry_with_backoff
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_succeeds_on_first_attempt() {
    let client = make_client(vec![flaky("work", "input", 0, "done")]);
    let flow = resilience::retry_with_backoff(3, "none", 0, "work", lit("x"), "out");
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls, vec!["work"]);
    assert_eq!(out.result, "done");
}

#[tokio::test]
async fn retry_recovers_after_transient_failures() {
    // Fails twice, then succeeds → three attempts in total.
    let client = make_client(vec![flaky("work", "input", 2, "done")]);
    let flow = resilience::retry_with_backoff(5, "none", 0, "work", lit("x"), "out");
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls.len(), 3);
    assert_eq!(out.result, "done");
}

#[tokio::test]
async fn retry_exhausts_and_errors() {
    let client = make_client(vec![flaky("work", "input", usize::MAX, "done")]);
    let flow = resilience::retry_with_backoff(2, "none", 0, "work", lit("x"), "out");
    assert!(
        try_run(&client, &flow).await.is_err(),
        "retry should exhaust"
    );
}

// ---------------------------------------------------------------------------
// resilience::with_timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn with_timeout_completes_within_budget() {
    let client = make_client(vec![op("quick", "input", |_| "ok".to_string())]);
    let flow = resilience::with_timeout(1_000, "quick", lit("x"), "out");
    let out = run(&client, &flow).await;
    assert_eq!(out.result, "ok");
}

#[tokio::test]
async fn with_timeout_fires_on_a_slow_op() {
    let client = make_client(vec![slow("slow_op", "input", 50, "late")]);
    let flow = resilience::with_timeout(5, "slow_op", lit("x"), "out");
    assert!(
        try_run(&client, &flow).await.is_err(),
        "timeout should fire"
    );
}

// ---------------------------------------------------------------------------
// resilience::with_budget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn with_budget_allows_within_limit() {
    let client = make_client(vec![op("step", "input", |_| "done".to_string())]);
    let flow = resilience::with_budget(1, "step", lit("x"), "out");
    let out = run(&client, &flow).await;
    assert_eq!(out.result, "done");
}

#[tokio::test]
async fn with_budget_zero_does_not_succeed() {
    let client = make_client(vec![op("step", "input", |_| "done".to_string())]);
    let flow = resilience::with_budget(0, "step", lit("x"), "out");
    assert!(
        try_run(&client, &flow).await.is_err(),
        "budget 0 must not succeed"
    );
}

// ---------------------------------------------------------------------------
// resilience::try_catch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn try_catch_passes_through_on_success() {
    let client = make_client(vec![
        op("body", "input", |_| "ok".to_string()),
        op("handle", "err", |_| "handled".to_string()),
    ]);
    let flow = resilience::try_catch("body", lit("x"), "err", "handle");
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls, vec!["body"]);
    assert_eq!(out.result, "ok");
}

#[tokio::test]
async fn try_catch_recovers_on_failure() {
    let client = make_client(vec![
        flaky("body", "input", usize::MAX, "unused"), // always errors
        op("handle", "err", |e| format!("handled: {e}")),
    ]);
    let flow = resilience::try_catch("body", lit("x"), "err", "handle");
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls, vec!["body", "handle"]);
    assert!(
        out.result.starts_with("handled:"),
        "handler saw the error: {}",
        out.result
    );
}

// ---------------------------------------------------------------------------
// fanout::parallel_all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn parallel_all_runs_every_branch() {
    let client = make_client(vec![
        op("a", "input", |_| "ra".to_string()),
        op("b", "input", |_| "rb".to_string()),
        op("c", "input", |_| "rc".to_string()),
    ]);
    let flow = fanout::parallel_all(&["a", "b", "c"], lit("x"));
    let out = run(&client, &flow).await;
    // Output replays in branch order, so this is deterministic.
    assert_eq!(out.tool_calls, vec!["a", "b", "c"]);
    assert_eq!(out.result, "rc"); // the last branch's value
}

// ---------------------------------------------------------------------------
// dispatch::match_value
// ---------------------------------------------------------------------------

#[tokio::test]
async fn match_value_dispatches_on_the_bound_subject() {
    fn status(i: &str) -> String {
        if i.contains("paid") {
            "paid".to_string()
        } else {
            "open".to_string()
        }
    }
    let make = || {
        make_client(vec![
            op("status", "input", status),
            op("ship", "input", |_| "shipped".to_string()),
            op("hold", "input", |_| "on hold".to_string()),
        ])
    };

    // A matching subject routes to its handler.
    let client = make();
    let flow = dispatch::match_value("status", lit("order is paid"), &[("paid", "ship")], "hold");
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls, vec!["status", "ship"]);
    assert_eq!(out.result, "shipped");

    // No matching case falls through to the default.
    let client = make();
    let flow = dispatch::match_value("status", lit("order is open"), &[("paid", "ship")], "hold");
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls, vec!["status", "hold"]);
    assert_eq!(out.result, "on hold");
}

// ---------------------------------------------------------------------------
// compose::resilient_call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resilient_call_prefers_primary_then_degrades() {
    fn primary(i: &str) -> String {
        if i.contains("ok") {
            "primary-result".to_string()
        } else {
            String::new() // empty → fallback degrades to backup
        }
    }
    let make = || {
        make_client(vec![
            op("primary", "input", primary),
            op("backup", "input", |_| "backup-result".to_string()),
        ])
    };

    // Primary returns non-empty → fallback wins on primary, retry succeeds first attempt.
    let client = make();
    let flow = compose::resilient_call(
        3,
        "none",
        0,
        1_000,
        "primary",
        "backup",
        lit("ok please"),
        "out",
    );
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls, vec!["primary"]);
    assert_eq!(out.result, "primary-result");

    // Primary empty → fallback degrades to backup.
    let client = make();
    let flow =
        compose::resilient_call(3, "none", 0, 1_000, "primary", "backup", lit("nope"), "out");
    let out = run(&client, &flow).await;
    assert_eq!(out.tool_calls, vec!["primary", "backup"]);
    assert_eq!(out.result, "backup-result");
}

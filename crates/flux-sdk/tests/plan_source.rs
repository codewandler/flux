//! L-38 integration: an ACCEPTED plan is durably recorded on its `PlanAttempted` event with
//! `plan_source` — the CANONICAL parseable Flux-Lang projection (`flux_lang::format::format`) —
//! alongside the display-only `plan_text` (`render_pretty`), and it round-trips through
//! `flux_lang::parse` back to exactly the accepted AST (the L-18 totality invariant pinned at the
//! event boundary). Oversized sources are dropped (`None`), NEVER truncated — a present
//! `plan_source` always parses (a truncation suffix would poison downstream corpus mining).
//!
//! Added RED first (the field decoded but nothing populated it), made GREEN by the L-38 wiring in
//! `crates/flux-flow/src/loop_host.rs`. Hermetic — mock provider, in-memory stores, no API key.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_agent::AgentSpec;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_flow::AgentSink;
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::{AllowApprover, Approver, ToolContext, ToolRegistry};
use flux_system::{System, Workspace};
use serde_json::json;

/// Plays back scripted per-call chunk sequences; any call past the script answers in prose (the
/// terminal chat round), so a loop-shape change can't hang the test.
struct ScriptedProvider(Mutex<VecDeque<Vec<Chunk>>>);

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let chunks = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| prose("done"));
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// One model turn that emits an `emit_plan` tool call carrying `ast` (the engine tests' helper
/// shape, `crates/flux-flow/src/engine.rs`).
fn emit_plan(ast: serde_json::Value) -> Vec<Chunk> {
    vec![
        Chunk::Block(ContentBlock::ToolUse {
            id: "p1".into(),
            name: "emit_plan".into(),
            input: json!({ "ast": ast }),
        }),
        Chunk::Done {
            stop_reason: Some(StopReason::ToolUse),
        },
    ]
}

/// One model turn that answers in prose (the chat round that ends the turn).
fn prose(text: &str) -> Vec<Chunk> {
    vec![
        Chunk::TextDelta(text.to_string()),
        Chunk::Done {
            stop_reason: Some(StopReason::EndTurn),
        },
    ]
}

#[derive(Default)]
struct NullSink;
impl AgentSink for NullSink {
    fn text_delta(&mut self, _t: &str) {}
    fn tool_call(&mut self, _name: &str, _input: &serde_json::Value) {}
}

/// Assemble a real engine (the `ClientBuilder::build` recipe, `crates/flux-sdk/src/lib.rs`) around
/// the scripted provider and a caller-owned event store, so the test can read `turns()` back.
fn engine_with(name: &str, responses: VecDeque<Vec<Chunk>>, events: Arc<EventStore>) -> FlowEngine {
    let root = std::env::temp_dir().join(format!("flux-l38-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let system = Arc::new(System::new(Workspace::new(root.clone()).unwrap()));
    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    let approver: Arc<dyn Approver> = Arc::new(AllowApprover);
    let spec = AgentSpec {
        model: "mock".into(),
        cwd: root,
        ..AgentSpec::default()
    };
    spec.assemble(
        Arc::new(ScriptedProvider(Mutex::new(responses))),
        registry,
        approver,
        ToolContext::new(system),
        events,
        FlowStore::in_memory().unwrap(),
    )
    .unwrap()
}

/// The accepted attempt carries a `plan_source` that parses back to exactly the accepted AST;
/// non-accepted attempts (the terminal chat round) carry `None`.
#[tokio::test]
async fn accepted_plan_records_canonical_parseable_plan_source() {
    let store = Arc::new(EventStore::in_memory().unwrap());
    let sid = store.create_session("mock").unwrap();
    // `merge` is in the always-on cognition group, so the plan passes the surfacing gate (A-04).
    let plan_ast = json!({
        "body": [
            {
                "kind": "bind", "name": "claims",
                "value": { "kind": "call", "op": "merge",
                           "args": [{ "kind": "lit", "value": { "lists": [[1], [2]] } }] }
            },
            { "kind": "return", "value": { "kind": "var", "name": "claims" } }
        ]
    });
    let responses = VecDeque::from(vec![emit_plan(plan_ast.clone()), prose("done")]);
    let engine = engine_with("roundtrip", responses, store.clone());
    engine
        .run_turn(&sid, "merge the claims", &mut NullSink)
        .await
        .unwrap();

    let attempts: Vec<flux_events::PlanAttempt> = store
        .turns(&sid)
        .unwrap()
        .into_iter()
        .flat_map(|t| t.plan_attempts)
        .collect();
    let accepted: Vec<&flux_events::PlanAttempt> = attempts
        .iter()
        .filter(|a| a.outcome == "accepted")
        .collect();
    assert_eq!(accepted.len(), 1, "attempts: {attempts:?}");
    let source = accepted[0]
        .plan_source
        .as_deref()
        .expect("the accepted attempt carries plan_source");
    let parsed = flux_lang::parse::parse(source).expect("plan_source parses");
    let expected: flux_lang::ast::DraftAst = serde_json::from_value(plan_ast).unwrap();
    assert_eq!(
        parsed, expected,
        "parse(plan_source) == the accepted AST (L-18 roundtrip at the event boundary)"
    );
    assert!(
        accepted[0].plan_text.is_some(),
        "the display-only plan_text is still recorded alongside"
    );
    assert!(
        attempts
            .iter()
            .filter(|a| a.outcome != "accepted")
            .all(|a| a.plan_source.is_none()),
        "non-accepted attempts carry no plan_source: {attempts:?}"
    );
}

/// `plan_source` is None-on-overflow: a plan whose canonical text exceeds the cap is dropped, not
/// truncated, while the independently-capped `plan_text` stays present (truncated as today).
#[tokio::test]
async fn oversized_plan_source_is_dropped_not_truncated() {
    let store = Arc::new(EventStore::in_memory().unwrap());
    let sid = store.create_session("mock").unwrap();
    // A pure value-bind plan (no ops — nothing to surface or approve) whose one string literal
    // pushes the canonical text past PLAN_SOURCE_CAP (32k).
    let big = "x".repeat(40_000);
    let plan_ast = json!({
        "body": [
            { "kind": "bind", "name": "blob", "value": { "kind": "lit", "value": big } },
            { "kind": "return", "value": { "kind": "var", "name": "blob" } }
        ]
    });
    let responses = VecDeque::from(vec![emit_plan(plan_ast), prose("done")]);
    let engine = engine_with("oversized", responses, store.clone());
    engine
        .run_turn(&sid, "bind the blob", &mut NullSink)
        .await
        .unwrap();

    let accepted: Vec<flux_events::PlanAttempt> = store
        .turns(&sid)
        .unwrap()
        .into_iter()
        .flat_map(|t| t.plan_attempts)
        .filter(|a| a.outcome == "accepted")
        .collect();
    assert_eq!(accepted.len(), 1, "accepted attempts: {accepted:?}");
    assert_eq!(
        accepted[0].plan_source, None,
        "an over-cap plan_source is dropped, not truncated"
    );
    assert!(
        accepted[0].plan_text.is_some(),
        "the display-only plan_text stays present (its own cap truncates)"
    );
}

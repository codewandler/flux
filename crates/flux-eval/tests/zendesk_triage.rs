//! Offline execution proof for the shipped Zendesk workflow (A-136).
//!
//! The exact checked-in module is selected, lowered, input-bound, and executed for all four
//! entrypoints. Static tools stand in at the operation boundary for Zendesk and cognition; no
//! credential, provider, plugin process, or network is involved. A second run makes cognition fail
//! and proves the authored fallback returns the already-gathered ticket evidence.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_flow::AgentSink;
use flux_runtime::{
    AllowApprover, Executor, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::{System, Workspace};
use serde_json::{json, Value};

#[derive(Default)]
struct NullSink;
impl AgentSink for NullSink {}

struct StaticOperation {
    name: &'static str,
    response: Value,
    fail: bool,
    model: bool,
}

#[async_trait]
impl Tool for StaticOperation {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: format!("Offline fixture for {}.", self.name),
            input_schema: json!({"type":"object"}),
            output_schema: None,
            effects: if self.model {
                vec![Effect::Network]
            } else {
                vec![Effect::Read, Effect::Network]
            },
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: if self.model {
                vec![AccessKind::Provider]
            } else {
                vec![AccessKind::Network]
            },
            group: None,
        }
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        if self.fail {
            Ok(ToolResult::error("offline cognition fixture failed"))
        } else {
            Ok(ToolResult::ok(self.response.to_string()))
        }
    }
}

fn workflow_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/zendesk.triage.flux")
}

fn registry(ai_fails: bool) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for (name, response) in [
        (
            "zendesk.test",
            json!({"status":"ok","user":{"id":7,"name":"Flux Support"}}),
        ),
        (
            "zendesk.ticket.search",
            json!({
                "tickets":[{"id":42,"subject":"Cannot sign in","updated_at":"2026-07-30T12:00:00Z"}],
                "count":1,
                "next_page":null,
                "previous_page":null,
                "contributed":1
            }),
        ),
        (
            "zendesk.ticket.show",
            json!({"ticket":{"id":42,"subject":"Cannot sign in","description":"SSO loops"}}),
        ),
        (
            "zendesk.ticket.comment.list",
            json!({"comments":[{"id":9,"body":"Customer supplied a trace"}],"count":1}),
        ),
    ] {
        registry.register(Arc::new(StaticOperation {
            name,
            response,
            fail: false,
            model: false,
        }));
    }
    registry.register(Arc::new(StaticOperation {
        name: "ai.extract",
        response: json!({"summary":"offline model analysis","ticket_id":42}),
        fail: ai_fails,
        model: true,
    }));
    registry
}

fn selected_flow(entry: &str) -> flux_flow::ast::DraftAst {
    let source = std::fs::read_to_string(workflow_path()).expect("read Zendesk reference workflow");
    let module = flux_flow::program::Module::parse_str(&source).expect("parse Zendesk workflow");
    match module {
        flux_flow::program::Module::Program(program) => program
            .flows
            .into_iter()
            .find(|flow| flow.name.as_deref() == Some(entry))
            .unwrap_or_else(|| panic!("missing `{entry}` entrypoint")),
        flux_flow::program::Module::Flow(_) => panic!("Zendesk reference must remain multi-flow"),
    }
}

fn bind_input(ast: &mut flux_flow::ast::DraftAst, name: &str, value: Value) {
    let mut prefix = vec![flux_flow::ast::Node::Bind {
        name: name.into(),
        value: Box::new(flux_flow::ast::Node::Lit { value }),
        ty: None,
        effect: None,
    }];
    prefix.append(&mut ast.body);
    ast.body = prefix;
}

async fn run_entry(entry: &str, input: Option<(&str, Value)>, ai_fails: bool) -> String {
    let mut ast = selected_flow(entry);
    if let Some((name, value)) = input {
        bind_input(&mut ast, name, value);
    }
    let registry = registry(ai_fails);
    flux_flow::analyze::lower(
        &ast,
        &flux_flow::registry::OpRegistry::new(&registry),
        &Default::default(),
    )
    .unwrap_or_else(|diagnostics| panic!("`{entry}` fails the direct-flow gate: {diagnostics:?}"));

    let executor = Executor::new(
        registry,
        PermissionManager::from_rules(&["*".into()], &[]),
        Arc::new(AllowApprover),
        ToolContext::new(Arc::new(System::new(
            Workspace::new(env!("CARGO_MANIFEST_DIR")).unwrap(),
        ))),
    );
    let store = flux_flow::state::FlowStore::in_memory().unwrap();
    let mut sink = NullSink;
    flux_flow::runtime::execute_flow(&store, &executor, "zendesk-offline", &ast, &mut sink)
        .await
        .unwrap_or_else(|error| panic!("`{entry}` execution failed: {error}"))
        .result
}

#[tokio::test]
async fn every_zendesk_entrypoint_executes_offline() {
    for (entry, input) in [
        ("setup", None),
        ("triage", Some(("query", json!("type:ticket status:new")))),
        ("brief", Some(("ticket_id", json!(42)))),
        ("eod", Some(("query", json!("type:ticket updated>24hours")))),
    ] {
        let result = run_entry(entry, input, false).await;
        assert!(!result.trim().is_empty(), "`{entry}` returned no result");
        assert!(
            result.contains("42") || entry == "setup",
            "{entry}: {result}"
        );
    }
}

#[tokio::test]
async fn cognition_failure_returns_gathered_zendesk_evidence() {
    let result = run_entry(
        "triage",
        Some(("query", json!("type:ticket status:new"))),
        true,
    )
    .await;
    assert!(result.contains("AI analysis unavailable"), "{result}");
    assert!(result.contains("Cannot sign in"), "{result}");
}

//! Parameterized flow execution — the **behaviour-runner seam** (story D-01). A single *stored*,
//! validated Flux-Lang flow is parsed **once** (deterministically, no model round-trip) and then run
//! **per invocation** with different *effective settings* injected as `$variables` — never baked into
//! the AST as literals.
//!
//! This is what a downstream service (e.g. a multi-tenant agent runner) needs: one flow as a reusable
//! behaviour, driven by validated per-call settings. The adapter op is mocked, so it runs with no API
//! key and never touches the network.
//!
//! Run with: `cargo run -p flux-sdk --example parameterized_flow`

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_sdk::FlowClient;
use flux_spec::ToolSpec;
use serde_json::{json, Map, Value};

/// A never-called provider — `parse` is deterministic and the flow dispatches only registered ops.
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

/// The behaviour's one custom op: compose a greeting from two settings and wrap it in a typed `Answer`.
/// Two declared params (`greeting`, `name`) — a behaviour-runner's ops look exactly like this.
struct GreetOp;

#[async_trait]
impl Tool for GreetOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "greet",
            "compose a greeting from settings",
            json!({
                "type": "object",
                "properties": { "greeting": { "type": "string" }, "name": { "type": "string" } },
                "required": ["greeting", "name"],
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let greeting = params
            .get("greeting")
            .and_then(|v| v.as_str())
            .unwrap_or("Hello");
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("there");
        // Return the typed prelude `Answer` shape so `ExecutionResult::answer()` round-trips it.
        let answer = json!({
            "status": "answered",
            "summary": format!("{greeting}, {name}!"),
            "evidence": [], "gaps": [], "risks": [],
        });
        Ok(ToolResult::ok(
            serde_json::to_string(&answer).expect("Answer serializes"),
        ))
    }
}

/// The *stored* behaviour — canonical Flux-Lang text. It references only `$greeting` / `$name`; there
/// is **no literal** anywhere, so every value comes from the per-run settings, not the AST.
const GREETING_FLOW: &str = "\
flow
  $answer = greet($greeting, $name)
  return $answer";

/// One run's effective settings — the validated invocation-time JSON a behaviour runner threads in.
fn settings(greeting: &str, name: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("greeting".to_string(), json!(greeting));
    m.insert("name".to_string(), json!(name));
    m
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(StubProvider), ".")?;
    client.register_op(Arc::new(GreetOp));

    // Parse the stored flow ONCE — deterministic, no provider round-trip.
    let flow = client.parse(GREETING_FLOW)?;
    client
        .analyze(&flow)
        .map_err(|d| Error::Other(format!("analyze: {d:?}")))?;
    // The stored AST bakes in no input value — prove it carries no `lit` node.
    assert!(
        !serde_json::to_string(&flow).unwrap().contains("\"lit\""),
        "the stored behaviour must hold no baked-in inputs"
    );

    // Run the SAME stored flow per invocation, each with different effective settings.
    println!("one stored flow, three invocations:");
    for (greeting, name) in [("Hej", "Timo"), ("Hello", "Ada"), ("Bonjour", "René")] {
        let out = client.execute_with(&flow, settings(greeting, name)).await?;
        match out.answer() {
            Some(a) => println!("  [{}] {}", a.status, a.summary),
            None => println!("  result: {}", out.result),
        }
    }
    Ok(())
}

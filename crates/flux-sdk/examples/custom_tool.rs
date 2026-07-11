//! Custom tools + approval policy (D-143), and the "one import" surface (D-146): a consumer builds
//! a function-tool and an approval policy using only `flux_sdk::` paths — no direct `flux-runtime`
//! or `flux-spec` dependency. Here the mock plans a call to the custom `greet` op, which dispatches
//! through the same safety envelope as every built-in.
//!
//! Run with: `cargo run -p codewandler-flux-sdk --example custom_tool`

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::approval::{ApprovalChoice, Approver, IntentSet};
use flux_sdk::tools::{tool_fn, ToolSpec};
use flux_sdk::Client;

/// Plans a single `greet` call on the first turn, then answers in prose.
struct PlanGreetMock {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for PlanGreetMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        let chunks = if n == 0 {
            let ast = serde_json::json!({
                "body": [{
                    "kind": "call", "op": "greet",
                    "args": [ { "kind": "lit", "value": { "name": "flux" } } ]
                }]
            });
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "p1".into(),
                    name: "emit_plan".into(),
                    input: serde_json::json!({ "ast": ast }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else {
            vec![
                Chunk::Block(ContentBlock::Text {
                    text: "Greeted.".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ]
        };
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// A per-op policy: allow `greet`, deny anything else. The seam a real app plugs its own
/// risk-aware confirmation into.
struct AllowGreetOnly;

#[async_trait]
impl Approver for AllowGreetOnly {
    async fn request(
        &self,
        tool: &str,
        _subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        if tool == "greet" {
            ApprovalChoice::Allow
        } else {
            ApprovalChoice::Deny
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // A function-tool built from a closure — no trait impl needed for the tool itself.
    let greet = tool_fn(
        ToolSpec::read_only(
            "greet",
            "Greets someone by name",
            serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        |input| async move {
            let name = input["name"].as_str().unwrap_or("world");
            Ok(serde_json::json!(format!("Hello, {name}!")))
        },
    );

    let client = Client::builder()
        .model("mock")
        .register_op(greet)
        .approver(std::sync::Arc::new(AllowGreetOnly))
        .build(
            Box::new(PlanGreetMock {
                calls: AtomicUsize::new(0),
            }),
            ".",
        )?;

    let out = client.run("greet flux for me").await?;
    println!("tools invoked: {:?}", out.tool_calls);
    println!("final: {}", out.text);
    Ok(())
}

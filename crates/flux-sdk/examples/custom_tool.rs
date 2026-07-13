//! Custom tools + approval policy (D-143), and the "one import" surface (D-146): a consumer builds
//! a function-tool and an approval policy using only `flux_sdk::` paths — no direct `flux-runtime`
//! or `flux-spec` dependency. Here the mock declares intent, calls the custom `greet` op with its
//! native schema, then presents the result. The call dispatches through the same safety envelope as
//! every built-in; the model never generates Flux code.
//!
//! Run with: `cargo run -p codewandler-flux-sdk --example custom_tool`

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::approval::{ApprovalChoice, Approver, IntentSet};
use flux_sdk::tools::{tool_fn, ToolSpec};
use flux_sdk::Client;

/// Declares intent, calls `greet`, then answers in prose.
struct GreetMock;

#[async_trait]
impl Provider for GreetMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, req: Request) -> Result<ChunkStream> {
        let intent_stage = req.tools.iter().any(|tool| tool.name == "declare_intent");
        let has_result = req.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "greet"
                )
            })
        });
        let chunks = if intent_stage {
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: serde_json::json!({
                        "intent": "greet the requested person",
                        "capability_families": ["core"],
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else if !has_result {
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "greet".into(),
                    name: "greet".into(),
                    input: serde_json::json!({ "name": "flux" }),
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
        .build(Box::new(GreetMock), ".")?;

    let out = client.run("greet flux for me").await?;
    println!("tools invoked: {:?}", out.tool_calls);
    println!("final: {}", out.text);
    Ok(())
}

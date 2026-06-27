//! The NL→AST front door: [`flux_sdk::FlowClient::run`] compiles a natural-language instruction into a
//! Flux-Lang AST (prompt-and-parse), analyzes it, and executes it. Here the mock provider returns a
//! fenced AST instead of calling a real model, so the example runs with no API key — the same
//! `compile → analyze → execute` lifecycle the DSL feeds into at the `analyze` step.
//!
//! Run with: `cargo run -p flux-sdk --example flow_compile`

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Chunk, Result};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::FlowClient;

/// Returns a canned fenced AST as the "compiled plan" — the shape `FlowClient::compile` parses.
struct AstMock {
    ast_json: String,
}

#[async_trait]
impl Provider for AstMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let reply = format!("```json\n{}\n```", self.ast_json);
        let chunks = vec![
            Ok(Chunk::TextDelta(reply)),
            Ok(Chunk::Done { stop_reason: None }),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::temp_dir().join(format!("flux-flow-compile-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|e| flux_core::Error::Other(e.to_string()))?;
    std::fs::write(root.join("doc.md"), "the lifecycle works")
        .map_err(|e| flux_core::Error::Other(e.to_string()))?;

    // The plan the "model" emits: read doc.md.
    let ast_json =
        r#"{"body":[{"kind":"call","op":"read","args":[{"kind":"lit","value":"doc.md"}]}]}"#;

    let client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(
            Arc::new(AstMock {
                ast_json: ast_json.to_string(),
            }),
            &root,
        )?;

    // compile → analyze → execute, all in one call.
    let out = client.run("read the doc and show it").await?;
    println!("dispatched {} ops: {:?}", out.steps, out.tool_calls);
    println!("result: {}", out.result.replace('\n', " "));

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}

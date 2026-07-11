//! Streaming a turn (D-144/D-145): [`flux_sdk::Session::stream`] returns a `TurnStream` of owned
//! [`flux_sdk::AgentEvent`]s you can consume with a loop, plus `cancel()`/`finish()`. Here a hermetic
//! mock streams a couple of text deltas.
//!
//! Run with: `cargo run -p codewandler-flux-sdk --example streaming`

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::{AgentEvent, Client};

/// Streams the answer in two deltas, then the final block.
struct StreamingMock;

#[async_trait]
impl Provider for StreamingMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(futures::stream::iter(
            vec![
                Chunk::TextDelta("Streaming ".into()),
                Chunk::TextDelta("turns.".into()),
                Chunk::Block(ContentBlock::Text {
                    text: "Streaming turns.".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ]
            .into_iter()
            .map(Ok),
        )))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let client = Client::builder()
        .model("mock")
        .build(Box::new(StreamingMock), ".")?;

    let mut stream = client.default_session().stream("Say something");
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TextDelta(t) => print!("{t}"),
            AgentEvent::ToolCall { name, .. } => println!("\n[tool call: {name}]"),
            AgentEvent::ToolResult { name, .. } => println!("[tool result: {name}]"),
            AgentEvent::TurnEnd { usage } => {
                println!("\n[turn end; usage: {}]", usage.is_some())
            }
            _ => {}
        }
    }
    // `finish()` returns the collected TurnOutput (same as a plain `send`).
    let out = stream.finish().await?;
    println!("collected text: {}", out.text);
    Ok(())
}

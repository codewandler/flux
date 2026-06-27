//! The classic agent front door: [`flux_sdk::Client`] wraps the tool-enabled agent loop, the safety
//! envelope, and a session. Here a hermetic mock provider returns one text turn so the example runs
//! with no API key.
//!
//! Run with: `cargo run -p flux-sdk --example client_basic`

use std::sync::Mutex;

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason, Usage};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::Client;

/// Replays one canned assistant turn, then nothing.
struct OneShotMock {
    chunks: Mutex<Option<Vec<Chunk>>>,
}

#[async_trait]
impl Provider for OneShotMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let chunks = self.chunks.lock().unwrap().take().unwrap_or_default();
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let provider = Box::new(OneShotMock {
        chunks: Mutex::new(Some(vec![
            Chunk::TextDelta("Hello from the flux-sdk Client.".into()),
            Chunk::Block(ContentBlock::Text {
                text: "Hello from the flux-sdk Client.".into(),
            }),
            Chunk::Usage(Usage {
                input_tokens: 8,
                output_tokens: 7,
                ..Default::default()
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ])),
    });

    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .build(provider, ".")?;

    let out = client.run("Say hello").await?;
    println!("text: {}", out.text);
    println!("tool calls: {:?}", out.tool_calls);
    if let Some(u) = out.usage {
        println!("tokens: in={} out={}", u.input_tokens, u.output_tokens);
    }
    Ok(())
}

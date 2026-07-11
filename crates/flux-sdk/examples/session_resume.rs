//! Durable sessions (D-142): point a [`flux_sdk::Client`] at a [`flux_sdk::Storage::dir`] and its
//! conversation survives the process. Here one client runs a turn and "exits"; a second client over
//! the same directory reopens the session by id and reads its history.
//!
//! Run with: `cargo run -p codewandler-flux-sdk --example session_resume`

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::{Client, Storage};

/// Answers with a fixed line on every turn (survives multiple client builds).
struct ProseMock(&'static str);

#[async_trait]
impl Provider for ProseMock {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let text = self.0.to_string();
        Ok(Box::pin(futures::stream::iter(
            vec![
                Chunk::Block(ContentBlock::Text { text: text.clone() }),
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
    let dir = std::env::temp_dir().join("flux-sdk-session-resume-example");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let store = dir.join("state");

    // First "process": run a turn against a persistent store, then drop the client.
    let id = {
        let client = Client::builder()
            .model("mock")
            .storage(Storage::dir(&store))
            .build(Box::new(ProseMock("Filed under 'later'.")), &dir)?;
        client.run("Remember: the meeting moved to Friday.").await?;
        client.session_id()?
    };
    println!("first process wrote session {id}");

    // Second "process": a brand-new client over the same directory resumes the session.
    let client = Client::builder()
        .model("mock")
        .storage(Storage::dir(&store))
        .build(Box::new(ProseMock("Got it.")), &dir)?;
    let session = client.open_session(&id)?;
    println!(
        "resumed session {} with {} prior messages",
        session.id(),
        session.history()?.len()
    );
    session.send("What day did the meeting move to?").await?;
    println!("after the follow-up: {} messages", session.history()?.len());

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

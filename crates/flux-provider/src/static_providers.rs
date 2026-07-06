//! Deterministic, key-free [`Provider`] implementations.
//!
//! Neither talks to a network — no API key, no HTTP request, no cost. They cover the two recurring
//! shapes that otherwise get hand-rolled per crate/test as a bespoke `struct Mock; impl Provider for
//! Mock { .. }` (see almost any `mod tests` in this workspace):
//!
//! - [`NullProvider`] — never generates. For flows that are fully deterministic/preset (no step in
//!   them actually calls the model) but still need a concrete [`Provider`] to construct — wiring one
//!   up with no API key at all.
//! - [`StaticProvider`] — always answers with the same fixed text, as one complete turn. For
//!   key-free demos/dry runs and tests that want a canned reply without hand-rolling a mock
//!   `Provider` per call site.

use async_trait::async_trait;
use futures::stream;

use flux_core::{Chunk, ContentBlock, Result, StopReason};

use crate::{ChunkStream, Provider, Request};

/// A provider that never generates: [`stream`](Provider::stream) completes immediately with zero
/// chunks — no [`Chunk::TextDelta`], no [`Chunk::Block`], not even a [`Chunk::Done`]. Callers that
/// only accumulate text/blocks from the stream (e.g. `flux-flow`'s planner) see an empty turn, not a
/// hang or an error.
///
/// Use this for flows that are fully deterministic/preset — every step is a scripted tool call, no
/// step actually consults the model — but the surrounding code still needs a `Provider` value to
/// construct (a role, a spawner, a client). If such a flow is ever restructured to genuinely reach
/// the model while still wired to `NullProvider`, the turn completes with an empty reply rather than
/// blocking forever or panicking, so the failure mode is "the model said nothing" — legible and
/// local — instead of a hang.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProvider;

#[async_trait]
impl Provider for NullProvider {
    fn name(&self) -> &str {
        "null"
    }

    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(stream::empty()))
    }
}

/// A provider that always answers with the same fixed text, as one complete turn (`TextDelta` +
/// a `Text` [`ContentBlock`] + `Done { stop_reason: EndTurn }`). For key-free paths (demos, dry
/// runs) and tests that want a canned reply without hand-rolling a mock [`Provider`] per call site.
#[derive(Debug, Clone)]
pub struct StaticProvider {
    text: String,
}

impl StaticProvider {
    /// A `StaticProvider` that always replies with `text`.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

#[async_trait]
impl Provider for StaticProvider {
    fn name(&self) -> &str {
        "static"
    }

    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let text = self.text.clone();
        let chunks = vec![
            Chunk::TextDelta(text.clone()),
            Chunk::Block(ContentBlock::Text { text }),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ];
        Ok(Box::pin(stream::iter(chunks.into_iter().map(Ok))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn null_provider_stream_ends_immediately_with_no_chunks() {
        let mut s = NullProvider
            .stream(Request::new("any", "hi"))
            .await
            .unwrap();
        assert!(
            s.next().await.is_none(),
            "NullProvider must yield zero chunks"
        );
    }

    #[tokio::test]
    async fn static_provider_replies_with_the_same_fixed_text_every_call() {
        let p = StaticProvider::new("scouted: 3 files");
        for _ in 0..2 {
            let mut s = p.stream(Request::new("any", "look around")).await.unwrap();
            let mut text = String::new();
            let mut done = None;
            while let Some(chunk) = s.next().await {
                match chunk.unwrap() {
                    Chunk::TextDelta(t) => text.push_str(&t),
                    Chunk::Done { stop_reason } => done = stop_reason,
                    _ => {}
                }
            }
            assert_eq!(text, "scouted: 3 files");
            assert_eq!(done, Some(StopReason::EndTurn));
        }
    }
}

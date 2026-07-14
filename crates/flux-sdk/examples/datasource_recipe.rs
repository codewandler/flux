//! **Attaching a knowledge datasource to an embedded agent** (D-159).
//!
//! There is no first-class `with_datasource(...)` on the SDK builders yet — deliberately: the
//! first-class surface waits on the async paged live-backend seam (story D-62) so we don't freeze
//! the wrong contract (see the "Out of scope" note in `docs/designs/sdk-surface.md`). Until then,
//! the recipe below is the supported way to give an embedded agent retrieval ops: add a direct
//! `flux-capabilities` dependency, build a backend, and register its ops through the existing
//! [`FlowClient::try_register_pack`] (or [`ClientBuilder::try_register_pack`]) seam. The registered
//! `search`/`get`/`list`/`relation`/`batch_get`/`sources` ops dispatch through the same
//! authorization → approval → guarded-IO envelope as every built-in.
//!
//! Run with: `cargo run -p codewandler-flux-sdk --example datasource_recipe`

use std::sync::Arc;

use async_trait::async_trait;
use flux_capabilities::{
    ingest_markdown, try_register_datasource_ops, DatasourceBackend, MemoryBackend,
};
use flux_core::{Chunk, Result};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::FlowClient;

/// The datasource ops dispatch directly (no planning), so the model is never called here.
struct NoopProvider;
#[async_trait]
impl Provider for NoopProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(futures::stream::iter(vec![Ok(Chunk::Done {
            stop_reason: None,
        })])))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Build a datasource backend and index some documents into it. `MemoryBackend` is the default
    //    keyword backend; a Postgres/embeddings backend implements the same `DatasourceBackend` trait.
    let backend: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
    let docs = vec![(
        "intro.md".to_string(),
        "# Flux\nFlux is a deterministic agent platform: the LLM is not the runtime.".to_string(),
    )];
    ingest_markdown(&*backend, "local", &docs)?;

    // 2. Register the retrieval ops against the backend via `try_register_pack` — the same seam any
    //    op-pack uses. `move` hands the shared backend into the installer.
    let mut client = FlowClient::builder()
        .auto_approve(true)
        .build(Arc::new(NoopProvider), ".")?;
    client.try_register_pack(move |registry| {
        try_register_datasource_ops(registry, backend.clone())
    })?;

    // 3. The retrieval ops are now part of the agent's catalog, gated by the safety envelope.
    let ops = client.op_names();
    for op in ["search", "get", "list", "relation", "batch_get", "sources"] {
        assert!(
            ops.iter().any(|n| n == op),
            "the `{op}` datasource op is registered"
        );
    }
    println!("datasource ops attached: search, get, list, relation, batch_get, sources");
    Ok(())
}

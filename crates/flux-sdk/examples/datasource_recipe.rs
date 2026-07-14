//! **Attaching a knowledge datasource to an embedded agent** (D-159).
//!
//! This example covers **indexed knowledge**: ingest records into a local index-shaped
//! [`DatasourceBackend`], then attach its generic retrieval operations through
//! [`FlowClient::try_register_pack`] (or `ClientBuilder::try_register_pack`). The registered
//! `search`/`get`/`list`/`relation`/`batch_get`/`sources` operations dispatch through the same
//! authorization → approval → guarded-IO envelope as every built-in.
//!
//! Async systems of record use a different, first-class shape: implement
//! `flux_sdk::datasource::LiveDatasource` and call `ClientBuilder::try_with_live_datasource` to
//! install generated `<domain>.list` / `<domain>.get` operations with their evidence surface and
//! exact datasource plus backend authority. See the hermetic `live_datasource` example.
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

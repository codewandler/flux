//! `datasource` — the knowledge layer (story D-07).
//!
//! A pluggable [`DatasourceBackend`] over [`flux_datasource`] records, plus the retrieval ops the agent
//! calls ([`register_datasource_ops`]). Two backends implement the trait: [`MemoryBackend`] (the default
//! — an in-memory keyword index) and (later) a persistent SQLite-FTS5 store. The record/retrieval *types*
//! live in the L0 `flux-datasource` crate so plugins (`flux-plugin`) can emit records into the same shape.
//!
//! v1 ranks by keyword/term-frequency; the [`Embedder`] seam is defined but **no semantic backend is
//! wired** (deferred).

mod host_caps;
mod ingest;
mod memory;
mod ops;
mod semantic;
mod sqlite;
mod text;
mod vector;

#[cfg(feature = "postgres")]
mod postgres;

#[cfg(feature = "embeddings")]
mod embeddings;

#[cfg(feature = "local-embeddings")]
mod embeddings_local;

pub use host_caps::DatasourceHostCaps;
pub use ingest::{
    chunk_text, freshness, ingest_markdown, ingest_openapi, ingest_text, reindex, ChunkOptions,
};
pub use memory::MemoryBackend;
pub use ops::{datasource_tools, register_datasource_ops};
pub use semantic::SemanticIndex;
pub use sqlite::SqliteBackend;

#[cfg(feature = "postgres")]
pub use postgres::PostgresBackend;
pub use vector::{MemoryVectorStore, VectorAddr, VectorStore};

#[cfg(feature = "embeddings")]
pub use embeddings::OpenAiEmbedder;

#[cfg(feature = "local-embeddings")]
pub use embeddings_local::FastEmbedEmbedder;

#[cfg(feature = "sqlite-vec")]
pub use vector::SqliteVecStore;

use flux_core::{ContextBlock, Result};
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput, SourceSummary,
};

/// A datasource index backend: upsert records and answer the six retrieval verbs. All methods take
/// `&self` (interior mutability) so a backend is shared as `Arc<dyn DatasourceBackend>` across the ops.
///
/// The trait is **per-scope by design** — an implementor instance is bound to exactly one scope (a
/// SQLite file, a Postgres `ns`). Cross-scope reads (`namespaces`, `scan`) are deliberately
/// associated fns on the Postgres impl, not trait methods.
pub trait DatasourceBackend: Send + Sync {
    /// Insert or replace records, keyed by their `(source, entity, id)` address.
    fn upsert(&self, records: &[Record]) -> Result<()>;
    /// Keyword/relevance search, optionally scoped to one source/entity, capped by `limit`.
    fn search(&self, input: &SearchInput) -> Result<Vec<Match>>;
    /// Fetch one record by its `(source, entity, id)` address.
    fn get(&self, input: &GetInput) -> Result<Option<Record>>;
    /// Enumerate a source's records (optionally one entity), paged.
    fn list(&self, input: &ListInput) -> Result<Vec<Record>>;
    /// The records linked from one record (optionally filtered by relation name).
    fn relation(&self, input: &RelationInput) -> Result<Vec<Record>>;
    /// Fetch several records of one entity from one source in a single call.
    fn batch_get(&self, input: &BatchGetInput) -> Result<Vec<Record>>;
    /// Enumerate the distinct sources this backend holds: per source, its distinct entity types
    /// (sorted) and total record count (story D-114). Closes the discoverability loop the other
    /// five methods presuppose — `search`/`get`/`list`/`relation`/`batch_get` all require a known
    /// `source` key; this is how a caller learns which ones exist.
    fn sources(&self) -> Result<Vec<SourceSummary>>;
    /// Drop every record (the rebuild half of reindex; the caller then re-ingests).
    fn clear(&self) -> Result<()>;
    /// Drop every record under one source key. Returns how many were removed. Unlike [`clear`](Self::clear)
    /// this is scoped to a single source, so a multi-source backend can manage one source's lifecycle.
    fn delete_source(&self, source: &str) -> Result<usize>;
    /// Drop specific records of one entity in one source by id. Returns how many were removed.
    fn delete(&self, source: &str, entity: &str, ids: &[String]) -> Result<usize>;
    /// Total record count (diagnostics / freshness).
    fn len(&self) -> usize;
    /// Whether the index holds no records.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The embeddings seam — **deferred, no backend wired in v1**. A semantic backend implements this and a
/// hybrid (keyword + vector) search path layers on top behind it later.
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts into vectors.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Turn datasource [`Record`]s into injectable [`ContextBlock`]s (story A-19): a consumer that wants to
/// **inject** a small KB inline into the system prompt (instead of exposing the `search` tool) maps its
/// records through this and hands the result to [`flux_core::render_knowledge_blocks`]. `source`/`entity`
/// ride in `meta` so they render as tag attributes.
pub fn records_to_context_blocks(records: &[Record]) -> Vec<ContextBlock> {
    records
        .iter()
        .map(|r| ContextBlock {
            id: r.id.clone(),
            title: r.title.clone(),
            meta: serde_json::json!({ "source": r.source.key(), "entity": r.entity }),
            body: r.body.clone(),
        })
        .collect()
}

#[cfg(test)]
mod block_tests {
    use super::*;
    use flux_datasource::Source;

    #[test]
    fn records_map_to_blocks_and_render() {
        let recs = [Record::new(
            Source::new("local"),
            "file.document",
            "hours",
            "Opening hours",
            "Mon–Fri 09:00–18:00 CET.",
        )];
        let blocks = records_to_context_blocks(&recs);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].id, "hours");
        let rendered = flux_core::render_knowledge_blocks(&blocks, 0);
        assert!(rendered.contains("<knowledge-base id=\"hours\" title=\"Opening hours\""));
        assert!(
            rendered.contains("source=\"local\""),
            "source is an attribute: {rendered}"
        );
        assert!(rendered.contains("Mon–Fri 09:00–18:00 CET."));
    }
}

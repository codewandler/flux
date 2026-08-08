//! `datasource` — indexed knowledge and live systems of record.
//!
//! [`DatasourceBackend`] is the index-shaped contract: hosts ingest [`flux_datasource`] records into
//! memory, SQLite, Postgres, or a semantic index and attach the generic
//! `search`/`get`/`list`/`relation`/`batch_get`/`sources` pack with
//! [`try_register_datasource_ops`]. This remains the right shape for local, indexed knowledge.
//!
//! [`LiveDatasource`] is the async system-of-record contract. [`try_register_live_datasource`]
//! snapshots its entity/filter/page schema and declared [`LiveAccess`], then installs generated
//! `<domain>.list` / `<domain>.get` operations plus their evidence surface. Each invocation carries
//! stable `<domain>/<entity>` permission subjects and exact datasource plus network/connection
//! authority through the ordinary runtime envelope.
//!
//! [`WorkBoard`] is the **write-capable** sibling of that contract, and follows it exactly:
//! [`try_register_work_board`] snapshots the schema and [`LiveAccess`], validates once, and installs
//! generated `<domain>.list` / `.get` / `.create` / `.transition` / `.claim` / `.comment`
//! operations atomically on a clone. The difference is that five of the seven write, so they carry
//! `Effect::Write`, a non-`Low` risk tier, and **concrete** `<domain>/item/<id>` permission
//! subjects — never `*`, never empty (AGENTS.md:98). Items move through a closed state machine and
//! an illegal edge is an error, not a write. [`MemoryBoard`] is the offline double.
//!
//! [`HarnessHistory`] is the **contained** ingest of another coding harness's conversation history
//! (C-215). It is the one source whose input is outside the workspace jail, secret-bearing by
//! construction, and verbatim adversarial text — so it ships **off by default**, every body is
//! redacted and `<knowledge-base>`-escaped *at ingest* rather than at render, and `search` carries
//! per-harness `datasource:harness.<id>` permission subjects. See `harness_history` for the
//! reasoning; `crate::harness` is the acquisition layer beneath it.
//!
//! The record/retrieval contracts and pure live row, filter, page, and weak-reference types live in
//! the L0 `flux-datasource` crate, as do the board's item and state-machine contracts. Real IO
//! remains host-owned and guarded; no contract here gives the model a credential, connection, or
//! live handle.

mod board;
mod board_registry;
mod harness_history;
mod host_caps;
mod ingest;
mod live;
mod markdown_board;
mod memory;
mod memory_board;
mod ops;
mod semantic;
mod session_board;
mod sqlite;
mod text;
mod vector;

#[cfg(feature = "postgres")]
mod postgres;

#[cfg(feature = "embeddings")]
mod embeddings;

#[cfg(feature = "local-embeddings")]
mod embeddings_local;

pub use board::{
    board_connection_system, try_register_work_board, validate_board_contract, work_board_tools,
    BoardLedger, WorkBoard, WorkBoardSurface,
};
pub use board_registry::{BoardBinding, BoardRegistry};
pub use harness_history::{
    ingest_harness_history, HarnessHistory, HarnessIngestReport, HarnessSelector,
    HARNESS_MESSAGE_ENTITY, HARNESS_SESSION_ENTITY, HARNESS_SESSION_REL, HARNESS_SOURCE,
    MAX_LIVE_SESSION_ENVELOPES,
};
pub use host_caps::DatasourceHostCaps;
pub use ingest::{
    chunk_text, freshness, ingest_markdown, ingest_openapi, ingest_text, reindex, ChunkOptions,
};
pub use live::{
    admit_live_locality, declared_host, live_connection_system, live_datasource_tools,
    try_register_live_datasource, validate_live_contract, LiveAccess, LiveDatasource,
    LiveDatasourceSurface, LiveLocality,
};
pub use markdown_board::MarkdownBoard;
pub use memory::MemoryBackend;
pub use memory_board::MemoryBoard;
pub use ops::{
    datasource_tools, datasource_tools_with_history, register_datasource_ops,
    try_register_datasource_ops, try_register_datasource_ops_with_history,
};
pub use semantic::SemanticIndex;
pub use session_board::{SessionBoard, SessionBoardItem, SessionBoardSnapshot};
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

/// The embeddings seam. A semantic backend implements this; the hybrid (keyword + vector)
/// [`SemanticIndex`] layers on top of it. Concrete embedders: the remote [`OpenAiEmbedder`]
/// (feature `embeddings`) and the local fastembed one (feature `local-embeddings`).
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts into vectors.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Token usage reported by an embeddings API call (story D-162). Embeddings endpoints return a
/// `usage` object that flux previously discarded; capturing it makes embedding token spend
/// observable (an embedder accumulates it — e.g. [`OpenAiEmbedder::usage_snapshot`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingUsage {
    /// Tokens in the embedded input(s).
    pub prompt_tokens: u64,
    /// Total tokens billed (equals `prompt_tokens` for embeddings endpoints that don't split it).
    pub total_tokens: u64,
}

impl EmbeddingUsage {
    /// Fold onto the shared [`flux_core::Usage`] tally (embedding input tokens → `input_tokens`) so a
    /// consumer can route embedding spend through the same usage/pricing/event machinery as chat
    /// calls. (Emitting a `CallUsage` event into `flux usage` is a CLI-side follow-up.)
    pub fn as_usage(&self) -> flux_core::Usage {
        flux_core::Usage {
            input_tokens: self.prompt_tokens,
            ..Default::default()
        }
    }
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

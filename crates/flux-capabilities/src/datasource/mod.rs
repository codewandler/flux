//! `datasource` — the knowledge layer (story D-07).
//!
//! A pluggable [`DatasourceBackend`] over [`flux_datasource`] records, plus the retrieval ops the agent
//! calls ([`register_datasource_ops`]). Two backends implement the trait: [`MemoryBackend`] (the default
//! — an in-memory keyword index) and (later) a persistent SQLite-FTS5 store. The record/retrieval *types*
//! live in the L0 `flux-datasource` crate so plugins (`flux-plugin`) can emit records into the same shape.
//!
//! v1 ranks by keyword/term-frequency; the [`Embedder`] seam is defined but **no semantic backend is
//! wired** (deferred).

mod memory;
mod ops;

pub use memory::MemoryBackend;
pub use ops::register_datasource_ops;

use flux_core::Result;
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput,
};

/// A datasource index backend: upsert records and answer the five retrieval verbs. All methods take
/// `&self` (interior mutability) so a backend is shared as `Arc<dyn DatasourceBackend>` across the ops.
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

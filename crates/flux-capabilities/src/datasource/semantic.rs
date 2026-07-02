//! [`SemanticIndex`] — a hybrid (keyword + embeddings) decorator over any [`DatasourceBackend`].
//!
//! Wraps an inner backend (the keyword index) plus an [`Embedder`](super::Embedder) and a
//! [`VectorStore`](super::VectorStore): on `upsert` it embeds each **opted-in** record's `title`+`body` and
//! persists the vector; on `search` it widens the inner keyword candidate set, embeds the query, and
//! re-ranks by a **blend** of normalized keyword score and cosine similarity. All other verbs delegate.
//!
//! Two knobs added by story D-51:
//! - **Per-KB opt-in** ([`with_semantic_sources`](SemanticIndex::with_semantic_sources)) — only records of
//!   the named sources are embedded/reranked; every other source stays keyword-only (still fully
//!   searchable). Without it, every source is embedded (the prior behavior).
//! - **Pluggable vector store** ([`with_vector_store`](SemanticIndex::with_vector_store)) — vectors default
//!   to [`MemoryVectorStore`](super::MemoryVectorStore) but a durable backing (e.g. `sqlite-vec`) makes
//!   them survive a restart with no re-embed.
//!
//! Additive: the decorator is only inserted when an embedder is configured; without it the inner backend
//! behaves exactly as before. The decorator itself is pure logic (no IO, not feature-gated), so its rerank
//! and opt-in are unit-tested in the default build with a stub embedder; the only feature-gated pieces are
//! the concrete embedders (remote [`OpenAiEmbedder`](super::OpenAiEmbedder), local fastembed).

use std::collections::HashSet;
use std::sync::Arc;

use flux_core::Result;
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput,
};

use super::{DatasourceBackend, Embedder, MemoryVectorStore, VectorStore};

/// A backend that adds embeddings-based semantic rerank on top of an inner keyword backend.
pub struct SemanticIndex {
    inner: Arc<dyn DatasourceBackend>,
    embedder: Arc<dyn Embedder>,
    vectors: Arc<dyn VectorStore>,
    /// `None` = every source is embedded; `Some(set)` = only these source keys (per-KB opt-in, D-51).
    semantic_sources: Option<HashSet<String>>,
    keyword_weight: f64,
}

impl SemanticIndex {
    /// Wrap `inner` with semantic rerank via `embedder` (in-memory vectors, all sources, keyword weight
    /// `0.5`).
    pub fn new(inner: Arc<dyn DatasourceBackend>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            inner,
            embedder,
            vectors: Arc::new(MemoryVectorStore::new()),
            semantic_sources: None,
            keyword_weight: 0.5,
        }
    }

    /// Set the keyword weight `w` (clamped to `[0,1]`); the cosine weight is `1 - w`.
    pub fn with_keyword_weight(mut self, w: f64) -> Self {
        self.keyword_weight = w.clamp(0.0, 1.0);
        self
    }

    /// Persist vectors in `store` instead of the default in-memory one (e.g. a durable `sqlite-vec` store
    /// so vectors survive a restart). D-51.
    pub fn with_vector_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.vectors = store;
        self
    }

    /// Restrict embedding + semantic rerank to these source keys (per-KB opt-in, D-51). Records of other
    /// sources stay keyword-only. Without this, every source is embedded.
    pub fn with_semantic_sources(mut self, sources: impl IntoIterator<Item = String>) -> Self {
        self.semantic_sources = Some(sources.into_iter().collect());
        self
    }

    /// Whether `source_key` is embedded/reranked (all sources when no opt-in set is configured).
    fn is_semantic(&self, source_key: &str) -> bool {
        self.semantic_sources
            .as_ref()
            .map(|s| s.contains(source_key))
            .unwrap_or(true)
    }
}

/// Cosine similarity of two equal-length vectors (`0.0` for mismatched/empty/zero vectors).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

impl DatasourceBackend for SemanticIndex {
    fn upsert(&self, records: &[Record]) -> Result<()> {
        self.inner.upsert(records)?;
        // Only embed opted-in sources (per-KB opt-in). Others are keyword-only.
        let to_embed: Vec<&Record> = records
            .iter()
            .filter(|r| self.is_semantic(&r.source.key()))
            .collect();
        if to_embed.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = to_embed
            .iter()
            .map(|r| format!("{}\n{}", r.title, r.body))
            .collect();
        // Best-effort: an embedding failure must not lose the keyword index (records already upserted).
        if let Ok(vecs) = self.embedder.embed(&texts) {
            if vecs.len() == to_embed.len() {
                for (r, v) in to_embed.iter().zip(vecs) {
                    let _ = self.vectors.upsert(r.address(), v);
                }
            }
        }
        Ok(())
    }

    fn search(&self, input: &SearchInput) -> Result<Vec<Match>> {
        let limit = input.limit.unwrap_or(5);
        // Widen the keyword candidate set, then semantic-rerank and truncate back to `limit`.
        let wide = SearchInput {
            limit: Some(limit.saturating_mul(4).max(limit)),
            ..input.clone()
        };
        let candidates = self.inner.search(&wide)?;
        if candidates.is_empty() {
            return Ok(candidates);
        }
        // Embed the query (best-effort: on failure, fall back to keyword order).
        let query_vec = match self.embedder.embed(std::slice::from_ref(&input.query)) {
            Ok(mut v) if !v.is_empty() => v.remove(0),
            _ => {
                let mut c = candidates;
                c.truncate(limit);
                return Ok(c);
            }
        };
        let max_kw = candidates
            .iter()
            .map(|m| m.score)
            .fold(0.0f64, f64::max)
            .max(1e-9);
        let w = self.keyword_weight;
        let mut scored: Vec<Match> = candidates
            .into_iter()
            .map(|mut m| {
                let kw_norm = m.score / max_kw; // [0,1]
                                                // A record with no stored vector (keyword-only source, or embedding not yet computed)
                                                // contributes cosine 0 — it ranks on keyword alone.
                let cos = self
                    .vectors
                    .get(&m.record.address())
                    .ok()
                    .flatten()
                    .map(|v| cosine(&query_vec, &v))
                    .unwrap_or(0.0) as f64;
                let cos_norm = ((cos + 1.0) / 2.0).clamp(0.0, 1.0); // [-1,1] -> [0,1]
                m.score = w * kw_norm + (1.0 - w) * cos_norm;
                m
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);
        Ok(scored)
    }

    fn get(&self, input: &GetInput) -> Result<Option<Record>> {
        self.inner.get(input)
    }
    fn list(&self, input: &ListInput) -> Result<Vec<Record>> {
        self.inner.list(input)
    }
    fn relation(&self, input: &RelationInput) -> Result<Vec<Record>> {
        self.inner.relation(input)
    }
    fn batch_get(&self, input: &BatchGetInput) -> Result<Vec<Record>> {
        self.inner.batch_get(input)
    }
    fn clear(&self) -> Result<()> {
        self.vectors.clear()?;
        self.inner.clear()
    }
    fn delete_source(&self, source: &str) -> Result<usize> {
        self.vectors.delete_source(source)?;
        self.inner.delete_source(source)
    }
    fn delete(&self, source: &str, entity: &str, ids: &[String]) -> Result<usize> {
        self.vectors.delete(source, entity, ids)?;
        self.inner.delete(source, entity, ids)
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datasource::MemoryBackend;
    use flux_datasource::Source;

    /// A deterministic stub embedder: a 2-d vector `[has "alpha", has "beta"]` — no network.
    struct StubEmbedder;
    impl Embedder for StubEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    vec![
                        if t.contains("alpha") { 1.0 } else { 0.0 },
                        if t.contains("beta") { 1.0 } else { 0.0 },
                    ]
                })
                .collect())
        }
    }

    fn doc_in(source: &str, id: &str, title: &str, body: &str) -> Record {
        Record::new(Source::new(source), "file.document", id, title, body)
    }
    fn doc(id: &str, title: &str, body: &str) -> Record {
        doc_in("local", id, title, body)
    }

    #[test]
    fn semantic_rerank_prefers_the_cosine_match() {
        let inner: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
        // pure cosine (keyword weight 0) so the embedding decides the order
        let idx = SemanticIndex::new(inner, Arc::new(StubEmbedder)).with_keyword_weight(0.0);
        // Both docs share the keyword "topic" (so both are keyword candidates); embeddings differ.
        idx.upsert(&[
            doc("a", "alpha topic", "alpha topic body"),
            doc("b", "beta topic", "beta topic body"),
        ])
        .unwrap();
        // Query embeds to [0,1] (beta) → the beta doc must rank first under cosine.
        let hits = idx
            .search(&SearchInput {
                query: "beta topic".into(),
                limit: Some(2),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].record.id, "b",
            "cosine should rank the beta doc first"
        );
    }

    #[test]
    fn delegates_non_search_verbs_and_clear() {
        let inner: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
        let idx = SemanticIndex::new(inner, Arc::new(StubEmbedder));
        idx.upsert(&[doc("x", "t", "b")]).unwrap();
        assert_eq!(idx.len(), 1);
        assert!(idx
            .get(&GetInput {
                source: "local".into(),
                entity: "file.document".into(),
                id: "x".into(),
            })
            .unwrap()
            .is_some());
        idx.clear().unwrap();
        assert_eq!(idx.len(), 0);
    }

    /// D-51: per-KB opt-in — only records of a `semantic` source get a vector; a `default` source stays
    /// keyword-only (no vector stored).
    #[test]
    fn per_kb_opt_in_embeds_only_selected_sources() {
        let inner: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
        let store = Arc::new(MemoryVectorStore::new());
        let idx = SemanticIndex::new(inner, Arc::new(StubEmbedder))
            .with_vector_store(store.clone())
            .with_semantic_sources(["sem".to_string()]);
        idx.upsert(&[
            doc_in("sem", "1", "alpha", "alpha body"),
            doc_in("plain", "1", "alpha", "alpha body"),
        ])
        .unwrap();
        // The opted-in source has a stored vector; the keyword-only source does not.
        assert!(store
            .get(&("sem".into(), "file.document".into(), "1".into()))
            .unwrap()
            .is_some());
        assert!(store
            .get(&("plain".into(), "file.document".into(), "1".into()))
            .unwrap()
            .is_none());
    }

    /// D-51: durability — a fresh `SemanticIndex` sharing an already-populated vector store + inner backend
    /// semantic-reranks **without re-embedding** the documents (proxy for surviving a restart).
    #[test]
    fn reranks_from_a_prepopulated_store_without_reembedding() {
        let inner: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
        let store: Arc<dyn VectorStore> = Arc::new(MemoryVectorStore::new());
        // First index writes records + vectors.
        SemanticIndex::new(inner.clone(), Arc::new(StubEmbedder))
            .with_vector_store(store.clone())
            .with_keyword_weight(0.0)
            .upsert(&[
                doc("a", "alpha topic", "alpha topic body"),
                doc("b", "beta topic", "beta topic body"),
            ])
            .unwrap();
        // A brand-new index over the SAME store + inner (never upserts) still reranks by cosine — the
        // vectors came from the store, not a re-embed.
        let reopened = SemanticIndex::new(inner, Arc::new(StubEmbedder))
            .with_vector_store(store)
            .with_keyword_weight(0.0);
        let hits = reopened
            .search(&SearchInput {
                query: "beta topic".into(),
                limit: Some(2),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits[0].record.id, "b", "reranked from persisted vectors");
    }
}

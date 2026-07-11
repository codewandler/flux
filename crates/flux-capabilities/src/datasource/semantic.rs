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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use flux_core::Result;
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput, SourceSummary,
};

use super::{DatasourceBackend, Embedder, MemoryVectorStore, VectorStore};

/// A backend that adds embeddings-based semantic rerank on top of an inner keyword backend.
pub struct SemanticIndex {
    inner: Arc<dyn DatasourceBackend>,
    embedder: Arc<dyn Embedder>,
    /// Per-source embedder overrides (story D-162). Empty = every source uses `embedder` (the prior,
    /// single-global-embedder behavior). A source present here is embedded — and its scoped queries
    /// embedded — with its own embedder, so a deployment can route different KBs to different
    /// models/providers.
    source_embedders: HashMap<String, Arc<dyn Embedder>>,
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
            source_embedders: HashMap::new(),
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

    /// Route `source_key` to its own `embedder` (story D-162) instead of the default one passed to
    /// [`new`](Self::new). Records of that source — and queries scoped to it — are embedded with
    /// this embedder; every other source keeps the default. Cosine rerank stays within one embedding
    /// space: a query is only compared against vectors produced by the same embedder it was embedded
    /// with (see [`search`](DatasourceBackend::search)). Additive — with no override configured the
    /// index behaves exactly as before.
    pub fn with_source_embedder(
        mut self,
        source_key: impl Into<String>,
        embedder: Arc<dyn Embedder>,
    ) -> Self {
        self.source_embedders.insert(source_key.into(), embedder);
        self
    }

    /// The embedder that handles `source_key`: its routed override, or the default embedder.
    fn embedder_for(&self, source_key: &str) -> &Arc<dyn Embedder> {
        self.source_embedders
            .get(source_key)
            .unwrap_or(&self.embedder)
    }

    /// Embed `records` with `embedder` and persist each vector. Best-effort: an embedding failure (or
    /// a length mismatch) leaves the keyword index intact — the records are already upserted.
    fn embed_and_store(&self, embedder: &Arc<dyn Embedder>, records: &[&Record]) {
        if records.is_empty() {
            return;
        }
        let texts: Vec<String> = records
            .iter()
            .map(|r| format!("{}\n{}", r.title, r.body))
            .collect();
        if let Ok(vecs) = embedder.embed(&texts) {
            if vecs.len() == records.len() {
                for (r, v) in records.iter().zip(vecs) {
                    let _ = self.vectors.upsert(r.address(), v);
                }
            }
        }
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
        if self.source_embedders.is_empty() {
            // Default path (unchanged): one batch through the single default embedder.
            self.embed_and_store(&self.embedder, &to_embed);
        } else {
            // Routed path (D-162): group by source key and embed each group with its own embedder,
            // so different KBs can use different models.
            let mut by_source: HashMap<String, Vec<&Record>> = HashMap::new();
            for r in to_embed {
                by_source.entry(r.source.key()).or_default().push(r);
            }
            for (key, recs) in by_source {
                self.embed_and_store(self.embedder_for(&key), &recs);
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
        // The query is embedded with the embedder that owns its source scope (D-162): a scoped
        // search uses that source's embedder; an unscoped search uses the default. Cosine is then
        // only applied against vectors produced by this same embedder (below), so different-model
        // sources never get compared across embedding spaces.
        let query_embedder = match &input.source {
            Some(src) => self.embedder_for(src),
            None => &self.embedder,
        };
        // Embed the query (best-effort: on failure, fall back to keyword order).
        let query_vec = match query_embedder.embed(std::slice::from_ref(&input.query)) {
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
                                                // contributes cosine 0 — it ranks on keyword alone. So does a record whose source
                                                // uses a *different* embedder than the query (D-162): its vectors live in another
                                                // space, so comparing them would be meaningless — keyword-only is the honest score.
                let same_space =
                    Arc::ptr_eq(self.embedder_for(&m.record.source.key()), query_embedder);
                let cos = if same_space {
                    self.vectors
                        .get(&m.record.address())
                        .ok()
                        .flatten()
                        .map(|v| cosine(&query_vec, &v))
                        .unwrap_or(0.0) as f64
                } else {
                    0.0
                };
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
    fn sources(&self) -> Result<Vec<SourceSummary>> {
        self.inner.sources()
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

    /// A stub whose every vector is a single fixed tag value — so a stored vector reveals *which*
    /// embedder produced it (used to prove per-source routing, D-162).
    struct TagEmbedder(f32);
    impl Embedder for TagEmbedder {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![self.0]).collect())
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

    /// D-162: per-source embedder routing — a source with a routed embedder is embedded by *that*
    /// embedder, while every other source uses the default one. The stored vectors' tag values prove
    /// which embedder ran for each source.
    #[test]
    fn per_source_routing_embeds_each_source_with_its_own_embedder() {
        let inner: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
        let store = Arc::new(MemoryVectorStore::new());
        let idx = SemanticIndex::new(inner, Arc::new(TagEmbedder(1.0))) // default embedder tags 1.0
            .with_vector_store(store.clone())
            .with_source_embedder("special", Arc::new(TagEmbedder(9.0))); // routed embedder tags 9.0
        idx.upsert(&[
            doc_in("special", "1", "t", "b"),
            doc_in("plain", "1", "t", "b"),
        ])
        .unwrap();
        let special = store
            .get(&("special".into(), "file.document".into(), "1".into()))
            .unwrap()
            .expect("special source has a vector");
        let plain = store
            .get(&("plain".into(), "file.document".into(), "1".into()))
            .unwrap()
            .expect("plain source has a vector");
        assert_eq!(
            special,
            vec![9.0],
            "special source used its routed embedder"
        );
        assert_eq!(plain, vec![1.0], "plain source used the default embedder");
    }

    /// D-162: a source-scoped search embeds the **query** with that source's routed embedder (not the
    /// default one), so the rerank happens inside the source's own embedding space. Here the default
    /// embedder carries no ranking signal; only the routed `StubEmbedder` can rank beta first.
    #[test]
    fn scoped_search_uses_the_source_routed_embedder_for_the_query() {
        let inner: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
        let idx = SemanticIndex::new(inner, Arc::new(TagEmbedder(0.0))) // default: constant, no signal
            .with_keyword_weight(0.0) // pure cosine so the embedding decides the order
            .with_source_embedder("sem", Arc::new(StubEmbedder));
        idx.upsert(&[
            doc_in("sem", "a", "alpha topic", "alpha topic body"),
            doc_in("sem", "b", "beta topic", "beta topic body"),
        ])
        .unwrap();
        let hits = idx
            .search(&SearchInput {
                query: "beta topic".into(),
                source: Some("sem".into()),
                limit: Some(2),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            hits[0].record.id, "b",
            "the query was embedded by the routed StubEmbedder, so cosine ranks beta first"
        );
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

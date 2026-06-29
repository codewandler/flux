//! [`SemanticIndex`] — a hybrid (keyword + embeddings) decorator over any [`DatasourceBackend`].
//!
//! Wraps an inner backend (the keyword index) plus an [`Embedder`](super::Embedder): on `upsert` it
//! embeds each record's `title`+`body` and stores the vector; on `search` it widens the inner keyword
//! candidate set, embeds the query, and re-ranks by a **blend** of normalized keyword score and cosine
//! similarity. All other verbs delegate. Vectors are held in memory (rebuilt on ingest) in v1.
//!
//! Additive: this decorator is only inserted when an embedder is configured; without it the inner
//! backend behaves exactly as before. The decorator itself is pure logic (no IO, not feature-gated), so
//! its rerank is unit-tested in the default build with a stub embedder; the only feature-gated piece is
//! the concrete remote [`OpenAiEmbedder`](super::OpenAiEmbedder).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use flux_core::Result;
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput,
};

use super::{DatasourceBackend, Embedder};

/// A backend that adds embeddings-based semantic rerank on top of an inner keyword backend.
pub struct SemanticIndex {
    inner: Arc<dyn DatasourceBackend>,
    embedder: Arc<dyn Embedder>,
    vectors: Mutex<HashMap<(String, String, String), Vec<f32>>>,
    keyword_weight: f64,
}

impl SemanticIndex {
    /// Wrap `inner` with semantic rerank via `embedder` (default keyword weight `0.5`).
    pub fn new(inner: Arc<dyn DatasourceBackend>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            inner,
            embedder,
            vectors: Mutex::new(HashMap::new()),
            keyword_weight: 0.5,
        }
    }

    /// Set the keyword weight `w` (clamped to `[0,1]`); the cosine weight is `1 - w`.
    pub fn with_keyword_weight(mut self, w: f64) -> Self {
        self.keyword_weight = w.clamp(0.0, 1.0);
        self
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
        let texts: Vec<String> = records
            .iter()
            .map(|r| format!("{}\n{}", r.title, r.body))
            .collect();
        // Best-effort: an embedding failure must not lose the keyword index (records already upserted).
        if let Ok(vecs) = self.embedder.embed(&texts) {
            if vecs.len() == records.len() {
                let mut store = self.vectors.lock().expect("semantic vectors poisoned");
                for (r, v) in records.iter().zip(vecs) {
                    store.insert(r.address(), v);
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
        let store = self.vectors.lock().expect("semantic vectors poisoned");
        let w = self.keyword_weight;
        let mut scored: Vec<Match> = candidates
            .into_iter()
            .map(|mut m| {
                let kw_norm = m.score / max_kw; // [0,1]
                let cos = store
                    .get(&m.record.address())
                    .map(|v| cosine(&query_vec, v))
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
        self.vectors
            .lock()
            .expect("semantic vectors poisoned")
            .clear();
        self.inner.clear()
    }
    fn delete_source(&self, source: &str) -> Result<usize> {
        self.vectors
            .lock()
            .expect("semantic vectors poisoned")
            .retain(|(src, _, _), _| src != source);
        self.inner.delete_source(source)
    }
    fn delete(&self, source: &str, entity: &str, ids: &[String]) -> Result<usize> {
        self.vectors
            .lock()
            .expect("semantic vectors poisoned")
            .retain(|(src, ent, id), _| {
                !(src == source && ent == entity && ids.iter().any(|i| i == id))
            });
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

    fn doc(id: &str, title: &str, body: &str) -> Record {
        Record::new(Source::new("local"), "file.document", id, title, body)
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
}

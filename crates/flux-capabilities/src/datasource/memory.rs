//! [`MemoryBackend`] — the default in-memory datasource index: a term-frequency keyword ranker over
//! each record's `title`+`body`. No persistence, no embeddings (the persistent SQLite-FTS5 backend and
//! the [`Embedder`](super::Embedder) seam are separate). Records are deduped by `(source, entity, id)`.

use std::sync::Mutex;

use flux_core::Result;
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput,
};

use super::DatasourceBackend;

/// An in-memory, keyword-ranked datasource index. Cheap and dependency-free; the default backend.
#[derive(Default)]
pub struct MemoryBackend {
    records: Mutex<Vec<Record>>,
}

impl MemoryBackend {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Lowercase whitespace-split query terms (empty if the query is blank).
fn terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Term-frequency score: how many query-term occurrences appear in `title`+`body` (title counts
/// double, a light boost). Returns `(score, matched_fields)`.
fn score(record: &Record, terms: &[String]) -> (usize, Vec<String>) {
    let title = record.title.to_lowercase();
    let body = record.body.to_lowercase();
    let mut score = 0usize;
    let mut matched = Vec::new();
    let title_hits: usize = terms
        .iter()
        .map(|t| title.matches(t.as_str()).count())
        .sum();
    let body_hits: usize = terms.iter().map(|t| body.matches(t.as_str()).count()).sum();
    if title_hits > 0 {
        score += title_hits * 2;
        matched.push("title".to_string());
    }
    if body_hits > 0 {
        score += body_hits;
        matched.push("body".to_string());
    }
    (score, matched)
}

/// A ~160-char snippet around the first matching term in `body` (or its start).
fn snippet(body: &str, terms: &[String]) -> String {
    let lower = body.to_lowercase();
    let byte_pos = terms
        .iter()
        .filter_map(|t| lower.find(t.as_str()))
        .min()
        .unwrap_or(0);
    let pos = lower.get(..byte_pos).map_or(0, |s| s.chars().count());
    let start = pos.saturating_sub(40);
    let take = 160;
    let snip: String = body.chars().skip(start).take(take).collect();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(snip.trim());
    if start + take < body.chars().count() {
        out.push('…');
    }
    out
}

impl DatasourceBackend for MemoryBackend {
    fn upsert(&self, records: &[Record]) -> Result<()> {
        let mut store = self.records.lock().expect("datasource records poisoned");
        for incoming in records {
            let addr = incoming.address();
            match store.iter_mut().find(|r| r.address() == addr) {
                Some(existing) => *existing = incoming.clone(),
                None => store.push(incoming.clone()),
            }
        }
        Ok(())
    }

    fn search(&self, input: &SearchInput) -> Result<Vec<Match>> {
        let terms = terms(&input.query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let limit = input.limit.unwrap_or(5);
        let store = self.records.lock().expect("datasource records poisoned");
        let mut hits: Vec<Match> = store
            .iter()
            .filter(|r| input.source.as_deref().is_none_or(|s| r.source.key() == s))
            .filter(|r| input.entity.as_deref().is_none_or(|e| r.entity == e))
            .filter_map(|r| {
                let (s, matched) = score(r, &terms);
                (s > 0).then(|| {
                    // The stored body stays intact; the snippet is a display aid carried in the match's
                    // record body so the model sees only the relevant window.
                    let mut rec = r.clone();
                    rec.body = snippet(&r.body, &terms);
                    Match {
                        record: rec,
                        score: s as f64,
                        matched_fields: matched,
                    }
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.record.id.cmp(&b.record.id))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    fn get(&self, input: &GetInput) -> Result<Option<Record>> {
        let store = self.records.lock().expect("datasource records poisoned");
        Ok(store
            .iter()
            .find(|r| {
                r.source.key() == input.source && r.entity == input.entity && r.id == input.id
            })
            .cloned())
    }

    fn list(&self, input: &ListInput) -> Result<Vec<Record>> {
        let store = self.records.lock().expect("datasource records poisoned");
        let mut out: Vec<Record> = store
            .iter()
            .filter(|r| r.source.key() == input.source)
            .filter(|r| input.entity.as_deref().is_none_or(|e| r.entity == e))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.entity.cmp(&b.entity).then_with(|| a.id.cmp(&b.id)));
        let off = input.offset.unwrap_or(0);
        let out = out.into_iter().skip(off);
        Ok(match input.limit {
            Some(n) => out.take(n).collect(),
            None => out.collect(),
        })
    }

    fn relation(&self, input: &RelationInput) -> Result<Vec<Record>> {
        let store = self.records.lock().expect("datasource records poisoned");
        let Some(origin) = store.iter().find(|r| {
            r.source.key() == input.source && r.entity == input.entity && r.id == input.id
        }) else {
            return Ok(Vec::new());
        };
        // Resolve each (matching) link to its target record within the same source.
        let links: Vec<_> = origin
            .links
            .iter()
            .filter(|l| input.rel.as_deref().is_none_or(|rel| l.rel == rel))
            .map(|l| (l.target_entity.clone(), l.target_id.clone()))
            .collect();
        Ok(links
            .into_iter()
            .filter_map(|(entity, id)| {
                store
                    .iter()
                    .find(|r| r.source.key() == input.source && r.entity == entity && r.id == id)
                    .cloned()
            })
            .collect())
    }

    fn batch_get(&self, input: &BatchGetInput) -> Result<Vec<Record>> {
        let store = self.records.lock().expect("datasource records poisoned");
        Ok(input
            .ids
            .iter()
            .filter_map(|id| {
                store
                    .iter()
                    .find(|r| {
                        r.source.key() == input.source && r.entity == input.entity && &r.id == id
                    })
                    .cloned()
            })
            .collect())
    }

    fn len(&self) -> usize {
        self.records
            .lock()
            .expect("datasource records poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_datasource::{Link, Source};

    fn doc(id: &str, title: &str, body: &str) -> Record {
        Record::new(Source::new("local"), "file.document", id, title, body)
    }

    fn sample() -> MemoryBackend {
        let b = MemoryBackend::new();
        b.upsert(&[
            doc(
                "a",
                "Agent loop",
                "The agent loop streams tokens and dispatches tools.",
            ),
            doc(
                "b",
                "Permissions",
                "Permission rules gate every tool call before execution.",
            ),
            doc(
                "c",
                "Sessions",
                "Sessions are stored in SQLite and can be resumed.",
            ),
        ])
        .unwrap();
        b
    }

    #[test]
    fn search_ranks_by_term_frequency_and_respects_limit() {
        let b = sample();
        let hits = b
            .search(&SearchInput {
                query: "tool".into(),
                limit: Some(5),
                ..Default::default()
            })
            .unwrap();
        assert!(!hits.is_empty());
        assert!(hits
            .iter()
            .any(|h| h.record.id == "a" || h.record.id == "b"));
        // a non-matching query → empty
        let none = b
            .search(&SearchInput {
                query: "kubernetes".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(none.is_empty());
        // limit honoured
        let one = b
            .search(&SearchInput {
                query: "the and tool sessions".into(),
                limit: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn upsert_dedups_on_address() {
        let b = MemoryBackend::new();
        b.upsert(&[doc("x", "v1", "first")]).unwrap();
        b.upsert(&[doc("x", "v2", "second")]).unwrap();
        assert_eq!(
            b.len(),
            1,
            "same (source, entity, id) replaces, not appends"
        );
        let got = b
            .get(&GetInput {
                source: "local".into(),
                entity: "file.document".into(),
                id: "x".into(),
            })
            .unwrap()
            .unwrap();
        assert_eq!(got.title, "v2");
    }

    #[test]
    fn list_filters_and_pages() {
        let b = sample();
        let all = b
            .list(&ListInput {
                source: "local".into(),
                entity: Some("file.document".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(all.len(), 3);
        let paged = b
            .list(&ListInput {
                source: "local".into(),
                entity: None,
                offset: Some(1),
                limit: Some(1),
            })
            .unwrap();
        assert_eq!(paged.len(), 1);
        assert_eq!(paged[0].id, "b"); // sorted by (entity, id), skip 1
    }

    #[test]
    fn relation_resolves_links_within_a_source() {
        let b = MemoryBackend::new();
        let mut parent = doc("index", "Index", "root");
        parent.links.push(Link {
            rel: "child".into(),
            target_entity: "file.document".into(),
            target_id: "leaf".into(),
        });
        b.upsert(&[parent, doc("leaf", "Leaf", "child doc")])
            .unwrap();
        let kids = b
            .relation(&RelationInput {
                source: "local".into(),
                entity: "file.document".into(),
                id: "index".into(),
                rel: Some("child".into()),
            })
            .unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].id, "leaf");
    }

    #[test]
    fn batch_get_returns_present_ids() {
        let b = sample();
        let got = b
            .batch_get(&BatchGetInput {
                source: "local".into(),
                entity: "file.document".into(),
                ids: vec!["a".into(), "missing".into(), "c".into()],
            })
            .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "a");
        assert_eq!(got[1].id, "c");
    }
}

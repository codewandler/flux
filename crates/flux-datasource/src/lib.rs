//! `flux-datasource` — a minimal in-memory document index with keyword search, exposed to the
//! agent as a `search` tool.
//!
//! v1 is a term-frequency keyword index (no embeddings): documents are scored by how many query
//! terms they contain. The [`SearchTool`] makes an [`Index`] queryable by the model. Pluggable
//! backends (embeddings/RAG, external stores) implement the same shape later.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::ToolSpec;

/// A document in the index.
#[derive(Debug, Clone)]
pub struct Document {
    pub id: String,
    pub text: String,
}

/// A search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub id: String,
    pub score: usize,
    pub snippet: String,
}

/// An in-memory keyword index.
#[derive(Debug, Default, Clone)]
pub struct Index {
    docs: Vec<Document>,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, id: impl Into<String>, text: impl Into<String>) {
        self.docs.push(Document {
            id: id.into(),
            text: text.into(),
        });
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Rank documents by how many query terms they contain (case-insensitive), returning the top
    /// `limit` with a short snippet around the first match.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|t| t.to_string())
            .collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<SearchHit> = self
            .docs
            .iter()
            .filter_map(|d| {
                let lower = d.text.to_lowercase();
                let score: usize = terms
                    .iter()
                    .map(|t| lower.matches(t.as_str()).count())
                    .sum();
                (score > 0).then(|| SearchHit {
                    id: d.id.clone(),
                    score,
                    snippet: snippet(&d.text, &lower, &terms),
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        hits.truncate(limit);
        hits
    }
}

/// Build a ~160-char snippet around the first matching term (or the start of the doc).
fn snippet(text: &str, lower: &str, terms: &[String]) -> String {
    // `find` returns a BYTE offset; convert it to a char index so the char-based windowing below
    // isn't off (or empty) on multibyte text.
    let byte_pos = terms
        .iter()
        .filter_map(|t| lower.find(t.as_str()))
        .min()
        .unwrap_or(0);
    let pos = lower.get(..byte_pos).map_or(0, |s| s.chars().count());
    let start = pos.saturating_sub(40);
    let take = 160;
    let snip: String = text.chars().skip(start).take(take).collect();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(snip.trim());
    if start + take < text.chars().count() {
        out.push('…');
    }
    out
}

/// The `search` tool: query a datasource [`Index`].
pub struct SearchTool {
    index: Arc<Index>,
}

impl SearchTool {
    pub fn new(index: Arc<Index>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "search",
            "Search the indexed datasource for documents matching a keyword query.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "description": "Max results (default 5)"}
                },
                "required": ["query"]
            }),
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("search: `query` required".into()))?;
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let hits = self.index.search(query, limit);
        if hits.is_empty() {
            return Ok(ToolResult::ok("no matches"));
        }
        let body = hits
            .iter()
            .map(|h| format!("[{}] (score {}) {}", h.id, h.score, h.snippet))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolResult::ok(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::{System, Workspace};

    fn sample() -> Index {
        let mut idx = Index::new();
        idx.add("a", "The agent loop streams tokens and dispatches tools.");
        idx.add(
            "b",
            "Permission rules gate every tool call before execution.",
        );
        idx.add("c", "Sessions are stored in SQLite and can be resumed.");
        idx
    }

    #[test]
    fn snippet_handles_multibyte_text() {
        // A match after multibyte chars: the byte offset must be converted to a char index so the
        // window is right (and never panics).
        let text = "café au lait — the agent dispatches tools after the résumé section";
        let lower = text.to_lowercase();
        let snip = snippet(text, &lower, &["dispatches".into()]);
        assert!(snip.contains("dispatches"), "got: {snip}");
    }

    #[test]
    fn ranks_by_term_frequency() {
        let idx = sample();
        let hits = idx.search("tool", 5);
        assert!(!hits.is_empty());
        // doc "b" mentions tool-related terms; ensure a tool doc ranks
        assert!(hits.iter().any(|h| h.id == "b" || h.id == "a"));
        // non-matching query → empty
        assert!(idx.search("kubernetes", 5).is_empty());
    }

    #[test]
    fn limit_is_respected() {
        let idx = sample();
        let hits = idx.search("the and tool sessions", 1);
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn search_tool_returns_hits() {
        let idx = Arc::new(sample());
        let tool = SearchTool::new(idx);
        let dir = std::env::temp_dir().join(format!("flux-ds-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));
        let r = tool
            .execute(&ctx, json!({"query": "sqlite", "limit": 3}))
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("[c]"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

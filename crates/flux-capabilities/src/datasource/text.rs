//! Shared text helpers for the datasource backends: the search-result `snippet` window and the
//! `matched_fields` attribution.
//!
//! Every persistent backend re-fetches the full record for each search hit and runs these two so the
//! emitted [`Match`](flux_datasource::Match) shape is byte-identical across backends — the stored
//! `body` is never touched; only the returned copy carries the snippet. Lifted verbatim from the
//! SQLite backend (the canonical behavior) so both it and the Postgres backend share one definition.

use flux_datasource::Record;

/// Which fields (`title`/`body`) contain any query term (case-insensitive).
pub(super) fn matched_fields(record: &Record, query: &str) -> Vec<String> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let title = record.title.to_lowercase();
    let body = record.body.to_lowercase();
    let mut out = Vec::new();
    if terms.iter().any(|t| title.contains(t.as_str())) {
        out.push("title".to_string());
    }
    if terms.iter().any(|t| body.contains(t.as_str())) {
        out.push("body".to_string());
    }
    out
}

/// A ~160-char snippet around the first matching term in `body` (or its start), UTF-8-safe, with `…`
/// ellipses when the window is clipped on either side.
pub(super) fn snippet(body: &str, query: &str) -> String {
    let lower = body.to_lowercase();
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
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

//! Injected context blocks — knowledge handed to an agent *inline in its system prompt* rather than via a
//! retrieval tool call (story A-19, epic `grounded-knowledge`).
//!
//! A [`ContextBlock`] is a titled chunk of text; [`render_knowledge_blocks`] wraps a slice of them as
//! `<knowledge-base id="…" title="…">…</knowledge-base>` sections, appended after the persona, bounded by a
//! byte budget. Over-budget content is **truncated with a visible marker** — never dropped silently — so
//! the model can tell its grounding was clipped. This is the shared renderer both ends use: an agent
//! surface builds blocks by hand, and `flux-capabilities` turns datasource records into blocks, but both
//! produce identical tag text.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One block of knowledge injected into the system prompt. `id`/`title` become tag attributes; any string
/// entries in `meta` render as extra attributes (e.g. `source="local"`); `body` is the content.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ContextBlock {
    /// A stable identifier for the block (the `id` attribute).
    pub id: String,
    /// A short human title (the `title` attribute; omitted when empty).
    #[serde(default)]
    pub title: String,
    /// Freeform metadata; string-valued keys render as extra tag attributes.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub meta: Value,
    /// The block's text content.
    #[serde(default)]
    pub body: String,
}

impl ContextBlock {
    /// A block from its id, title, and body (no meta).
    pub fn new(id: impl Into<String>, title: impl Into<String>, body: impl Into<String>) -> Self {
        ContextBlock {
            id: id.into(),
            title: title.into(),
            meta: Value::Null,
            body: body.into(),
        }
    }
}

/// Escape a string for use inside a double-quoted XML-ish attribute value.
fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// The opening `<knowledge-base …>` tag for a block (id + title + string `meta` entries as attributes).
fn open_tag(b: &ContextBlock) -> String {
    let mut tag = format!("<knowledge-base id=\"{}\"", attr_escape(&b.id));
    if !b.title.is_empty() {
        tag.push_str(&format!(" title=\"{}\"", attr_escape(&b.title)));
    }
    if let Some(obj) = b.meta.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                tag.push_str(&format!(" {k}=\"{}\"", attr_escape(s)));
            }
        }
    }
    tag.push('>');
    tag
}

const CLOSE: &str = "\n</knowledge-base>";
const TRUNC_MARKER: &str = "\n… [truncated]";

/// Render one block in full: `<knowledge-base …>\n{body}\n</knowledge-base>`.
fn render_one(b: &ContextBlock) -> String {
    format!("{}\n{}{}", open_tag(b), b.body.trim_end(), CLOSE)
}

/// The largest char-boundary prefix of `s` no longer than `max` bytes.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A truncated-but-well-formed rendering of `b` fitting within `avail` bytes (tag stays closed, a
/// `… [truncated]` marker sits before the close). `None` when there isn't room for even the tag scaffolding.
fn render_one_truncated(b: &ContextBlock, avail: usize) -> Option<String> {
    let open = open_tag(b);
    let overhead =
        open.len() + 1 /* the \n after the open tag */ + TRUNC_MARKER.len() + CLOSE.len();
    if avail <= overhead {
        return None;
    }
    let body = truncate_str(b.body.trim_end(), avail - overhead);
    Some(format!("{open}\n{body}{TRUNC_MARKER}{CLOSE}"))
}

/// Render `blocks` as `<knowledge-base>` sections joined by blank lines, bounded by `budget` bytes
/// (`0` = unbounded). Whole blocks are kept while they fit; the first block that overflows is truncated to
/// fit (tag stays closed); any blocks beyond it are omitted. When anything is truncated or omitted a
/// trailing HTML comment records how many blocks were dropped — the clip is always visible.
pub fn render_knowledge_blocks(blocks: &[ContextBlock], budget: usize) -> String {
    if blocks.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut omitted = 0usize;
    for (i, b) in blocks.iter().enumerate() {
        let rendered = render_one(b);
        let sep_len = if out.is_empty() { 0 } else { 2 }; // "\n\n"
        if budget == 0 || out.len() + sep_len + rendered.len() <= budget {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&rendered);
            continue;
        }
        // This block overflows: fit a truncated version if we can, then stop.
        let avail = budget.saturating_sub(out.len() + sep_len);
        match render_one_truncated(b, avail) {
            Some(t) => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&t);
                omitted += blocks.len() - i - 1;
            }
            None => omitted += blocks.len() - i,
        }
        break;
    }
    if omitted > 0 {
        out.push_str(&format!(
            "\n\n<!-- {omitted} knowledge block(s) omitted to fit the context budget -->"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn blocks() -> Vec<ContextBlock> {
        vec![
            ContextBlock::new("hours", "Opening hours", "Mon–Fri 09:00–18:00 CET."),
            ContextBlock::new("refund", "Refunds", "Refunds take 5–7 business days."),
        ]
    }

    #[test]
    fn empty_renders_nothing() {
        assert_eq!(render_knowledge_blocks(&[], 0), "");
    }

    #[test]
    fn blocks_render_in_order_with_tags() {
        let out = render_knowledge_blocks(&blocks(), 0);
        let expected = "<knowledge-base id=\"hours\" title=\"Opening hours\">\n\
             Mon–Fri 09:00–18:00 CET.\n</knowledge-base>\n\n\
             <knowledge-base id=\"refund\" title=\"Refunds\">\n\
             Refunds take 5–7 business days.\n</knowledge-base>";
        assert_eq!(out, expected);
        // order preserved: hours before refund
        assert!(out.find("hours").unwrap() < out.find("refund").unwrap());
    }

    #[test]
    fn meta_string_entries_become_attributes() {
        let mut b = ContextBlock::new("x", "X", "body");
        b.meta = json!({ "source": "local", "n": 3 });
        let out = render_knowledge_blocks(std::slice::from_ref(&b), 0);
        assert!(out.contains("source=\"local\""), "got: {out}");
        assert!(
            !out.contains("n="),
            "non-string meta is not rendered: {out}"
        );
    }

    #[test]
    fn over_budget_truncates_with_a_visible_marker() {
        // A budget that fits the first block but not the second in full.
        let one = render_one(&blocks()[0]).len();
        let out = render_knowledge_blocks(&blocks(), one + 40);
        assert!(out.contains("Opening hours"), "first block kept: {out}");
        assert!(
            out.contains("truncated") || out.contains("omitted"),
            "a visible clip marker is present: {out}"
        );
        // the second block's body is not fully present (it was clipped)
        assert!(
            !out.contains("5–7 business days."),
            "second block clipped: {out}"
        );
    }

    #[test]
    fn attribute_values_are_escaped() {
        let b = ContextBlock::new("a\"b", "T<>&", "body");
        let out = render_knowledge_blocks(std::slice::from_ref(&b), 0);
        assert!(out.contains("id=\"a&quot;b\""), "got: {out}");
        assert!(out.contains("title=\"T&lt;>&amp;\""), "got: {out}");
    }
}

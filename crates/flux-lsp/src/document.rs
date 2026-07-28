//! `document` — the open-document store: text, its **cached** CST, and the line index (L-90).
//!
//! Before L-90 the store held `Url → String` and every handler re-parsed from text: completion,
//! hover, formatting, and semantic tokens each ran `parse_cst` on the whole buffer, some of them
//! twice per request. A [`Document`] now owns its `Parse`, and this module is the **only** place in
//! the crate that parses — pinned by `parsing_is_confined_to_the_document_store`, so a future
//! handler cannot quietly reintroduce a per-request parse.
//!
//! `didChange` takes the incremental path (`flux_lang::parser::reparse`): an edit contained inside a
//! single top-level declaration reparses that declaration and reuses every other declaration's green
//! node. When the edit crosses a declaration boundary the parser declines and we parse in full — the
//! fast path is never allowed to produce a different tree than a full parse would.

use std::collections::HashMap;

use flux_lang::parser::Parse;
use flux_lang::syntax::SyntaxNode;
use text_size::TextRange;
use tokio::sync::RwLock;
use tower_lsp::lsp_types::{Position, TextDocumentContentChangeEvent, Url};

use crate::convert::{change_range, LineIndex};

/// One open document: the client's text, its CST, and the line index over that text.
pub struct Document {
    pub text: String,
    pub parse: Parse,
    pub index: LineIndex,
    /// How many times this buffer has been parsed. Handlers read the cached tree, so a whole
    /// `didChange` → completion → hover cycle costs exactly one — pinned by `one_edit_costs_one_parse`.
    parses: usize,
}

impl Document {
    pub fn new(text: String) -> Self {
        let parse = flux_lang::parser::parse_cst(&text);
        let index = LineIndex::new(&text);
        Document {
            text,
            parse,
            index,
            parses: 1,
        }
    }

    /// The number of parses this document has cost since it was opened.
    pub fn parses(&self) -> usize {
        self.parses
    }

    /// The CST root. Cheap — rowan red nodes are built on demand over the shared green tree.
    pub fn root(&self) -> SyntaxNode {
        self.parse.syntax()
    }

    pub fn offset(&self, pos: Position) -> usize {
        self.index.offset(&self.text, pos)
    }

    /// Apply one `didChange` content change, updating the cached tree incrementally when the edit
    /// stays inside a single declaration.
    pub fn apply(&mut self, change: &TextDocumentContentChangeEvent) {
        let replaced = change_range(&self.text, &self.index, change);
        let mut new_text = self.text.clone();
        new_text.replace_range(replaced.clone(), &change.text);

        let range = TextRange::new(
            (replaced.start as u32).into(),
            (replaced.end as u32).into(),
        );
        self.parse = match flux_lang::parser::reparse(
            &self.parse,
            &self.text,
            &new_text,
            range,
            change.text.len(),
        ) {
            Some(incremental) => incremental,
            None => flux_lang::parser::parse_cst(&new_text),
        };
        self.parses += 1;
        self.index = LineIndex::new(&new_text);
        self.text = new_text;
    }
}

/// Open documents by URI.
#[derive(Default)]
pub struct DocumentStore {
    docs: RwLock<HashMap<Url, Document>>,
}

impl DocumentStore {
    pub async fn open(&self, uri: Url, text: String) {
        self.docs.write().await.insert(uri, Document::new(text));
    }

    pub async fn close(&self, uri: &Url) {
        self.docs.write().await.remove(uri);
    }

    pub async fn change(&self, uri: &Url, changes: &[TextDocumentContentChangeEvent]) {
        let mut docs = self.docs.write().await;
        let doc = docs
            .entry(uri.clone())
            .or_insert_with(|| Document::new(String::new()));
        for change in changes {
            doc.apply(change);
        }
    }

    /// Run `f` against the open document for `uri`, or return `None` when it is not open.
    pub async fn with<T>(&self, uri: &Url, f: impl FnOnce(&Document) -> T) -> Option<T> {
        self.docs.read().await.get(uri).map(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Range;

    fn edit(range: Option<Range>, text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range,
            range_length: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn incremental_edits_reconstruct_the_full_buffer_and_tree() {
        let mut doc = Document::new("flow f\n  $x = 1\n  return $x\n".into());
        doc.apply(&edit(
            Some(Range::new(Position::new(1, 7), Position::new(1, 8))),
            "read(\"a.txt\")",
        ));
        doc.apply(&edit(
            Some(Range::new(Position::new(1, 0), Position::new(1, 0))),
            "  # note\n",
        ));
        let full = "flow f\n  # note\n  $x = read(\"a.txt\")\n  return $x\n";
        assert_eq!(doc.text, full, "incremental edits reconstruct the buffer");
        assert_eq!(
            doc.parse.green,
            flux_lang::parser::parse_cst(full).green,
            "the incrementally maintained tree equals a full reparse"
        );
    }

    #[test]
    fn a_change_without_a_range_replaces_the_whole_document() {
        let mut doc = Document::new("flow f\n  return 1\n".into());
        doc.apply(&edit(None, "flow g\n  return 2\n"));
        assert_eq!(doc.text, "flow g\n  return 2\n");
        assert_eq!(
            doc.parse.green,
            flux_lang::parser::parse_cst(&doc.text).green
        );
    }

    #[test]
    fn one_edit_costs_one_parse() {
        let mut doc = Document::new("flow f\n  $x = 1\n  return $x\n".into());
        assert_eq!(doc.parses(), 1, "opening the document is one parse");
        doc.apply(&edit(
            Some(Range::new(Position::new(1, 7), Position::new(1, 8))),
            "2",
        ));
        // Reading the tree afterwards — as completion and hover do — must be free.
        let _ = doc.root();
        let _ = doc.root();
        assert_eq!(
            doc.parses(),
            2,
            "one edit plus any number of tree reads adds exactly one parse"
        );
    }

    /// The parse cache is only a cache if nothing else parses. Every other module must read
    /// `Document::parse`; this is the L-90 invariant, checked over the crate's own sources.
    #[test]
    fn parsing_is_confined_to_the_document_store() {
        let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(src_dir).expect("src/ is readable") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") || name == "document.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable source");
            // Only the shipping half of each file: test modules legitimately build trees by hand.
            let shipping = text.split("#[cfg(test)]").next().unwrap_or_default();
            for (i, line) in shipping.lines().enumerate() {
                let parses = ["parse_cst(", "Module::parse_str(", "parse_with_ranges("]
                    .iter()
                    .any(|needle| line.contains(needle));
                if parses && !line.trim_start().starts_with("//") {
                    offenders.push(format!("{name}:{}", i + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these handlers parse instead of reading the cached tree: {offenders:?}"
        );
    }
}

//! `convert` — the coordinate system: byte offsets ↔ LSP `Position` (line + UTF-16 column), and the
//! edit application that keeps the server's buffer identical to the client's.

use tower_lsp::lsp_types::{Position, Range, TextDocumentContentChangeEvent};

/// Byte-offset ↔ LSP `Position` via cached line starts.
pub struct LineIndex {
    pub line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex { line_starts }
    }

    pub fn position(&self, text: &str, offset: usize) -> Position {
        let offset = offset.min(text.len());
        let line = self
            .line_starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line];
        let character = text[line_start..offset].encode_utf16().count() as u32;
        Position {
            line: line as u32,
            character,
        }
    }

    /// LSP `Position` → byte offset. The inverse of [`LineIndex::position`], used to resolve a
    /// request cursor and to apply incremental edit ranges. Clamps out-of-range lines/columns to the
    /// end of the line (or the buffer), so a stale client position never panics.
    pub fn offset(&self, text: &str, pos: Position) -> usize {
        let line = pos.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return text.len();
        };
        // The line's *content* end — a character column past it clamps to just before the line
            // break, never onto the next line.
        let mut line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(text.len());
        while line_end > line_start && matches!(text.as_bytes()[line_end - 1], b'\n' | b'\r') {
            line_end -= 1;
        }
        // Walk the line by UTF-16 units until we have consumed `pos.character` of them.
        let mut remaining = pos.character as usize;
        let mut offset = line_start;
        for ch in text[line_start..line_end].chars() {
            if remaining == 0 {
                break;
            }
            let units = ch.len_utf16();
            if units > remaining {
                break;
            }
            remaining -= units;
            offset += ch.len_utf8();
        }
        offset.min(text.len())
    }
}

/// A CST range as an LSP `Range`.
pub fn source_range(range: text_size::TextRange, text: &str, index: &LineIndex) -> Range {
    Range {
        start: index.position(text, u32::from(range.start()) as usize),
        end: index.position(text, u32::from(range.end()) as usize),
    }
}

pub fn whole_document_range(text: &str, index: &LineIndex) -> Range {
    Range {
        start: Position::new(0, 0),
        end: index.position(text, text.len()),
    }
}

/// The byte range one `didChange` content change replaces (the whole buffer when it has no range).
pub fn change_range(text: &str, index: &LineIndex, change: &TextDocumentContentChangeEvent) -> std::ops::Range<usize> {
    match change.range {
        Some(range) => {
            let start = index.offset(text, range.start);
            let end = index.offset(text, range.end).max(start);
            start..end
        }
        None => 0..text.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_and_offset_round_trip_through_utf16() {
        // `é` is one UTF-16 unit but two UTF-8 bytes; an emoji is two units and four bytes.
        let text = "flow f\n  $x = \"é🙂\"\n";
        let index = LineIndex::new(text);
        for offset in 0..=text.len() {
            if !text.is_char_boundary(offset) {
                continue;
            }
            let pos = index.position(text, offset);
            assert_eq!(index.offset(text, pos), offset, "round trip at {offset}");
        }
    }

    #[test]
    fn offset_clamps_a_stale_client_position() {
        let text = "flow f\n";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(99, 0)), text.len());
        assert_eq!(index.offset(text, Position::new(0, 99)), 6);
    }

    #[test]
    fn change_range_covers_the_whole_buffer_without_a_range() {
        let text = "flow f\n  return 1\n";
        let index = LineIndex::new(text);
        let change = TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "flow g\n".into(),
        };
        assert_eq!(change_range(text, &index, &change), 0..text.len());
    }
}

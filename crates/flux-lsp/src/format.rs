//! `format` — whole-document and range formatting (L-88).
//!
//! The formatting *policy* lives in `flux_lang::format_cst`, one layer down, so `flux fmt` and the
//! editor cannot drift apart. This module is the LSP adapter: cached tree in, `TextEdit`s out.
//!
//! What changed in round 2: formatting used to return no edit at all for any multi-declaration
//! module (a `Program` groups declarations by kind, so re-rendering could reorder the author's file)
//! and downgraded a *commented* flow to an indentation-only re-indent. Both restrictions were
//! artefacts of formatting from the AST. Formatting from the CST makes declaration order and
//! comments structural, so both cases now format properly — and `rangeFormatting` exists at all.

use flux_lang::parser::Parse;
use tower_lsp::lsp_types::{Range, TextEdit};

use crate::convert::{source_range, whole_document_range, LineIndex};

/// The edit that formats the whole document, or `None` when it is already canonical or the
/// equivalence guard declined.
pub fn format_document(parse: &Parse, text: &str, index: &LineIndex) -> Option<Vec<TextEdit>> {
    let formatted = flux_lang::format_cst::format_module(parse)?;
    if formatted == text {
        return None;
    }
    Some(vec![TextEdit {
        range: whole_document_range(text, index),
        new_text: formatted,
    }])
}

/// The edit that formats the lines covered by `range`.
pub fn format_selection(
    parse: &Parse,
    text: &str,
    index: &LineIndex,
    range: Range,
) -> Option<Vec<TextEdit>> {
    let start = index.offset(text, range.start);
    let end = index.offset(text, range.end).max(start);
    let (replaced, new_text) = flux_lang::format_cst::format_range(parse, text, start..end)?;
    let replaced = text_size::TextRange::new(
        (replaced.start as u32).into(),
        (replaced.end as u32).into(),
    );
    Some(vec![TextEdit {
        range: source_range(replaced, text, index),
        new_text,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Position;

    fn format(src: &str) -> Option<String> {
        let parse = flux_lang::parser::parse_cst(src);
        let index = LineIndex::new(src);
        format_document(&parse, src, &index).map(|edits| edits[0].new_text.clone())
    }

    #[test]
    fn a_commented_flow_formats_canonically_and_keeps_every_comment() {
        let src = "flow f\n    # a leading note\n    $x   =   1  # trailing\n    return   $x\n";
        let formatted = format(src).expect("formats");
        assert_eq!(
            formatted,
            "flow f\n  # a leading note\n  $x = 1  # trailing\n  return $x\n",
            "canonical spacing *and* every comment"
        );
    }

    #[test]
    fn a_module_formats_and_keeps_its_declaration_order() {
        // This replaces `formatting_is_deliberately_disabled_for_modules` (L-70): a module used to
        // return no edit because `Program` could not reproduce source order. The CST can.
        let src = "op one() -> String\n    return \"1\"\n\nflow mid\n    return one()\n\nop two() -> String\n    return \"2\"\n";
        let formatted = format(src).expect("a module formats");
        let heads: Vec<&str> = formatted
            .lines()
            .filter(|l| l.starts_with("op ") || l.starts_with("flow "))
            .collect();
        assert_eq!(
            heads,
            vec!["op one() -> String", "flow mid", "op two() -> String"],
            "source declaration order is preserved"
        );
        assert!(formatted.contains("\n  return \"1\"\n"), "bodies re-indented");
    }

    #[test]
    fn a_comment_free_flow_still_reaches_the_canonical_formatter() {
        let src = "flow f\n    $x = 1\n    return $x\n";
        assert_eq!(format(src).expect("formats"), "flow f\n  $x = 1\n  return $x\n");
    }

    #[test]
    fn an_already_canonical_buffer_produces_no_edit() {
        assert_eq!(format("flow f\n  $x = 1\n  return $x\n"), None);
    }

    #[test]
    fn a_buffer_with_parse_errors_produces_no_edit() {
        assert_eq!(format("flow f\n  $a =\n"), None);
    }

    #[test]
    fn range_formatting_edits_only_the_selected_lines() {
        let src = "flow f\n  $x = 1\n  $y   =   read( \"a.txt\" )\n  return $x\n";
        let parse = flux_lang::parser::parse_cst(src);
        let index = LineIndex::new(src);
        let edits = format_selection(
            &parse,
            src,
            &index,
            Range::new(Position::new(2, 2), Position::new(2, 4)),
        )
        .expect("formats the selection");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start, Position::new(2, 0));
        assert_eq!(edits[0].range.end, Position::new(3, 0));
        assert_eq!(edits[0].new_text, "  $y = read(\"a.txt\")\n");
    }
}

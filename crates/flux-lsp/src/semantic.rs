//! `semantic` — semantic tokens (L-69), now with `range` and `full/delta` support (L-90).
//!
//! The CST token stream lifted to the LSP legend, enriched with the distinctions a grammar cannot
//! make: a registry-known op vs an unknown identifier, a `$var` bind site vs a use.
//!
//! Round 2 closed the capability gap. `initialize` used to advertise `range: Some(false)` and no
//! delta while the handler was full-document only, so every keystroke re-serialized the whole token
//! stream. Both are now implemented, and the advertisement matches — that pairing is what the L-91
//! protocol harness checks.

use std::collections::HashSet;

use flux_lang::highlight::{highlight, HighlightClass};
use flux_lang::syntax::SyntaxNode;
use text_size::TextRange;
use tower_lsp::lsp_types::{
    Range, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensEdit, SemanticTokensLegend,
};

use crate::convert::LineIndex;
use crate::scope::{all_var_defs, collect_declarations, DefRole};

// Legend indices — must match the order in `legend`.
pub const TOK_KEYWORD: u32 = 0;
pub const TOK_FUNCTION: u32 = 1;
pub const TOK_VARIABLE: u32 = 2;
pub const TOK_PARAMETER: u32 = 3;
pub const TOK_TYPE: u32 = 4;
pub const TOK_STRING: u32 = 5;
pub const TOK_NUMBER: u32 = 6;
pub const TOK_COMMENT: u32 = 7;
pub const TOK_DECORATOR: u32 = 8;

// Modifier bits — must match the order in `legend`.
pub const MOD_DECLARATION: u32 = 1 << 0;
pub const MOD_DEFINITION: u32 = 1 << 1;
pub const MOD_DEFAULT_LIBRARY: u32 = 1 << 2;

/// The legend advertised in `initialize` and used to decode the token stream.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::KEYWORD,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::TYPE,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::DECORATOR,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::DEFINITION,
            SemanticTokenModifier::DEFAULT_LIBRARY,
        ],
    }
}

/// NAME nodes in op-call position (`op(args)`, `do op`, `fmt`/`parse`) whose full (possibly dotted)
/// text is a registry-known op — the ranges that earn the `defaultLibrary` modifier.
fn known_op_ranges(root: &SyntaxNode, known: &HashSet<String>) -> Vec<TextRange> {
    use flux_lang::syntax::SyntaxKind;
    root.descendants()
        .filter(|n| n.kind() == SyntaxKind::NAME)
        .filter(|name| {
            matches!(
                name.parent().map(|p| p.kind()),
                Some(
                    SyntaxKind::CALL_EXPR
                        | SyntaxKind::CALL_STMT
                        | SyntaxKind::FMT_EXPR
                        | SyntaxKind::PARSE_EXPR
                )
            ) && known.contains(&name.text().to_string())
        })
        .map(|name| name.text_range())
        .collect()
}

/// Semantic tokens for the whole document, or — when `lines` is given — only for that line span.
pub fn semantic_tokens(
    root: &SyntaxNode,
    text: &str,
    index: &LineIndex,
    known: &HashSet<String>,
    lines: Option<std::ops::Range<u32>>,
) -> Vec<SemanticToken> {
    let defs = all_var_defs(root);
    let def_ranges: HashSet<TextRange> = defs.iter().map(|d| d.name_range).collect();
    let param_ranges: HashSet<TextRange> = defs
        .iter()
        .filter(|d| d.role == DefRole::Param)
        .map(|d| d.name_range)
        .collect();
    let decl_name_ranges: HashSet<TextRange> = collect_declarations(root)
        .iter()
        .map(|(d, _)| d.name_range)
        .collect();
    let op_ranges = known_op_ranges(root, known);

    // (line, start_char, len, token_type, modifiers), source order (highlight is already ordered).
    let mut raw: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
    for (range, class) in highlight(text) {
        let Some((ty, modifiers)) = classify(
            class,
            range,
            &def_ranges,
            &param_ranges,
            &decl_name_ranges,
            &op_ranges,
        ) else {
            continue;
        };
        push_token_spans(range, ty, modifiers, text, index, &mut raw);
    }
    raw.sort_by_key(|t| (t.0, t.1));
    if let Some(lines) = &lines {
        raw.retain(|t| lines.contains(&t.0));
    }

    let mut data = Vec::with_capacity(raw.len());
    let (mut prev_line, mut prev_char) = (0u32, 0u32);
    for (line, ch, len, ty, modifiers) in raw {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 { ch - prev_char } else { ch };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: ty,
            token_modifiers_bitset: modifiers,
        });
        prev_line = line;
        prev_char = ch;
    }
    data
}

/// The line span an LSP `Range` covers, end-inclusive (a token starting on the last line counts).
pub fn line_span(range: Range) -> std::ops::Range<u32> {
    range.start.line..range.end.line.saturating_add(1)
}

/// The minimal single-splice delta between two token streams: drop the common prefix and suffix and
/// send what is left. A one-character edit therefore ships a handful of tokens instead of the whole
/// document.
pub fn delta(previous: &[SemanticToken], current: &[SemanticToken]) -> Vec<SemanticTokensEdit> {
    let prefix = previous
        .iter()
        .zip(current.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = previous.len().min(current.len()) - prefix;
    let suffix = (0..max_suffix)
        .take_while(|i| previous[previous.len() - 1 - i] == current[current.len() - 1 - i])
        .count();
    if prefix == previous.len() && previous.len() == current.len() {
        return Vec::new();
    }
    // The wire encoding counts in u32s, and every token is five of them.
    let start = prefix * 5;
    let delete_count = (previous.len() - prefix - suffix) * 5;
    let data = current[prefix..current.len() - suffix].to_vec();
    vec![SemanticTokensEdit {
        start: start as u32,
        delete_count: delete_count as u32,
        data: Some(data),
    }]
}

pub fn tokens(result_id: String, data: Vec<SemanticToken>) -> SemanticTokens {
    SemanticTokens {
        result_id: Some(result_id),
        data,
    }
}

/// Map one highlight class + its range to a legend token type and modifier bitset, or `None` to
/// skip (punctuation/operators are left to the grammar).
fn classify(
    class: HighlightClass,
    range: TextRange,
    def_ranges: &HashSet<TextRange>,
    param_ranges: &HashSet<TextRange>,
    decl_name_ranges: &HashSet<TextRange>,
    op_ranges: &[TextRange],
) -> Option<(u32, u32)> {
    let token = match class {
        HighlightClass::Keyword => (TOK_KEYWORD, 0),
        HighlightClass::Op => {
            let mut modifiers = 0;
            if op_ranges.iter().any(|r| r.contains_range(range)) {
                modifiers |= MOD_DEFAULT_LIBRARY;
            }
            if decl_name_ranges.contains(&range) {
                modifiers |= MOD_DECLARATION;
            }
            (TOK_FUNCTION, modifiers)
        }
        HighlightClass::Var => {
            let ty = if param_ranges.contains(&range) {
                TOK_PARAMETER
            } else {
                TOK_VARIABLE
            };
            let modifiers = if def_ranges.contains(&range) {
                MOD_DEFINITION
            } else {
                0
            };
            (ty, modifiers)
        }
        HighlightClass::Annotation => (TOK_DECORATOR, 0),
        HighlightClass::String => (TOK_STRING, 0),
        HighlightClass::Number => (TOK_NUMBER, 0),
        HighlightClass::Comment => (TOK_COMMENT, 0),
        HighlightClass::Type => (TOK_TYPE, 0),
        // Punctuation, operators, and error tokens carry no semantic colour of their own.
        HighlightClass::Punct | HighlightClass::Error => return None,
    };
    Some(token)
}

/// Push one source span as one or more single-line semantic tokens (the LSP encoding cannot express
/// a token that crosses a line, so a multi-line `"""…"""` string is split per line).
fn push_token_spans(
    range: TextRange,
    ty: u32,
    modifiers: u32,
    text: &str,
    index: &LineIndex,
    out: &mut Vec<(u32, u32, u32, u32, u32)>,
) {
    let start = index.position(text, range.start().into());
    let end = index.position(text, range.end().into());
    if start.line == end.line {
        let len = end.character - start.character;
        if len > 0 {
            out.push((start.line, start.character, len, ty, modifiers));
        }
        return;
    }
    let start_byte: usize = range.start().into();
    let end_byte: usize = range.end().into();
    for line in start.line..=end.line {
        let content_start = if line == start.line {
            start_byte
        } else {
            index.line_starts[line as usize]
        };
        let mut content_end = if line == end.line {
            end_byte
        } else {
            index
                .line_starts
                .get(line as usize + 1)
                .copied()
                .unwrap_or(text.len())
        };
        // Exclude the trailing line break from non-final lines.
        while content_end > content_start
            && matches!(text.as_bytes().get(content_end - 1), Some(b'\n' | b'\r'))
        {
            content_end -= 1;
        }
        let start_char = index.position(text, content_start).character;
        let len = text[content_start..content_end].encode_utf16().count() as u32;
        if len > 0 {
            out.push((line, start_char, len, ty, modifiers));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::authoring_op_signatures;
    use tower_lsp::lsp_types::Position;

    fn tokens_for(src: &str, lines: Option<std::ops::Range<u32>>) -> Vec<SemanticToken> {
        let known: HashSet<String> = authoring_op_signatures()
            .into_iter()
            .map(|op| op.name)
            .collect();
        let root = flux_lang::parser::parse_cst(src).syntax();
        semantic_tokens(&root, src, &LineIndex::new(src), &known, lines)
    }

    /// Decode the delta-encoded token stream back to `(text, type, modifiers)` (ASCII input only).
    fn decode(src: &str, data: &[SemanticToken]) -> Vec<(String, u32, u32)> {
        let lines: Vec<&str> = src.split('\n').collect();
        let (mut line, mut ch) = (0u32, 0u32);
        let mut out = Vec::new();
        for t in data {
            if t.delta_line != 0 {
                line += t.delta_line;
                ch = t.delta_start;
            } else {
                ch += t.delta_start;
            }
            let text: String = lines[line as usize]
                .chars()
                .skip(ch as usize)
                .take(t.length as usize)
                .collect();
            out.push((text, t.token_type, t.token_modifiers_bitset));
        }
        out
    }

    #[test]
    fn semantic_tokens_distinguish_known_op_from_unknown_and_bind_from_use() {
        let src =
            "flow f\n  # a note\n  $x = read(\"a.txt\")\n  $y = made_up(\"z\")\n  return $x\n";
        let decoded = decode(src, &tokens_for(src, None));
        let find = |t: &str| {
            decoded
                .iter()
                .find(|(text, _, _)| text == t)
                .unwrap_or_else(|| panic!("no token {t:?} in {decoded:?}"))
        };
        assert_eq!(find("flow").1, TOK_KEYWORD);
        assert_eq!(find("return").1, TOK_KEYWORD);
        assert_eq!(find("\"a.txt\"").1, TOK_STRING);
        assert!(decoded
            .iter()
            .any(|(t, ty, _)| t.contains("a note") && *ty == TOK_COMMENT));
        let read = find("read");
        assert_eq!(read.1, TOK_FUNCTION);
        assert_ne!(
            read.2 & MOD_DEFAULT_LIBRARY,
            0,
            "known op is defaultLibrary"
        );
        let made_up = find("made_up");
        assert_eq!(made_up.2 & MOD_DEFAULT_LIBRARY, 0, "unknown op is plain");
        let bind = find("$x");
        assert_eq!(bind.1, TOK_VARIABLE);
        assert_ne!(bind.2 & MOD_DEFINITION, 0, "bind site is a definition");
        let uses: Vec<_> = decoded.iter().filter(|(t, _, _)| t == "$x").collect();
        assert!(uses.iter().any(|(_, _, m)| m & MOD_DEFINITION == 0));
    }

    #[test]
    fn a_range_request_returns_only_that_line_span() {
        let src = "flow f\n  $x = read(\"a.txt\")\n  $y = 2\n  return $x\n";
        let span = line_span(Range::new(Position::new(2, 0), Position::new(2, 8)));
        let decoded = decode(src, &tokens_for(src, Some(span)));
        let texts: Vec<&str> = decoded.iter().map(|(t, _, _)| t.as_str()).collect();
        assert!(
            texts.contains(&"$y"),
            "the selected line is present: {texts:?}"
        );
        assert!(
            !texts.contains(&"read"),
            "other lines are excluded: {texts:?}"
        );
        assert!(
            !texts.contains(&"flow"),
            "other lines are excluded: {texts:?}"
        );
    }

    #[test]
    fn a_delta_ships_only_the_changed_span() {
        let before = tokens_for("flow f\n  $x = 1\n  return $x\n", None);
        let after = tokens_for("flow f\n  $x = 22\n  return $x\n", None);
        let edits = delta(&before, &after);
        assert_eq!(edits.len(), 1, "one contiguous splice");
        assert!(
            edits[0]
                .data
                .as_ref()
                .is_some_and(|d| d.len() < after.len()),
            "the delta is smaller than the full stream"
        );
    }

    #[test]
    fn an_unchanged_document_produces_no_delta_edits() {
        let tokens = tokens_for("flow f\n  return 1\n", None);
        assert!(delta(&tokens, &tokens).is_empty());
    }
}

//! `format_cst` — the **order- and comment-preserving** canonical formatter, driven by the lossless
//! CST rather than by a re-rendered [`crate::ast::DraftAst`] (L-88).
//!
//! [`crate::format`] projects a *semantic* `DraftAst` back to text. That is exactly right for a
//! model-authored plan, and exactly wrong for a file a human is editing: the AST has dropped the
//! comments, and a [`crate::program::Program`] groups declarations by kind, so re-rendering a module
//! would reorder the author's file. This module formats the tree the parser actually built, so
//! comments and declaration order are *structural facts* rather than things to work around.
//!
//! # What it does
//! - **Indentation** is recomputed from `BLOCK` nesting depth (two spaces per level). A comment-only
//!   line takes the indentation of the statement it precedes, since comments attach above their
//!   block in the CST and their own depth would under-indent them.
//! - **Interior spacing** is recomputed from a token-pair rule table: no space before `,` `:` `)`
//!   `]` `?` `.`, none after `(` `[` `.`, none between a name and its `(`, one space around the
//!   unambiguous binary/assignment operators, `{ … }` padded. The operators whose spelling is
//!   genuinely ambiguous (`-` as unary/kebab-name vs subtraction, `<`/`>` as comparison vs type
//!   arguments) **preserve the author's adjacency** — a formatter that guesses there would corrupt
//!   `List<String>` or `god-review`.
//! - **Blank lines** are preserved as separators but a run of them collapses to one.
//! - **Multi-line strings** (`"""…"""`) are single tokens and are emitted verbatim; nothing is
//!   injected into their interior.
//!
//! # The guard
//! [`format_module`] never returns text it cannot prove equivalent: the formatted buffer must
//! reparse without errors, lower to the *same* [`crate::program::Module`], and carry the same
//! comment multiset. Otherwise it returns `None` and the caller leaves the buffer alone. Formatting
//! is also idempotent — `format(format(x)) == format(x)` — which the corpus test pins.

use crate::parser::Parse;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Format `src` canonically, or `None` if it does not parse cleanly, is already canonical, or the
/// equivalence guard rejects the result.
pub fn format_source(src: &str) -> Option<String> {
    let parsed = crate::parser::parse_cst(src);
    let formatted = format_module(&parsed)?;
    (formatted != src).then_some(formatted)
}

/// Format an already-built CST. Returns `None` when the tree has parse errors or the formatted text
/// fails the reparse-equivalence guard.
pub fn format_module(parsed: &Parse) -> Option<String> {
    if !parsed.errors.is_empty() {
        return None;
    }
    let root = parsed.syntax();
    let original = crate::lower_cst::cst_to_module(parsed).ok()?.module;
    let formatted = print(&root);

    // Equivalence guard: reparse clean, lower to the same module, keep every comment.
    let reparsed = crate::parser::parse_cst(&formatted);
    if !reparsed.errors.is_empty() {
        return None;
    }
    if crate::lower_cst::cst_to_module(&reparsed).ok()?.module != original {
        return None;
    }
    if comment_multiset(&reparsed.syntax()) != comment_multiset(&root) {
        return None;
    }
    Some(formatted)
}

/// Format only the source lines covered by `range`, returning the byte range to replace and its
/// replacement. The selection is widened to whole lines — a formatter that rewrote half a line would
/// have to guess what the other half meant.
///
/// Indentation is canonicalized for the selected lines like anywhere else — which means a selection
/// whose *surroundings* are indented differently would leave the buffer with inconsistent layout.
/// The equivalence guard catches exactly that: it is evaluated against the **spliced** document, so
/// a range format that would not reparse to the same module simply returns `None` and the buffer is
/// left alone. Formatting the whole document is the way to fix a file with ragged indentation.
pub fn format_range(
    parsed: &Parse,
    text: &str,
    range: std::ops::Range<usize>,
) -> Option<(std::ops::Range<usize>, String)> {
    if !parsed.errors.is_empty() {
        return None;
    }
    let root = parsed.syntax();
    let original = crate::lower_cst::cst_to_module(parsed).ok()?.module;

    let start = text[..range.start.min(text.len())]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    let end = text[range.end.min(text.len())..]
        .find('\n')
        .map_or(text.len(), |i| range.end + i + 1);
    if start >= end {
        return None;
    }

    let all = lines(&root);
    let selected: Vec<&Vec<Piece>> = all
        .iter()
        .filter(|line| {
            line.first().is_some_and(|p| {
                let at = usize::from(p.token.text_range().start());
                at >= start && at < end
            })
        })
        .collect();
    if selected.is_empty() {
        return None;
    }

    let mut printed = String::new();
    for line in &selected {
        for _ in 0..line_depth(&all, index_of(&all, line)) {
            printed.push_str("  ");
        }
        printed.push_str(&print_line(line));
        printed.push('\n');
    }
    if printed == text[start..end] {
        return None;
    }

    let mut spliced = String::with_capacity(text.len());
    spliced.push_str(&text[..start]);
    spliced.push_str(&printed);
    spliced.push_str(&text[end..]);
    let reparsed = crate::parser::parse_cst(&spliced);
    if !reparsed.errors.is_empty()
        || crate::lower_cst::cst_to_module(&reparsed).ok()?.module != original
        || comment_multiset(&reparsed.syntax()) != comment_multiset(&root)
    {
        return None;
    }
    Some((start..end, printed))
}

/// The position of `line` within `all` (identity, not equality — lines carry CST tokens).
fn index_of(all: &[Vec<Piece>], line: &Vec<Piece>) -> usize {
    all.iter()
        .position(|candidate| std::ptr::eq(candidate, line))
        .expect("the line came from `all`")
}

/// The sorted comment texts of a tree — the fingerprint proving a reformat kept every comment.
pub fn comment_multiset(root: &SyntaxNode) -> Vec<String> {
    let mut comments: Vec<String> = root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .filter(|t| t.kind() == SyntaxKind::COMMENT)
        .map(|t| t.text().trim_end().to_string())
        .collect();
    comments.sort();
    comments
}

/// One printable token: the CST token plus whether the author wrote whitespace before it (the
/// tiebreaker for the ambiguous operators).
struct Piece {
    token: SyntaxToken,
    spaced_in_source: bool,
}

/// The BLOCK-nesting depth of a token — its canonical indentation level.
fn block_depth(tok: &SyntaxToken) -> usize {
    tok.parent()
        .into_iter()
        .flat_map(|p| p.ancestors())
        .filter(|n| n.kind() == SyntaxKind::BLOCK)
        .count()
}

/// Split the token stream into logical lines, dropping the whitespace the printer re-derives.
fn lines(root: &SyntaxNode) -> Vec<Vec<Piece>> {
    let mut out: Vec<Vec<Piece>> = Vec::new();
    let mut current: Vec<Piece> = Vec::new();
    let mut spaced = false;
    for token in root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        match token.kind() {
            SyntaxKind::WHITESPACE => spaced = true,
            SyntaxKind::NEWLINE => {
                out.push(std::mem::take(&mut current));
                spaced = false;
            }
            kind if kind.is_layout() || token.text().is_empty() => {}
            _ => {
                current.push(Piece {
                    token,
                    spaced_in_source: spaced,
                });
                spaced = false;
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn print(root: &SyntaxNode) -> String {
    let lines = lines(root);
    let mut out = String::new();
    let mut pending_blank = false;
    let mut started = false;
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            // A run of blank lines collapses to one, and leading blank lines are dropped.
            pending_blank = started;
            continue;
        }
        if pending_blank {
            out.push('\n');
            pending_blank = false;
        }
        let depth = line_depth(&lines, i);
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push_str(&print_line(line));
        out.push('\n');
        started = true;
    }
    out
}

/// The indentation depth of a line. A comment-only line aligns with the next real statement. At
/// end of input there is no following statement to reveal its scope, so an explicitly column-zero
/// comment remains module-level even when the tolerant CST attached it to the last declaration.
fn line_depth(lines: &[Vec<Piece>], i: usize) -> usize {
    let line = &lines[i];
    let first = &line[0];
    if first.token.kind() == SyntaxKind::COMMENT && line.len() == 1 {
        if let Some(next) = lines[i + 1..]
            .iter()
            .flatten()
            .find(|p| p.token.kind() != SyntaxKind::COMMENT)
        {
            return block_depth(&next.token);
        }
        if !first.spaced_in_source {
            return 0;
        }
    }
    block_depth(&first.token)
}

fn print_line(line: &[Piece]) -> String {
    let mut out = String::new();
    for (i, piece) in line.iter().enumerate() {
        if i > 0 {
            let prev = &line[i - 1];
            if piece.token.kind() == SyntaxKind::COMMENT {
                // A trailing comment is set off from the code by two spaces.
                out.push_str("  ");
            } else if wants_space(&prev.token, &piece.token, piece.spaced_in_source) {
                out.push(' ');
            }
        }
        out.push_str(piece.token.text());
    }
    out
}

/// Is the token an identifier in *name* position (an op name, a declaration name, a key, a type)
/// rather than a statement keyword? Only a name binds tightly to a following `(` or `[`.
fn is_name(tok: &SyntaxToken) -> bool {
    matches!(
        tok.parent().map(|p| p.kind()),
        Some(SyntaxKind::NAME | SyntaxKind::FLOW_HEADER | SyntaxKind::OP_HEADER)
    )
}

/// Should a space separate `prev` and `cur`? `spaced_in_source` is the author's own answer, used
/// only for the operators whose spelling is ambiguous.
fn wants_space(prev: &SyntaxToken, cur: &SyntaxToken, spaced_in_source: bool) -> bool {
    use SyntaxKind::*;
    let (p, c) = (prev.kind(), cur.kind());

    // Ambiguous by design: `-` is subtraction, negation, and the kebab joiner in `god-review`;
    // `<`/`>` are comparison and type-argument brackets. Keep what the author wrote.
    if matches!(p, MINUS | LT | GT | STAR | SLASH | PLUS)
        || matches!(c, MINUS | LT | GT | STAR | SLASH | PLUS)
    {
        return spaced_in_source;
    }

    // A duration literal is two tokens the lexer never joins — `500ms` is NUMBER + IDENT, and so is
    // the (meaningless) `500 ms`. Inserting a space would change `delay: 500ms` into something the
    // header decoder rejects, so the author's own adjacency decides here too.
    if p == NUMBER && c == IDENT {
        return spaced_in_source;
    }

    // Never a space before a closer, a separator, or a postfix marker.
    if matches!(c, R_PAREN | R_BRACK | COMMA | COLON | QUESTION | DOT) {
        return false;
    }
    // Never a space after an opener or a path dot.
    if matches!(p, L_PAREN | L_BRACK | DOT | BANG) {
        return false;
    }
    // `{ k: v }` is padded, `{}` is not.
    if p == L_BRACE {
        return c != R_BRACE;
    }
    if c == R_BRACE {
        return p != L_BRACE;
    }
    // A call/parameter list binds to its name; a parenthesized condition after a statement keyword
    // (`when (a && b)`) does not. An identifier the CST did not put in name position is ambiguous
    // between the two, so the author's own adjacency decides.
    if c == L_PAREN {
        if is_name(prev) || matches!(p, R_PAREN | R_BRACK | ANNOTATION) {
            return false;
        }
        return if p == IDENT { spaced_in_source } else { true };
    }
    if c == L_BRACK {
        return !is_name(prev);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_interior_spacing_and_indentation() {
        let src = "flow f\n    $x   =    read( \"a.txt\" )\n    return   $x\n";
        let formatted = format_source(src).expect("formats");
        assert_eq!(formatted, "flow f\n  $x = read(\"a.txt\")\n  return $x\n");
    }

    #[test]
    fn keeps_every_comment_attached_to_its_construct() {
        let src = "flow f\n    # a leading note\n    $x   =   1  # trailing\n    return $x\n";
        let formatted = format_source(src).expect("formats");
        assert_eq!(
            formatted,
            "flow f\n  # a leading note\n  $x = 1  # trailing\n  return $x\n"
        );
    }

    #[test]
    fn preserves_comments_at_module_and_statement_boundaries() {
        let src = "# before the first declaration\nflow first\n    $x   =   1 # trailing on a statement\n    return $x\n# between declarations\nflow second\n    return \"ok\"\n# after the last declaration\n";
        let formatted = format_source(src).expect("formats");
        assert_eq!(
            formatted,
            "# before the first declaration\nflow first\n  $x = 1  # trailing on a statement\n  return $x\n# between declarations\nflow second\n  return \"ok\"\n# after the last declaration\n",
            "comment columns and the constructs they document are part of formatting correctness"
        );
    }

    #[test]
    fn duration_suffixes_stay_attached_to_their_number() {
        // `500ms` is NUMBER + IDENT; splitting it makes the header undecodable, so the CST
        // formatter must leave the pair tight (and must not join a genuinely separated pair).
        let src = "flow f\n  retry 3, backoff: exponential, delay: 500ms -> out\n    flaky()\n  return out\n";
        assert_eq!(
            format_source(src),
            None,
            "a canonical duration header is already canonical"
        );
        let ragged = "flow f\n  timeout   5s\n    slow()\n  return \"ok\"\n";
        assert_eq!(
            format_source(ragged).as_deref(),
            Some("flow f\n  timeout 5s\n    slow()\n  return \"ok\"\n")
        );
    }

    #[test]
    fn preserves_module_declaration_order() {
        // `Program` groups declarations by kind; the CST does not — op/flow/op stays op/flow/op.
        let src = "op one() -> String\n  return \"1\"\n\nflow mid\n  return one()\n\nop two() -> String\n  return \"2\"\n";
        let formatted = format_module(&crate::parser::parse_cst(src)).expect("formats");
        let order: Vec<&str> = formatted
            .lines()
            .filter(|l| l.starts_with("op ") || l.starts_with("flow "))
            .collect();
        assert_eq!(
            order,
            vec!["op one() -> String", "flow mid", "op two() -> String"]
        );
    }

    #[test]
    fn preserves_ambiguous_operator_adjacency() {
        // Kebab-case declaration names and type arguments must not be pulled apart.
        let src = "flow god-review(items: List<String>)\n  return $items\n";
        let formatted = format_module(&crate::parser::parse_cst(src)).expect("formats");
        assert!(
            formatted.contains("god-review(items: List<String>)"),
            "kebab name and type arguments stay tight: {formatted:?}"
        );
    }

    #[test]
    fn is_idempotent_over_the_shipped_examples() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).expect("examples/ exists") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("flux") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("readable example");
            let parsed = crate::parser::parse_cst(&src);
            if !parsed.errors.is_empty() {
                continue;
            }
            let Some(once) = format_module(&parsed) else {
                continue;
            };
            let twice = format_module(&crate::parser::parse_cst(&once))
                .expect("a formatted buffer still formats");
            assert_eq!(once, twice, "formatting is not idempotent for {path:?}");
            checked += 1;
        }
        assert!(checked > 0, "no example formatted — corpus test is vacuous");
    }

    #[test]
    fn range_formatting_touches_only_the_selected_lines() {
        let src = "flow f\n  $x = 1\n  $y   =   read( \"a.txt\" )\n  return $x\n";
        let parsed = crate::parser::parse_cst(src);
        let start = src.find("$y").unwrap();
        let (range, printed) = format_range(&parsed, src, start..start + 2).expect("formats");
        assert_eq!(&src[range.clone()], "  $y   =   read( \"a.txt\" )\n");
        assert_eq!(printed, "  $y = read(\"a.txt\")\n");
    }

    #[test]
    fn range_formatting_widens_a_partial_selection_to_whole_lines() {
        let src = "flow f\n  $x   =   1\n  return $x\n";
        let parsed = crate::parser::parse_cst(src);
        let mid = src.find("=   1").unwrap();
        let (range, printed) = format_range(&parsed, src, mid..mid + 1).expect("formats");
        assert_eq!(&src[range], "  $x   =   1\n");
        assert_eq!(printed, "  $x = 1\n");
    }
}

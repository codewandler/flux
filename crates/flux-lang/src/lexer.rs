//! `lexer` — the lossless, layout-aware lexer, stage one of the CST front-end.
//!
//! Turns Flux-Lang source into a flat token stream where **concatenating every token's text
//! reproduces the source byte-for-byte** — comments and line breaks are kept as trivia, a `"""…"""`
//! block is a single [`SyntaxKind::STRING`] token (the `"""`→JSON re-encode is a *lowering* concern,
//! not the lexer's), and unrecognized bytes become [`SyntaxKind::ERROR`] tokens rather than
//! aborting. Losslessness is what lets the language server map any offset back to source.
//!
//! Flux-Lang is indentation-sensitive, so the flat stream also carries the block structure as
//! **zero-width** [`SyntaxKind::INDENT`]/[`SyntaxKind::DEDENT`] markers (empty text, so still
//! lossless) inserted at the first token of each content line. Blank and comment-only lines never
//! move the indent stack. Tabs in leading indentation are recorded as a [`LexError`] (parity with
//! the legacy `preprocess`), but still lexed — the lexer is total.

use crate::syntax::SyntaxKind;
use text_size::{TextRange, TextSize};

/// One lexed token: its kind and its byte range in the source. Layout markers
/// ([`SyntaxKind::INDENT`]/[`SyntaxKind::DEDENT`]) carry an empty range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexToken {
    pub kind: SyntaxKind,
    pub range: TextRange,
}

/// A non-fatal lexing problem (the token stream is still produced in full).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub range: TextRange,
    pub message: String,
}

/// The result of [`lex`]: the full token stream plus any recorded [`LexError`]s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lexed {
    pub tokens: Vec<LexToken>,
    pub errors: Vec<LexError>,
}

impl Lexed {
    /// The concatenated text of the non-empty (i.e. non-layout) tokens, for a given source. Used by
    /// the losslessness guarantee: `lexed.reconstruct(src) == src`.
    pub fn reconstruct(&self, src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        for t in &self.tokens {
            out.push_str(&src[t.range]);
        }
        out
    }
}

/// Lex `src` into a lossless, layout-aware token stream.
pub fn lex(src: &str) -> Lexed {
    let raw = lex_raw(src);
    insert_layout(src, raw)
}

// ---------------------------------------------------------------------------
// Stage 1 — raw scan (no layout tokens yet)
// ---------------------------------------------------------------------------

struct RawScan {
    tokens: Vec<LexToken>,
    errors: Vec<LexError>,
}

fn lex_raw(src: &str) -> RawScan {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut s = RawScan {
        tokens: Vec::new(),
        errors: Vec::new(),
    };
    let mut i = 0usize;

    while i < n {
        let start = i;
        let b = bytes[i];
        let kind = match b {
            b'\n' => {
                i += 1;
                SyntaxKind::NEWLINE
            }
            b'\r' => {
                i += 1;
                if i < n && bytes[i] == b'\n' {
                    i += 1;
                }
                SyntaxKind::NEWLINE
            }
            b' ' | b'\t' => {
                while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                }
                SyntaxKind::WHITESPACE
            }
            b'#' => {
                while i < n && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                SyntaxKind::COMMENT
            }
            b'"' => scan_string(src, &mut i, &mut s),
            b'$' => {
                i += 1;
                consume_ident_tail(bytes, &mut i);
                SyntaxKind::VAR
            }
            b'@' => {
                i += 1;
                consume_ident_tail(bytes, &mut i);
                SyntaxKind::ANNOTATION
            }
            b'0'..=b'9' => {
                while i < n && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                    i += 1;
                }
                if i + 1 < n && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                    i += 1; // '.'
                    while i < n && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                SyntaxKind::NUMBER
            }
            _ if is_ident_start(b) => {
                i += 1;
                consume_ident_tail(bytes, &mut i);
                SyntaxKind::IDENT
            }
            _ => scan_punct(bytes, &mut i),
        };
        s.tokens.push(LexToken {
            kind,
            range: range(start, i),
        });
    }

    s
}

/// Scan a `"…"` or `"""…"""` string starting at `bytes[*i] == '"'`. Advances `*i` past the closing
/// delimiter (or to EOF, recording an "unterminated" error). Returns [`SyntaxKind::STRING`].
fn scan_string(src: &str, i: &mut usize, s: &mut RawScan) -> SyntaxKind {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let start = *i;
    if bytes[*i..].starts_with(b"\"\"\"") {
        // Triple-quoted: verbatim to the next `"""`.
        *i += 3;
        loop {
            if *i >= n {
                s.errors.push(LexError {
                    range: range(start, *i),
                    message: "unterminated multi-line string: missing closing `\"\"\"`".into(),
                });
                break;
            }
            if bytes[*i..].starts_with(b"\"\"\"") {
                *i += 3;
                break;
            }
            *i += 1;
        }
    } else {
        // Single-quoted: to the next unescaped `"`, but never across a line break.
        *i += 1;
        loop {
            if *i >= n || bytes[*i] == b'\n' || bytes[*i] == b'\r' {
                s.errors.push(LexError {
                    range: range(start, *i),
                    message: "unterminated string: missing closing `\"`".into(),
                });
                break;
            }
            if bytes[*i] == b'\\' && *i + 1 < n {
                *i += 2; // skip the escape and its target
                continue;
            }
            if bytes[*i] == b'"' {
                *i += 1;
                break;
            }
            *i += 1;
        }
    }
    SyntaxKind::STRING
}

/// Scan one- or two-character punctuation/operators. Advances `*i`. Unknown bytes advance one byte
/// (by char, so a multi-byte UTF-8 char is one [`SyntaxKind::ERROR`] token, not a byte split).
fn scan_punct(bytes: &[u8], i: &mut usize) -> SyntaxKind {
    let n = bytes.len();
    let two = |a: u8, b: u8| *i + 1 < n && bytes[*i] == a && bytes[*i + 1] == b;
    // two-character operators first
    let two_kind = if two(b'-', b'>') {
        Some(SyntaxKind::ARROW)
    } else if two(b'+', b'=') {
        Some(SyntaxKind::PLUS_EQ)
    } else if two(b'=', b'=') {
        Some(SyntaxKind::EQ_EQ)
    } else if two(b'!', b'=') {
        Some(SyntaxKind::NEQ)
    } else if two(b'<', b'=') {
        Some(SyntaxKind::LT_EQ)
    } else if two(b'>', b'=') {
        Some(SyntaxKind::GT_EQ)
    } else if two(b'&', b'&') {
        Some(SyntaxKind::AMP_AMP)
    } else if two(b'|', b'|') {
        Some(SyntaxKind::PIPE_PIPE)
    } else {
        None
    };
    if let Some(k) = two_kind {
        *i += 2;
        return k;
    }
    let one = match bytes[*i] {
        b'(' => Some(SyntaxKind::L_PAREN),
        b')' => Some(SyntaxKind::R_PAREN),
        b'[' => Some(SyntaxKind::L_BRACK),
        b']' => Some(SyntaxKind::R_BRACK),
        b'{' => Some(SyntaxKind::L_BRACE),
        b'}' => Some(SyntaxKind::R_BRACE),
        b',' => Some(SyntaxKind::COMMA),
        b':' => Some(SyntaxKind::COLON),
        b'.' => Some(SyntaxKind::DOT),
        b'?' => Some(SyntaxKind::QUESTION),
        b'|' => Some(SyntaxKind::PIPE),
        b'!' => Some(SyntaxKind::BANG),
        b'=' => Some(SyntaxKind::EQ),
        b'+' => Some(SyntaxKind::PLUS),
        b'-' => Some(SyntaxKind::MINUS),
        b'*' => Some(SyntaxKind::STAR),
        b'/' => Some(SyntaxKind::SLASH),
        b'<' => Some(SyntaxKind::LT),
        b'>' => Some(SyntaxKind::GT),
        _ => None,
    };
    if let Some(k) = one {
        *i += 1;
        return k;
    }
    // Unknown byte(s): consume one whole UTF-8 char so ranges stay on char boundaries.
    let ch_len = utf8_len(bytes[*i]);
    *i = (*i + ch_len).min(n);
    SyntaxKind::ERROR
}

fn consume_ident_tail(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() && is_ident_continue(bytes[*i]) {
        *i += 1;
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte length of the UTF-8 char whose lead byte is `b` (1 on any continuation/ASCII byte).
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
}

// ---------------------------------------------------------------------------
// Stage 2 — layout: insert zero-width INDENT/DEDENT at content-line starts
// ---------------------------------------------------------------------------

fn insert_layout(src: &str, raw: RawScan) -> Lexed {
    let RawScan {
        tokens: raw_tokens,
        mut errors,
    } = raw;
    let mut out: Vec<LexToken> = Vec::with_capacity(raw_tokens.len() + 8);
    let mut indent_stack: Vec<usize> = vec![0];

    // Group the raw tokens into physical lines (each ending at a NEWLINE or EOF).
    let mut idx = 0usize;
    let total = raw_tokens.len();
    while idx < total {
        let line_start = idx;
        while idx < total && raw_tokens[idx].kind != SyntaxKind::NEWLINE {
            idx += 1;
        }
        // `idx` now points at the NEWLINE (or == total at EOF). The line's content tokens are
        // `[line_start, idx)`; the NEWLINE (if any) is at `idx`.
        let line = &raw_tokens[line_start..idx];
        emit_line_layout(src, line, &mut out, &mut indent_stack, &mut errors);
        if idx < total {
            out.push(raw_tokens[idx].clone()); // the NEWLINE
            idx += 1;
        }
    }

    // Unwind any open blocks at EOF.
    let eof = TextSize::new(src.len() as u32);
    while indent_stack.len() > 1 {
        indent_stack.pop();
        out.push(LexToken {
            kind: SyntaxKind::DEDENT,
            range: TextRange::empty(eof),
        });
    }

    Lexed {
        tokens: out,
        errors,
    }
}

/// Emit one physical line's tokens, prefixed with the INDENT/DEDENT markers its indentation implies.
/// Blank and comment-only lines pass through without touching the indent stack.
fn emit_line_layout(
    src: &str,
    line: &[LexToken],
    out: &mut Vec<LexToken>,
    indent_stack: &mut Vec<usize>,
    errors: &mut Vec<LexError>,
) {
    // Leading whitespace (at most one WHITESPACE token by construction) sets the indent width.
    let (indent, lead_ws) = match line.first() {
        Some(t) if t.kind == SyntaxKind::WHITESPACE => {
            let ws_text = &src[t.range];
            if ws_text.contains('\t') {
                errors.push(LexError {
                    range: t.range,
                    message: "tabs are not allowed for indentation".into(),
                });
            }
            // Indentation width counts leading spaces (a tab is flagged above; count it as one).
            (ws_text.chars().count(), 1usize)
        }
        _ => (0usize, 0usize),
    };

    // A line whose only content (after leading whitespace) is a comment — or nothing — is blank for
    // layout purposes: emit its tokens verbatim, don't move the stack.
    let is_blank = line[lead_ws..]
        .iter()
        .all(|t| t.kind == SyntaxKind::COMMENT || t.kind == SyntaxKind::WHITESPACE);

    if is_blank {
        out.extend(line.iter().cloned());
        return;
    }

    // Content line. Emit leading whitespace first, then the layout markers at the offset of the
    // first significant token, then the rest of the line.
    if lead_ws == 1 {
        out.push(line[0].clone());
    }
    let first_sig_offset = line[lead_ws].range.start();
    let cur = *indent_stack.last().expect("indent stack is never empty");
    if indent > cur {
        indent_stack.push(indent);
        out.push(LexToken {
            kind: SyntaxKind::INDENT,
            range: TextRange::empty(first_sig_offset),
        });
    } else if indent < cur {
        while indent_stack.len() > 1 && *indent_stack.last().unwrap() > indent {
            indent_stack.pop();
            out.push(LexToken {
                kind: SyntaxKind::DEDENT,
                range: TextRange::empty(first_sig_offset),
            });
        }
        // A dedent that lands between two stack levels (inconsistent indentation) is a structural
        // concern the parser reports; the lexer records it so nothing is silently swallowed.
        if *indent_stack.last().unwrap() != indent {
            errors.push(LexError {
                range: TextRange::empty(first_sig_offset),
                message: "inconsistent indentation: dedent does not match any enclosing block"
                    .into(),
            });
        }
    }
    out.extend(line[lead_ws..].iter().cloned());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<SyntaxKind> {
        lex(src).tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lexer_is_lossless() {
        // A corpus exercising comments, blank lines, strings (incl. triple), sigils, operators, and
        // a trailing-newline-free tail. Concatenating token text must reproduce it byte-for-byte.
        let corpus = "flow greet($name: String) -> String\n  # a comment\n  $g = fmt(\"hi {name}\")\n\n  when $g != \"\"\n    do write \"out.txt\", $g\n  $blob = \"\"\"\nmulti\nline\n\"\"\"\n  return $g";
        for src in [
            corpus,
            "",
            "\n",
            "no trailing newline",
            "flow x\n  do read \"a\"\r\n  do read \"b\"\n", // CRLF in the middle
            "  \n\t\n# only comments\n",
            "$a.b.c ?",
        ] {
            let lexed = lex(src);
            assert_eq!(lexed.reconstruct(src), src, "not lossless for {src:?}");
        }
    }

    #[test]
    fn layout_tokens_track_nesting() {
        // flow header (col 0) -> body (indent 2) -> nested when body (indent 4) -> back to 2 -> EOF.
        let src = "flow f\n  do a\n  when $x\n    do b\n  do c\n";
        let ks = kinds(src);
        // One INDENT into the body, one INDENT into the when-body, one DEDENT back to the body, and
        // a final DEDENT unwinding the body at EOF.
        let indents = ks.iter().filter(|k| **k == SyntaxKind::INDENT).count();
        let dedents = ks.iter().filter(|k| **k == SyntaxKind::DEDENT).count();
        assert_eq!(indents, 2, "kinds: {ks:?}");
        assert_eq!(dedents, 2, "kinds: {ks:?}");
        // Blank and comment-only lines must not emit layout tokens.
        let src2 = "flow f\n  do a\n\n  # note\n  do b\n";
        let ks2 = kinds(src2);
        assert_eq!(
            ks2.iter().filter(|k| **k == SyntaxKind::INDENT).count(),
            1,
            "{ks2:?}"
        );
        assert_eq!(
            ks2.iter().filter(|k| **k == SyntaxKind::DEDENT).count(),
            1,
            "{ks2:?}"
        );
    }

    #[test]
    fn tabs_in_indentation_are_flagged() {
        let src = "flow f\n\tdo a\n";
        let lexed = lex(src);
        assert_eq!(lexed.reconstruct(src), src, "still lossless");
        assert!(
            lexed
                .errors
                .iter()
                .any(|e| e.message.contains("tabs are not allowed")),
            "expected a tab-indent error, got {:?}",
            lexed.errors
        );
    }

    #[test]
    fn triple_string_is_one_token() {
        let src = "$x = \"\"\"a\nb\"\"\"\n";
        let strings: Vec<_> = lex(src)
            .tokens
            .into_iter()
            .filter(|t| t.kind == SyntaxKind::STRING)
            .collect();
        assert_eq!(
            strings.len(),
            1,
            "the `\"\"\"` block should lex as exactly one STRING token"
        );
        assert_eq!(&src[strings[0].range], "\"\"\"a\nb\"\"\"");
    }
}

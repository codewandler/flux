//! flux's own markdown parser (L-02): a line-based block pass producing the [`crate::ast`] tree,
//! with inline content resolved by [`crate::inline`].
//!
//! # Supported (a pragmatic CommonMark-core + GFM subset)
//! ATX headings, paragraphs, fenced code blocks (``` / ~~~, info string), blockquotes, bullet and
//! ordered lists (nested, tight/loose), thematic breaks, GFM pipe tables, and the inline set:
//! backslash escapes, code spans, emphasis/strong (`*`/`_`, flanking + mod-3), GFM strikethrough
//! (`~~`), inline links/images with optional titles, `<scheme:...>` autolinks, hard + soft breaks.
//!
//! # NOT parsed (deliberately — these stay literal text)
//! - setext headings (`===` / `---` underlines)
//! - indented (4-space) code blocks
//! - HTML blocks and inline raw HTML — with ONE exception: an HTML **comment** block
//!   (`<!-- … -->` at block start) is recognized as [`Block::Html`] so renderers can hide it
//!   (generated skills carry `<!-- BEGIN generated -->` markers)
//! - link reference definitions and reference links (`[text][label]`)
//! - entity references (`&amp;` is not decoded)
//! - GFM task-list checkboxes (`- [ ]` keeps its literal text), bare-URL autolinks, footnotes
//! - lazy continuation lines (blockquote/list paragraph continuations must keep their prefix)

use crate::ast::{Alignment, Block, Document, List, ListItem, ParseOptions, Table};
use crate::inline::parse_inlines;

/// Parse a complete markdown document with default [`ParseOptions`].
pub fn parse(src: &str) -> Document {
    parse_with(src, &ParseOptions::default())
}

/// Parse a complete markdown document.
pub fn parse_with(src: &str, opts: &ParseOptions) -> Document {
    let lines: Vec<&str> = src.lines().collect();
    Document {
        blocks: parse_blocks(&lines, opts),
    }
}

fn parse_blocks(lines: &[&str], opts: &ParseOptions) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if is_blank(line) {
            i += 1;
            continue;
        }
        if let Some((level, text)) = atx_heading(line) {
            out.push(Block::Heading {
                level,
                content: parse_inlines(text.trim(), opts),
            });
            i += 1;
        } else if let Some(f) = open_fence(line) {
            out.push(parse_fence(lines, &mut i, f));
        } else if is_thematic_break(line) {
            out.push(Block::ThematicBreak);
            i += 1;
        } else if is_blockquote_start(line) {
            out.push(parse_blockquote(lines, &mut i, opts));
        } else if is_html_comment_start(line) {
            out.push(parse_html_comment(lines, &mut i));
        } else if let Some(m) = list_marker(line) {
            out.push(parse_list(lines, &mut i, m, opts));
        } else if let Some(t) = table_start(lines, i, opts) {
            out.push(parse_table(lines, &mut i, t, opts));
        } else {
            out.push(parse_paragraph(lines, &mut i, opts));
        }
    }
    out
}

fn is_blank(line: &str) -> bool {
    line.chars().all(|c| c == ' ' || c == '\t')
}

/// Leading indentation in columns (tab advances to the next multiple of 4).
fn indent_cols(line: &str) -> usize {
    let mut col = 0;
    for c in line.chars() {
        match c {
            ' ' => col += 1,
            '\t' => col += 4 - col % 4,
            _ => break,
        }
    }
    col
}

/// Strip up to `cols` columns of leading whitespace (a tab that crosses the target counts as
/// consumed — pragmatic; flux's docs indent with spaces).
fn strip_cols(line: &str, cols: usize) -> &str {
    let mut col = 0;
    for (idx, c) in line.char_indices() {
        if col >= cols {
            return &line[idx..];
        }
        match c {
            ' ' => col += 1,
            '\t' => col += 4 - col % 4,
            _ => return &line[idx..],
        }
    }
    ""
}

/// The line with at most 3 leading spaces stripped, or `None` if it is indented 4+ columns (which
/// this parser treats as ordinary paragraph text — indented code is not supported).
fn block_start(line: &str) -> Option<&str> {
    if indent_cols(line) > 3 {
        return None;
    }
    Some(line.trim_start_matches(' '))
}

fn atx_heading(line: &str) -> Option<(u8, &str)> {
    let t = block_start(line)?;
    let hashes = t.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    // Strip an optional closing run of #'s (must be preceded by a space).
    let mut text = rest.trim();
    let trailing = text.bytes().rev().take_while(|&b| b == b'#').count();
    if trailing > 0 {
        let head = &text[..text.len() - trailing];
        if head.is_empty() || head.ends_with([' ', '\t']) {
            text = head.trim_end();
        }
    }
    Some((hashes as u8, text))
}

struct Fence {
    ch: u8,
    len: usize,
    indent: usize,
    info: String,
}

fn open_fence(line: &str) -> Option<Fence> {
    let t = block_start(line)?;
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = t.bytes().take_while(|&b| b == ch).count();
    if len < 3 {
        return None;
    }
    let info = t[len..].trim();
    if ch == b'`' && info.contains('`') {
        return None;
    }
    Some(Fence {
        ch,
        len,
        indent: indent_cols(line),
        info: info.to_string(),
    })
}

fn closes_fence(line: &str, f: &Fence) -> bool {
    let Some(t) = block_start(line) else {
        return false;
    };
    let run = t.bytes().take_while(|&b| b == f.ch).count();
    run >= f.len && t[run..].trim().is_empty()
}

fn parse_fence(lines: &[&str], i: &mut usize, f: Fence) -> Block {
    *i += 1;
    let mut body: Vec<&str> = Vec::new();
    while *i < lines.len() && !closes_fence(lines[*i], &f) {
        body.push(strip_cols(lines[*i], f.indent.min(indent_cols(lines[*i]))));
        *i += 1;
    }
    if *i < lines.len() {
        *i += 1; // consume the closing fence
    }
    Block::CodeBlock {
        info: f.info,
        literal: body.join("\n"),
    }
}

fn is_thematic_break(line: &str) -> bool {
    let Some(t) = block_start(line) else {
        return false;
    };
    let mut ch = None;
    let mut count = 0;
    for c in t.chars() {
        match c {
            ' ' | '\t' => {}
            '-' | '*' | '_' => {
                if ch.get_or_insert(c) != &c {
                    return false;
                }
                count += 1;
            }
            _ => return false,
        }
    }
    count >= 3
}

fn is_blockquote_start(line: &str) -> bool {
    block_start(line).is_some_and(|t| t.starts_with('>'))
}

fn parse_blockquote(lines: &[&str], i: &mut usize, opts: &ParseOptions) -> Block {
    let mut inner: Vec<&str> = Vec::new();
    while *i < lines.len() && is_blockquote_start(lines[*i]) {
        let t = block_start(lines[*i]).unwrap();
        let after = &t[1..];
        inner.push(after.strip_prefix(' ').unwrap_or(after));
        *i += 1;
    }
    Block::BlockQuote {
        blocks: parse_blocks(&inner, opts),
    }
}

fn is_html_comment_start(line: &str) -> bool {
    block_start(line).is_some_and(|t| t.starts_with("<!--"))
}

/// Consume an HTML comment block: from the `<!--` line through the first line containing `-->`
/// (or EOF), kept verbatim.
fn parse_html_comment(lines: &[&str], i: &mut usize) -> Block {
    let mut body: Vec<&str> = Vec::new();
    while *i < lines.len() {
        body.push(lines[*i]);
        let done = lines[*i].contains("-->");
        *i += 1;
        if done {
            break;
        }
    }
    Block::Html {
        literal: body.join("\n"),
    }
}

struct Marker {
    ordered: bool,
    start: u64,
    ch: char,
    /// The column where the item's content begins (marker + following spaces) — continuation
    /// lines must be indented at least this far.
    content_col: usize,
    /// Byte offset of the content on the marker line itself.
    content_start: usize,
}

fn list_marker(line: &str) -> Option<Marker> {
    let indent = indent_cols(line);
    let t = block_start(line)?;
    let (ordered, start, ch, marker_len) =
        if let Some(c) = t.chars().next().filter(|c| "-+*".contains(*c)) {
            (false, 1, c, 1)
        } else {
            let digits = t.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 || digits > 9 {
                return None;
            }
            let ch = *t.as_bytes().get(digits)? as char;
            if ch != '.' && ch != ')' {
                return None;
            }
            (true, t[..digits].parse().ok()?, ch, digits + 1)
        };
    let after = &t[marker_len..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let spaces = after.chars().take_while(|&c| c == ' ').count();
    // CommonMark: 1–4 spaces belong to the marker; more (or an empty item) means content starts
    // one column after the marker.
    let consumed = if after.is_empty() {
        0
    } else if spaces == 0 || spaces > 4 {
        1
    } else {
        spaces
    };
    let ws_bytes = line.len() - t.len();
    Some(Marker {
        ordered,
        start,
        ch,
        content_col: indent + marker_len + consumed.max(1),
        content_start: ws_bytes + marker_len + consumed,
    })
}

fn parse_list(lines: &[&str], i: &mut usize, first: Marker, opts: &ParseOptions) -> Block {
    let mut items = Vec::new();
    let mut loose = false;
    loop {
        let m = list_marker(lines[*i]).expect("caller matched a marker");
        let mut content: Vec<String> = vec![lines[*i][m.content_start..].to_string()];
        *i += 1;
        let mut pending_blanks = 0usize;
        while *i < lines.len() {
            let l = lines[*i];
            if is_blank(l) {
                pending_blanks += 1;
                *i += 1;
            } else if indent_cols(l) >= m.content_col {
                if pending_blanks > 0 {
                    loose = true;
                    content.extend(std::iter::repeat_n(String::new(), pending_blanks));
                    pending_blanks = 0;
                }
                content.push(strip_cols(l, m.content_col).to_string());
                *i += 1;
            } else {
                break;
            }
        }
        let content_refs: Vec<&str> = content.iter().map(String::as_str).collect();
        items.push(ListItem {
            blocks: parse_blocks(&content_refs, opts),
        });

        // Another item of the same list?
        if *i >= lines.len() {
            break;
        }
        match list_marker(lines[*i]) {
            Some(next) if next.ordered == first.ordered && next.ch == first.ch => {
                if pending_blanks > 0 {
                    loose = true; // blank line(s) between items
                }
            }
            _ => break,
        }
    }
    Block::List(List {
        ordered: first.ordered,
        start: first.start,
        tight: !loose,
        marker: first.ch,
        items,
    })
}

/// A paragraph runs until a blank line or a construct that may interrupt one.
fn interrupts_paragraph(line: &str, opts: &ParseOptions) -> bool {
    if atx_heading(line).is_some()
        || open_fence(line).is_some()
        || is_thematic_break(line)
        || is_blockquote_start(line)
    {
        return true;
    }
    let _ = opts;
    // A list interrupts a paragraph only with nonempty content, and an ordered one only from 1.
    if let Some(m) = list_marker(line) {
        let has_content = !line[m.content_start..].trim().is_empty();
        return has_content && (!m.ordered || m.start == 1);
    }
    false
}

fn parse_paragraph(lines: &[&str], i: &mut usize, opts: &ParseOptions) -> Block {
    let mut para: Vec<&str> = vec![lines[*i]];
    *i += 1;
    while *i < lines.len()
        && !is_blank(lines[*i])
        && !interrupts_paragraph(lines[*i], opts)
        // A table header + delimiter row interrupts a paragraph (GFM).
        && table_start(lines, *i, opts).is_none()
    {
        para.push(lines[*i]);
        *i += 1;
    }
    let src = para
        .iter()
        .map(|l| l.trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    Block::Paragraph {
        content: parse_inlines(src.trim_end(), opts),
    }
}

/// A table starts where a `|`-bearing header row is followed by a matching delimiter row.
fn table_start(lines: &[&str], i: usize, opts: &ParseOptions) -> Option<Vec<Alignment>> {
    if !opts.tables || !lines[i].contains('|') {
        return None;
    }
    let aligns = table_delim_row(lines.get(i + 1)?)?;
    (split_table_row(lines[i]).len() == aligns.len()).then_some(aligns)
}

fn table_delim_row(line: &str) -> Option<Vec<Alignment>> {
    if !line.contains('-') || !line.contains('|') {
        return None;
    }
    let mut aligns = Vec::new();
    for cell in split_table_row(line) {
        let c = cell.trim();
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        let dashes = c.trim_start_matches(':').trim_end_matches(':');
        if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
            return None;
        }
        aligns.push(match (left, right) {
            (true, true) => Alignment::Center,
            (true, false) => Alignment::Left,
            (false, true) => Alignment::Right,
            (false, false) => Alignment::None,
        });
    }
    (!aligns.is_empty()).then_some(aligns)
}

/// Split a table row on unescaped `|` (leading/trailing pipes optional), trimming each cell.
/// `\|` becomes a literal `|`; other escapes pass through to the inline parser.
fn split_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '\\' => {
                cur.push('\\');
                if let Some(&n) = chars.peek() {
                    cur.push(n);
                    chars.next();
                }
            }
            '|' => cells.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() || cells.is_empty() {
        cells.push(cur);
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

fn parse_table(
    lines: &[&str],
    i: &mut usize,
    aligns: Vec<Alignment>,
    opts: &ParseOptions,
) -> Block {
    let mut rows = Vec::new();
    let header = split_table_row(lines[*i]);
    rows.push(
        header
            .iter()
            .map(|c| parse_inlines(c, opts))
            .collect::<Vec<_>>(),
    );
    *i += 2; // header + delimiter row
    while *i < lines.len() {
        let l = lines[*i];
        if is_blank(l)
            || !l.contains('|')
            || atx_heading(l).is_some()
            || open_fence(l).is_some()
            || is_thematic_break(l)
            || is_blockquote_start(l)
        {
            break;
        }
        // GFM: body cells beyond the header's column count are dropped (short rows stay short —
        // renderers pad).
        let mut cells = split_table_row(l);
        cells.truncate(aligns.len());
        rows.push(cells.iter().map(|c| parse_inlines(c, opts)).collect());
        *i += 1;
    }
    Block::Table(Table {
        alignments: aligns,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Inline;

    fn one(src: &str) -> Block {
        let mut doc = parse(src);
        assert_eq!(doc.blocks.len(), 1, "one block from {src:?}: {doc:?}");
        doc.blocks.remove(0)
    }

    fn text(s: &str) -> Inline {
        Inline::Text(s.into())
    }

    #[test]
    fn headings_paragraphs_and_rules() {
        let doc = parse("# Title ##\n\nBody text.\n\n---\n");
        assert_eq!(
            doc.blocks[0],
            Block::Heading {
                level: 1,
                content: vec![text("Title")]
            }
        );
        assert_eq!(
            doc.blocks[1],
            Block::Paragraph {
                content: vec![text("Body text.")]
            }
        );
        assert_eq!(doc.blocks[2], Block::ThematicBreak);
    }

    #[test]
    fn fenced_code_keeps_literal_and_info() {
        let b = one("```rust ignore\nfn main() {}\n\n  done\n```\n");
        assert_eq!(
            b,
            Block::CodeBlock {
                info: "rust ignore".into(),
                literal: "fn main() {}\n\n  done".into()
            }
        );
        // Unterminated fences run to EOF (streaming-friendly).
        let b = one("```\nhalf");
        assert_eq!(
            b,
            Block::CodeBlock {
                info: String::new(),
                literal: "half".into()
            }
        );
    }

    #[test]
    fn blockquote_contains_blocks() {
        let b = one("> quoted line\n> more quote\n>\n> second para\n");
        let Block::BlockQuote { blocks } = b else {
            panic!("{b:?}")
        };
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            Block::Paragraph {
                content: vec![text("quoted line"), Inline::SoftBreak, text("more quote")]
            }
        );
    }

    #[test]
    fn tight_and_loose_lists() {
        let Block::List(l) = one("- a\n- b\n") else {
            panic!()
        };
        assert!(l.tight);
        assert_eq!(l.items.len(), 2);

        let Block::List(l) = one("- a\n\n- b\n") else {
            panic!()
        };
        assert!(!l.tight, "blank between items makes the list loose");

        let Block::List(l) = one("1. a\n2. b\n") else {
            panic!()
        };
        assert!(l.ordered);
        assert_eq!(l.start, 1);
        assert_eq!(l.marker, '.');
    }

    #[test]
    fn nested_list_structure() {
        let Block::List(l) = one("- parent\n  - child one\n  - child two\n- second\n") else {
            panic!()
        };
        assert_eq!(l.items.len(), 2);
        assert!(l.tight);
        let parent = &l.items[0].blocks;
        assert_eq!(
            parent[0],
            Block::Paragraph {
                content: vec![text("parent")]
            }
        );
        let Block::List(nested) = &parent[1] else {
            panic!("{parent:?}")
        };
        assert_eq!(nested.items.len(), 2);
    }

    #[test]
    fn list_item_with_fenced_code() {
        let Block::List(l) = one("- item\n  ```\n  code here\n  ```\n") else {
            panic!()
        };
        assert_eq!(
            l.items[0].blocks[1],
            Block::CodeBlock {
                info: String::new(),
                literal: "code here".into()
            }
        );
    }

    #[test]
    fn table_with_alignment() {
        let Block::Table(t) = one("| a | b |\n|:--|--:|\n| 1 | 2 |\n") else {
            panic!()
        };
        assert_eq!(t.alignments, vec![Alignment::Left, Alignment::Right]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1][0], vec![text("1")]);
    }

    #[test]
    fn emphasis_flavors() {
        let Block::Paragraph { content } = one("plain **bold** *em* ~~gone~~ `code`\n") else {
            panic!()
        };
        assert!(content.contains(&Inline::Strong(vec![text("bold")])));
        assert!(content.contains(&Inline::Emphasis(vec![text("em")])));
        assert!(content.contains(&Inline::Strikethrough(vec![text("gone")])));
        assert!(content.contains(&Inline::Code("code".into())));
    }

    #[test]
    fn emphasis_edge_cases() {
        // `**a***b*` = strong(a) + em(b) per CommonMark.
        let Block::Paragraph { content } = one("**a***b*") else {
            panic!()
        };
        assert_eq!(
            content,
            vec![
                Inline::Strong(vec![text("a")]),
                Inline::Emphasis(vec![text("b")])
            ]
        );
        // Nested: *a **b** c*
        let Block::Paragraph { content } = one("*a **b** c*") else {
            panic!()
        };
        assert_eq!(
            content,
            vec![Inline::Emphasis(vec![
                text("a "),
                Inline::Strong(vec![text("b")]),
                text(" c"),
            ])]
        );
        // No intraword underscore emphasis.
        let Block::Paragraph { content } = one("snake_case_name") else {
            panic!()
        };
        assert_eq!(content, vec![text("snake_case_name")]);
    }

    #[test]
    fn links_images_and_autolinks() {
        let Block::Paragraph { content } = one("See [the docs](https://e.com \"T\") now") else {
            panic!()
        };
        assert_eq!(
            content,
            vec![
                text("See "),
                Inline::Link {
                    dest: "https://e.com".into(),
                    title: "T".into(),
                    content: vec![text("the docs")]
                },
                text(" now"),
            ]
        );
        let Block::Paragraph { content } = one("<https://auto.link>") else {
            panic!()
        };
        assert_eq!(
            content,
            vec![Inline::Link {
                dest: "https://auto.link".into(),
                title: String::new(),
                content: vec![text("https://auto.link")]
            }]
        );
        // A bracket without a tail is literal.
        let Block::Paragraph { content } = one("[not a link]") else {
            panic!()
        };
        assert_eq!(content, vec![text("[not a link]")]);
    }

    #[test]
    fn breaks_and_escapes() {
        let Block::Paragraph { content } = one("one  \ntwo\\\nthree\nfour") else {
            panic!()
        };
        assert_eq!(
            content,
            vec![
                text("one"),
                Inline::HardBreak,
                text("two"),
                Inline::HardBreak,
                text("three"),
                Inline::SoftBreak,
                text("four"),
            ]
        );
        let Block::Paragraph { content } = one("\\*not em\\* and \\| pipe") else {
            panic!()
        };
        assert_eq!(content, vec![text("*not em* and | pipe")]);
    }

    #[test]
    fn code_span_rules() {
        // Longer closing runs don't close; doubled backticks allow embedded ones.
        let Block::Paragraph { content } = one("`` a ` b ``") else {
            panic!()
        };
        assert_eq!(content, vec![Inline::Code("a ` b".into())]);
        // Unclosed backticks are literal.
        let Block::Paragraph { content } = one("a ` b") else {
            panic!()
        };
        assert_eq!(content, vec![text("a ` b")]);
    }

    #[test]
    fn html_comment_block_is_captured_and_table_interrupts_paragraph() {
        // The comment is its own (renderer-hidden) block; the table that follows parses even
        // though a paragraph-ish line precedes it.
        let doc =
            parse("<!-- BEGIN generated -->\n| a | b |\n|---|---|\n| 1 | 2 |\n<!-- END -->\n");
        assert_eq!(
            doc.blocks[0],
            Block::Html {
                literal: "<!-- BEGIN generated -->".into()
            }
        );
        assert!(matches!(doc.blocks[1], Block::Table(_)), "{doc:?}");
        assert_eq!(
            doc.blocks[2],
            Block::Html {
                literal: "<!-- END -->".into()
            }
        );
        // A table header+delimiter interrupts a plain paragraph too.
        let doc = parse("intro line\n| a | b |\n|---|---|\n");
        assert_eq!(doc.blocks.len(), 2);
        assert!(matches!(doc.blocks[1], Block::Table(_)), "{doc:?}");
    }

    #[test]
    fn unsupported_constructs_stay_literal() {
        // Setext underline is just a paragraph line (documented divergence).
        let doc = parse("Title\n===\n");
        assert_eq!(doc.blocks.len(), 1);
        // Entities are not decoded.
        let Block::Paragraph { content } = one("a &amp; b") else {
            panic!()
        };
        assert_eq!(content, vec![text("a &amp; b")]);
        // Inline HTML stays literal text.
        let Block::Paragraph { content } = one("a <span>b</span>") else {
            panic!()
        };
        assert_eq!(content, vec![text("a <span>b</span>")]);
    }
}

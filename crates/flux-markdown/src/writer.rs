//! The markdown writer: canonical emission of the [`crate::ast`] tree.
//!
//! The contract is **AST round-trip stability**: `parse(&to_markdown(&parse(src))) == parse(src)`.
//! Escaping is deliberately conservative (a few extra backslashes beat a re-parse drift), and the
//! emission is canonical — `*em*`, `**strong**`, backslash hard breaks, `-` bullets are preserved
//! from the source's own markers where the AST records them.

use crate::ast::{Alignment, Block, Document, Inline, List};

/// Render `doc` back to markdown text. Blocks are separated by blank lines; the output ends with a
/// single trailing newline (or is empty for an empty document).
pub fn to_markdown(doc: &Document) -> String {
    let parts: Vec<String> = doc.blocks.iter().map(block).collect();
    let mut out = parts.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// One block, rendered WITHOUT a trailing newline (the joiner adds separation).
fn block(b: &Block) -> String {
    match b {
        Block::Paragraph { content } => inlines(content, true),
        Block::Heading { level, content } => {
            format!(
                "{} {}",
                "#".repeat(*level as usize),
                inlines(content, false)
            )
        }
        Block::CodeBlock { info, literal } => {
            // Fence with one more char than the longest interior run (min 3); switch to ~ when the
            // info string itself carries a backtick.
            let longest = literal
                .split(|c: char| c != '`')
                .map(str::len)
                .max()
                .unwrap_or(0);
            let ch = if info.contains('`') { "~" } else { "`" };
            let fence = ch.repeat((longest + 1).max(3));
            if literal.is_empty() {
                format!("{fence}{info}\n{fence}")
            } else {
                format!("{fence}{info}\n{literal}\n{fence}")
            }
        }
        Block::BlockQuote { blocks } => {
            let inner: Vec<String> = blocks.iter().map(block).collect();
            prefix_lines(&inner.join("\n\n"), "> ", "> ")
        }
        Block::List(l) => list(l),
        Block::ThematicBreak => "---".to_string(),
        Block::Table(t) => table(t),
        Block::Html { literal } => literal.clone(),
    }
}

fn list(l: &List) -> String {
    // Inside a loose item, sibling blocks are separated by a blank line (that's what made the list
    // loose); inside a tight one they sit adjacent (e.g. a nested list right under its item text).
    let inner_sep = if l.tight { "\n" } else { "\n\n" };
    let mut items = Vec::new();
    for (n, item) in l.items.iter().enumerate() {
        let marker = if l.ordered {
            format!("{}{} ", l.start + n as u64, l.marker)
        } else {
            format!("{} ", l.marker)
        };
        let cont = " ".repeat(marker.len());
        let inner: Vec<String> = item.blocks.iter().map(block).collect();
        items.push(prefix_lines(&inner.join(inner_sep), &marker, &cont));
    }
    items.join(if l.tight { "\n" } else { "\n\n" })
}

/// Prefix a multi-line body: `first` on its first line, `cont` on the rest (blank inner lines get
/// the continuation prefix trimmed so no trailing whitespace leaks).
fn prefix_lines(body: &str, first: &str, cont: &str) -> String {
    let mut out = String::new();
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let p = if i == 0 { first } else { cont };
        if line.is_empty() {
            out.push_str(p.trim_end());
        } else {
            out.push_str(p);
            out.push_str(line);
        }
    }
    out
}

fn table(t: &crate::ast::Table) -> String {
    let mut lines = Vec::new();
    for (ri, row) in t.rows.iter().enumerate() {
        let cells: Vec<String> = row
            .iter()
            .map(|c| escape_pipes(&inlines(c, false)))
            .collect();
        lines.push(format!("| {} |", cells.join(" | ")));
        if ri == 0 {
            let delims: Vec<&str> = t
                .alignments
                .iter()
                .map(|a| match a {
                    Alignment::None => "---",
                    Alignment::Left => ":--",
                    Alignment::Center => ":-:",
                    Alignment::Right => "--:",
                })
                .collect();
            lines.push(format!("|{}|", delims.join("|")));
        }
    }
    lines.join("\n")
}

fn escape_pipes(s: &str) -> String {
    s.replace('|', "\\|")
}

/// Render an inline run. `breaks` enables soft/hard break emission (paragraph context); heading
/// and table-cell context flattens breaks to spaces. Breaks inside emphasis/link containers are
/// preserved in paragraph context (they re-parse to the same nested breaks).
fn inlines(content: &[Inline], breaks: bool) -> String {
    let mut out = String::new();
    write_inlines(&mut out, content, breaks);
    out
}

/// Append `content` to `out`. Line-start state is derived from `out` itself, so escaping stays
/// correct across container boundaries (e.g. a soft break inside `**strong**`).
fn write_inlines(out: &mut String, content: &[Inline], breaks: bool) {
    // Toggle between `*` and `_` when emphasis nodes are directly adjacent, so `*a**b*` never
    // forms (it would re-parse differently).
    let mut prev_emphasis_star = false;
    for node in content {
        let is_emphasis = matches!(node, Inline::Emphasis(_) | Inline::Strong(_));
        let star = !(is_emphasis && prev_emphasis_star && out.ends_with('*'));
        match node {
            Inline::Text(t) => escape_text_into(out, t),
            Inline::Code(c) => {
                let longest = c
                    .split(|ch: char| ch != '`')
                    .map(str::len)
                    .max()
                    .unwrap_or(0);
                let ticks = "`".repeat(longest + 1);
                let pad = c.starts_with([' ', '`'])
                    || c.ends_with([' ', '`']) && c.chars().any(|ch| ch != ' ');
                if pad {
                    out.push_str(&format!("{ticks} {c} {ticks}"));
                } else {
                    out.push_str(&format!("{ticks}{c}{ticks}"));
                }
            }
            Inline::Emphasis(inner) => {
                let d = if star { "*" } else { "_" };
                out.push_str(d);
                write_inlines(out, inner, breaks);
                out.push_str(d);
            }
            Inline::Strong(inner) => {
                let d = if star { "**" } else { "__" };
                out.push_str(d);
                write_inlines(out, inner, breaks);
                out.push_str(d);
            }
            Inline::Strikethrough(inner) => {
                out.push_str("~~");
                write_inlines(out, inner, breaks);
                out.push_str("~~");
            }
            Inline::Link {
                dest,
                title,
                content,
            } => write_link(out, dest, title, content, false, breaks),
            Inline::Image {
                dest,
                title,
                content,
            } => write_link(out, dest, title, content, true, breaks),
            Inline::SoftBreak => out.push_str(if breaks { "\n" } else { " " }),
            Inline::HardBreak => out.push_str(if breaks { "\\\n" } else { " " }),
        }
        prev_emphasis_star = is_emphasis && star;
    }
}

/// Whether `dest` re-parses as an `<...>` autolink (a valid scheme, no whitespace/angles).
fn is_autolink_dest(dest: &str) -> bool {
    let Some((scheme, tail)) = dest.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.len() <= 32
        && scheme.chars().next().unwrap().is_ascii_alphabetic()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && !tail
            .chars()
            .any(|c| c.is_whitespace() || c == '<' || c == '>')
}

fn write_link(
    out: &mut String,
    dest: &str,
    title: &str,
    content: &[Inline],
    image: bool,
    breaks: bool,
) {
    // An autolink survives as an autolink only when it will re-parse as one.
    if !image
        && title.is_empty()
        && content == [Inline::Text(dest.to_string())]
        && is_autolink_dest(dest)
    {
        out.push_str(&format!("<{dest}>"));
        return;
    }
    if image {
        out.push('!');
    }
    out.push('[');
    write_inlines(out, content, breaks);
    out.push_str("](");
    if dest.is_empty()
        || dest.chars().any(|c| c.is_whitespace())
        || dest.contains('(')
        || dest.contains(')')
    {
        out.push_str(&format!(
            "<{}>",
            dest.replace('<', "\\<").replace('>', "\\>")
        ));
    } else {
        out.push_str(dest);
    }
    if !title.is_empty() {
        out.push_str(&format!(" \"{}\"", title.replace('"', "\\\"")));
    }
    out.push(')');
}

/// Escape `t` into `out` so it re-parses as the same literal run. Always escapes the
/// inline-significant set; at a line start (derived from `out`) additionally neutralizes
/// block-start markers.
fn escape_text_into(out: &mut String, t: &str) {
    for c in t.chars() {
        let at_line_start = out.is_empty() || out.ends_with('\n');
        if at_line_start && matches!(c, '#' | '>' | '-' | '+') {
            // Neutralize block-start interpretations: heading/quote/list/hr markers.
            out.push('\\');
            out.push(c);
            continue;
        }
        match c {
            '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '<' => {
                out.push('\\');
                out.push(c);
            }
            '.' | ')' => {
                // "12. " at a line start would become an ordered list; escape the delimiter when
                // everything before it on this line is 1–9 digits.
                let line = &out[out.rfind('\n').map(|p| p + 1).unwrap_or(0)..];
                let n = line.chars().count();
                if (1..=9).contains(&n) && line.chars().all(|p| p.is_ascii_digit()) {
                    out.push('\\');
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// The round-trip law on a construct zoo.
    #[test]
    fn write_then_parse_is_stable() {
        let srcs = [
            "# Title\n\nBody with **bold**, *em*, `code`, and [a link](https://e.com \"T\").\n",
            "- one\n- two\n  - nested\n- three\n",
            "1. first\n\n2. loose second\n\n   with a second paragraph\n",
            "> quote\n> more\n>\n> - quoted list\n",
            "```rust\nfn main() { let x = \"`\"; }\n```\n",
            "| a | b |\n|:--|--:|\n| **1** | `2|x` |\n",
            "hard  \nbreak\\\nsoft\nend\n\n---\n",
            "text with literal * star _ under ~ tilde [ bracket < angle | pipe\n",
            "\\# not a heading\n\n12\\. not a list\n",
        ];
        for src in srcs {
            let a = parse(src);
            let written = to_markdown(&a);
            let b = parse(&written);
            assert_eq!(a, b, "round-trip drift for {src:?} via {written:?}");
        }
    }

    /// Escaping keeps synthetic text literal even when it looks like markup.
    #[test]
    fn synthetic_text_survives() {
        use crate::ast::{Block, Document, Inline};
        let doc = Document {
            blocks: vec![Block::Paragraph {
                content: vec![Inline::Text("# not a heading - or *list*".into())],
            }],
        };
        let written = to_markdown(&doc);
        assert_eq!(parse(&written), doc, "written: {written:?}");
    }

    /// Adjacent emphasis nodes alternate delimiters so they don't merge on re-parse.
    #[test]
    fn adjacent_emphasis_alternates_delimiters() {
        use crate::ast::{Block, Document, Inline};
        let doc = Document {
            blocks: vec![Block::Paragraph {
                content: vec![
                    Inline::Emphasis(vec![Inline::Text("a".into())]),
                    Inline::Emphasis(vec![Inline::Text("b".into())]),
                ],
            }],
        };
        let written = to_markdown(&doc);
        assert_eq!(parse(&written), doc, "written: {written:?}");
    }
}

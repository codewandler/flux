//! The shared layout core: walk the AST into styled, pre-wrapped physical lines.
//!
//! Both renderers (ratatui spans, ANSI terminal) consume this one engine, so they lay out
//! identically: greedy word wrap against `width` (floor 20), hanging list markers emitted once,
//! repeating blockquote bars, per-line code with a glyph gutter and 2-space inset, box-drawing
//! tables, and the blank-line "gap" discipline between blocks. The visible conventions
//! deliberately match the wrapper stack this engine replaced (same markers, rules, and wrap
//! points), except for documented structural fixes: nested lists render correctly (the old stack
//! collapsed a parent item's marker into its first child's line), and fenced code is framed by a
//! mono-safe gutter.

use crate::ast::{Alignment, Block, Document, Inline, List, Table};
use unicode_width::UnicodeWidthStr;

/// Style flags for one laid-out segment. Renderers map these onto their theme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub heading: bool,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
    pub muted: bool,
}

/// One styled run within a physical line.
#[derive(Debug, Clone)]
pub struct Seg {
    pub text: String,
    pub style: Style,
}

impl Seg {
    fn plain(text: impl Into<String>) -> Self {
        Seg {
            text: text.into(),
            style: Style::default(),
        }
    }
}

/// Lay out `doc` as pre-wrapped lines of styled segments (an empty vec per blank gap line).
pub fn layout(doc: &Document, width: usize) -> Vec<Vec<Seg>> {
    let mut l = Layouter {
        width: width.max(20),
        prefixes: Vec::new(),
        pending_gap: false,
        wrote_any: false,
        cur: Vec::new(),
        lines: Vec::new(),
    };
    l.blocks(&doc.blocks, false);
    if !l.cur.is_empty() {
        l.newline();
    }
    l.lines
}

/// A nesting prefix: `first` on the first line a level appears on (a list marker), `cont` on every
/// continuation line (marker-width blanks; a blockquote bar repeats on both).
struct Prefix {
    first: Seg,
    cont: Seg,
    emitted: bool,
}

impl Prefix {
    /// A structural prefix repeated on every physical row (blockquote and fenced-code gutters).
    fn repeating(seg: Seg) -> Self {
        Prefix {
            first: seg.clone(),
            cont: seg,
            emitted: false,
        }
    }
}

struct Layouter {
    width: usize,
    prefixes: Vec<Prefix>,
    pending_gap: bool,
    wrote_any: bool,
    cur: Vec<Seg>,
    lines: Vec<Vec<Seg>>,
}

impl Layouter {
    fn newline(&mut self) {
        self.lines.push(std::mem::take(&mut self.cur));
        self.wrote_any = true;
    }

    /// Emit an owed blank line before a block.
    fn gap(&mut self) {
        if self.pending_gap && self.wrote_any {
            self.lines.push(Vec::new());
        }
        self.pending_gap = false;
    }

    fn indent_cont(&self) -> (Vec<Seg>, usize) {
        let mut segs = Vec::new();
        let mut w = 0;
        for p in &self.prefixes {
            w += UnicodeWidthStr::width(p.cont.text.as_str());
            segs.push(p.cont.clone());
        }
        (segs, w)
    }

    fn indent_first(&mut self) -> Vec<Seg> {
        let mut segs = Vec::new();
        for p in &mut self.prefixes {
            let seg = if p.emitted {
                &p.cont
            } else {
                p.emitted = true;
                &p.first
            };
            segs.push(seg.clone());
        }
        segs
    }

    fn blocks(&mut self, blocks: &[Block], tight: bool) {
        for b in blocks {
            self.block(b, tight);
        }
    }

    /// Lay out one block. In a `tight` list context no block owes a trailing gap (tight items sit
    /// on adjacent lines); everywhere else a rendered block owes one blank line.
    fn block(&mut self, b: &Block, tight: bool) {
        match b {
            Block::Paragraph { content } => {
                let segs = collect_inlines(content);
                self.flush(segs, false);
                self.pending_gap = !tight;
            }
            Block::Heading { content, .. } => {
                let segs = collect_inlines(content);
                self.flush(segs, true);
                self.pending_gap = !tight;
            }
            Block::CodeBlock { literal, .. } => {
                self.gap();
                self.prefixes.push(Prefix::repeating(Seg {
                    text: "▎ ".into(),
                    style: Style {
                        muted: true,
                        ..Style::default()
                    },
                }));
                if !literal.is_empty() {
                    for line in literal.split('\n') {
                        self.code_line(line);
                    }
                }
                self.prefixes.pop();
                self.pending_gap = !tight;
            }
            Block::BlockQuote { blocks } => {
                self.gap();
                self.prefixes.push(Prefix::repeating(Seg {
                    text: "│ ".into(),
                    style: Style {
                        muted: true,
                        ..Style::default()
                    },
                }));
                self.blocks(blocks, false);
                self.prefixes.pop();
                self.pending_gap = !tight;
            }
            Block::List(l) => {
                self.gap();
                self.list(l);
                self.pending_gap = !tight;
            }
            Block::ThematicBreak => {
                self.gap();
                let (ind, _) = self.indent_cont();
                self.cur.extend(ind);
                self.cur.push(Seg {
                    text: "─".repeat(self.width.min(60)),
                    style: Style {
                        muted: true,
                        ..Style::default()
                    },
                });
                self.newline();
                self.pending_gap = !tight;
            }
            Block::Table(t) => {
                self.gap();
                self.table(t);
                self.pending_gap = !tight;
            }
            // Comments are invisible: no output, and the gap discipline is left untouched.
            Block::Html { .. } => {}
        }
    }

    fn list(&mut self, l: &List) {
        for (n, item) in l.items.iter().enumerate() {
            let marker = if l.ordered {
                format!("{}{}", l.start + n as u64, marker_sep(l.marker))
            } else {
                "•".to_string()
            };
            let marker = if item.blocks.is_empty() {
                marker
            } else {
                format!("{marker} ")
            };
            let pad = " ".repeat(UnicodeWidthStr::width(marker.as_str()));
            self.prefixes.push(Prefix {
                first: Seg::plain(marker),
                cont: Seg::plain(pad),
                emitted: false,
            });
            if item.blocks.is_empty() {
                // Empty Markdown list items still have visible syntax. In particular, a terse
                // model answer such as `2.` parses as an empty ordered item; dropping its marker
                // here made the entire assistant response disappear in both terminal renderers.
                self.gap();
                let prefix = self.indent_first();
                self.cur.extend(prefix);
                self.newline();
            } else {
                self.blocks(&item.blocks, l.tight);
            }
            self.prefixes.pop();
        }
    }

    /// Wrap and emit accumulated inline segments; `heading` applies the heading base style.
    fn flush(&mut self, segments: Vec<(String, Style)>, heading: bool) {
        if segments.is_empty() {
            return;
        }
        self.gap();
        let (cont, cont_w) = self.indent_cont();
        let avail = self.width.saturating_sub(cont_w).max(20);
        let first = self.indent_first();
        self.cur.extend(first);

        let mut line_vis = 0usize;
        let mut pending_space = false;
        let mut started = false;
        for (raw, style) in &segments {
            for atom in atoms(raw) {
                match atom {
                    Atom::Space => pending_space = true,
                    Atom::Hard => {
                        // Drop a hard break before any word has started (a bare-marker line).
                        if !started {
                            continue;
                        }
                        self.newline();
                        self.cur.extend(cont.clone());
                        line_vis = 0;
                        pending_space = false;
                    }
                    Atom::Word(word) => {
                        let wv = UnicodeWidthStr::width(word);
                        let sep = usize::from(line_vis > 0 && pending_space);
                        if line_vis > 0 && line_vis + sep + wv > avail {
                            self.newline();
                            self.cur.extend(cont.clone());
                            line_vis = 0;
                        } else if sep == 1 {
                            self.cur.push(Seg::plain(" "));
                            line_vis += 1;
                        }
                        let mut st = *style;
                        st.heading = heading;
                        self.cur.push(Seg {
                            text: word.to_string(),
                            style: st,
                        });
                        line_vis += wv;
                        pending_space = false;
                        started = true;
                    }
                }
            }
        }
        self.newline();
    }

    /// One literal code line: continuation indent + a 2-space inset + uniformly code-styled text.
    fn code_line(&mut self, line: &str) {
        self.gap();
        let (ind, _) = self.indent_cont();
        self.cur.extend(ind);
        self.cur.push(Seg::plain("  "));
        self.cur.push(Seg {
            text: line.to_string(),
            style: Style {
                code: true,
                ..Style::default()
            },
        });
        self.newline();
    }

    /// Box-drawing table: column widths from max visible cell width, header separator after row 0.
    fn table(&mut self, t: &Table) {
        let muted = Style {
            muted: true,
            ..Style::default()
        };
        // Buffer every cell as flattened segs + visible width.
        let rows: Vec<Vec<(Vec<Seg>, usize)>> = t
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| table_cell(cell)).collect())
            .collect();
        let ncol = t
            .alignments
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        let mut widths = vec![0usize; ncol];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.1);
            }
        }
        let empty: (Vec<Seg>, usize) = (Vec::new(), 0);
        for (ri, row) in rows.iter().enumerate() {
            let (ind, _) = self.indent_cont();
            self.cur.extend(ind);
            self.cur.push(Seg {
                text: "│ ".into(),
                style: muted,
            });
            for (i, width) in widths.iter().enumerate() {
                let cell = row.get(i).unwrap_or(&empty);
                let pad = width.saturating_sub(cell.1);
                match t.alignments.get(i).copied().unwrap_or(Alignment::None) {
                    Alignment::Right => {
                        self.cur.push(Seg::plain(" ".repeat(pad)));
                        self.cur.extend(cell.0.clone());
                    }
                    Alignment::Center => {
                        let l = pad / 2;
                        self.cur.push(Seg::plain(" ".repeat(l)));
                        self.cur.extend(cell.0.clone());
                        self.cur.push(Seg::plain(" ".repeat(pad - l)));
                    }
                    _ => {
                        self.cur.extend(cell.0.clone());
                        self.cur.push(Seg::plain(" ".repeat(pad)));
                    }
                }
                self.cur.push(Seg {
                    text: " │ ".into(),
                    style: muted,
                });
            }
            self.newline();
            if ri == 0 {
                let (ind, _) = self.indent_cont();
                self.cur.extend(ind);
                self.cur.push(Seg {
                    text: "├".into(),
                    style: muted,
                });
                for (i, width) in widths.iter().enumerate() {
                    self.cur.push(Seg {
                        text: "─".repeat(width + 2),
                        style: muted,
                    });
                    self.cur.push(Seg {
                        text: (if i + 1 < ncol { "┼" } else { "┤" }).into(),
                        style: muted,
                    });
                }
                self.newline();
            }
        }
    }
}

/// Ordered markers always render `N. ` / `N) ` from the list's own marker char.
fn marker_sep(marker: char) -> char {
    if marker == ')' {
        ')'
    } else {
        '.'
    }
}

/// Flatten a table cell's inline content (breaks become spaces) into segs + visible width.
fn table_cell(content: &[Inline]) -> (Vec<Seg>, usize) {
    let mut segs = Vec::new();
    for (text, style) in collect_inlines(content) {
        let t = text.replace('\n', " ");
        segs.push(Seg { text: t, style });
    }
    let w = segs
        .iter()
        .map(|s| UnicodeWidthStr::width(s.text.as_str()))
        .sum();
    (segs, w)
}

/// Flatten an inline tree into `(text, style)` runs, mirroring the flat event stream the old
/// renderers consumed: breaks are plain (` `/`\n`) runs, link/image content is styled `link`, and
/// code spans carry `code`.
fn collect_inlines(content: &[Inline]) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    rec(content, Style::default(), &mut out);
    return out;

    fn rec(content: &[Inline], style: Style, out: &mut Vec<(String, Style)>) {
        for node in content {
            match node {
                Inline::Text(t) => out.push((t.clone(), style)),
                Inline::Code(c) => out.push((
                    c.clone(),
                    Style {
                        code: true,
                        ..style
                    },
                )),
                Inline::Emphasis(inner) => rec(
                    inner,
                    Style {
                        italic: true,
                        ..style
                    },
                    out,
                ),
                Inline::Strong(inner) => rec(
                    inner,
                    Style {
                        bold: true,
                        ..style
                    },
                    out,
                ),
                Inline::Strikethrough(inner) => rec(
                    inner,
                    Style {
                        strike: true,
                        ..style
                    },
                    out,
                ),
                Inline::Link { content, .. } | Inline::Image { content, .. } => rec(
                    content,
                    Style {
                        link: true,
                        ..style
                    },
                    out,
                ),
                Inline::SoftBreak => out.push((" ".into(), Style::default())),
                Inline::HardBreak => out.push(("\n".into(), Style::default())),
            }
        }
    }
}

/// A wrapping atom: a word, a space between words, or a hard line break.
enum Atom<'a> {
    Word(&'a str),
    Space,
    Hard,
}

/// Split a run into wrap atoms, preserving exactly where spaces did and didn't exist (adjacent
/// styled runs must not gain a space).
fn atoms(s: &str) -> Vec<Atom<'_>> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let (mut start, mut i) = (0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'\n' => {
                if start < i {
                    out.push(Atom::Word(&s[start..i]));
                }
                out.push(Atom::Hard);
                i += 1;
                start = i;
            }
            b' ' | b'\t' => {
                if start < i {
                    out.push(Atom::Word(&s[start..i]));
                }
                out.push(Atom::Space);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < s.len() {
        out.push(Atom::Word(&s[start..]));
    }
    out
}

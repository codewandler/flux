//! The markdown AST — goldmark-style block and inline nodes.
//!
//! [`Document`] is a tree of [`Block`]s whose leaves carry [`Inline`] content. The tree is plain
//! data (public fields, `PartialEq`), so consumers extend the engine by walking it — a custom
//! renderer is a function over `&Document`, not a parser fork. [`ParseOptions`] gates the GFM
//! extensions (tables, strikethrough) the parser recognizes.

/// Options for [`crate::parser::parse_with`]. The GFM extensions flux's docs rely on are on by
/// default; turn one off to parse a stricter CommonMark-core subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptions {
    /// Recognize GFM pipe tables (header row + `|---|` delimiter row).
    pub tables: bool,
    /// Recognize GFM `~~strikethrough~~`.
    pub strikethrough: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            tables: true,
            strikethrough: true,
        }
    }
}

/// A parsed markdown document: a sequence of top-level blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    /// Depth-first walk over every block (containers included, before their children).
    pub fn walk(&self, f: &mut impl FnMut(&Block)) {
        fn rec(blocks: &[Block], f: &mut impl FnMut(&Block)) {
            for b in blocks {
                f(b);
                match b {
                    Block::BlockQuote { blocks } => rec(blocks, f),
                    Block::List(l) => {
                        for item in &l.items {
                            rec(&item.blocks, f);
                        }
                    }
                    _ => {}
                }
            }
        }
        rec(&self.blocks, f);
    }
}

/// A block-level node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A paragraph of inline content (soft/hard breaks included).
    Paragraph { content: Vec<Inline> },
    /// An ATX heading, level 1–6.
    Heading { level: u8, content: Vec<Inline> },
    /// A fenced code block: the info string (language + attrs, unparsed) and the literal lines
    /// joined with `\n` (no trailing newline; `""` for an empty block).
    CodeBlock { info: String, literal: String },
    /// A `>`-quoted container.
    BlockQuote { blocks: Vec<Block> },
    /// A bullet or ordered list.
    List(List),
    /// A horizontal rule.
    ThematicBreak,
    /// A GFM pipe table.
    Table(Table),
    /// An HTML **comment** block (`<!-- … -->`), kept verbatim. Renderers hide it (comments are
    /// invisible content); the writer re-emits it. This is the only HTML the parser recognizes —
    /// all other HTML stays literal text.
    Html { literal: String },
}

/// A list container. `tight` follows CommonMark: any blank line between the list's block elements
/// makes it loose (items render with paragraph gaps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    pub ordered: bool,
    /// Starting number for ordered lists (`1` for bullets).
    pub start: u64,
    pub tight: bool,
    /// The marker character: `-`/`*`/`+` for bullets, `.`/`)` for ordered.
    pub marker: char,
    pub items: Vec<ListItem>,
}

/// One list item: a sequence of blocks (a tight item's text is still a `Paragraph` here — tightness
/// is a render-time property of the [`List`], not a structural difference).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
}

/// A GFM pipe table: `rows[0]` is the header row; each cell is inline content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub alignments: Vec<Alignment>,
    pub rows: Vec<Vec<Vec<Inline>>>,
}

/// Column alignment from a table's delimiter row (`:---`, `:---:`, `---:`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// An inline-level node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// A run of plain text (escapes already resolved).
    Text(String),
    /// A code span (backticks stripped, interior newlines normalized to spaces).
    Code(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    /// An inline link `[content](dest "title")` or autolink `<dest>` (whose content is the dest).
    Link {
        dest: String,
        title: String,
        content: Vec<Inline>,
    },
    /// An image `![alt](src "title")`.
    Image {
        dest: String,
        title: String,
        content: Vec<Inline>,
    },
    /// A newline within a paragraph (renders as a space).
    SoftBreak,
    /// A hard line break (two trailing spaces or backslash-newline).
    HardBreak,
}

impl Inline {
    /// The visible plain text of this node and its children (breaks become spaces).
    pub fn plain_text(&self) -> String {
        fn rec(inlines: &[Inline], out: &mut String) {
            for i in inlines {
                match i {
                    Inline::Text(s) | Inline::Code(s) => out.push_str(s),
                    Inline::Emphasis(c)
                    | Inline::Strong(c)
                    | Inline::Strikethrough(c)
                    | Inline::Link { content: c, .. }
                    | Inline::Image { content: c, .. } => rec(c, out),
                    Inline::SoftBreak | Inline::HardBreak => out.push(' '),
                }
            }
        }
        let mut s = String::new();
        rec(std::slice::from_ref(self), &mut s);
        s
    }
}

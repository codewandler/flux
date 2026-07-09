//! `syntax` — the [`SyntaxKind`] alphabet for the lossless CST front-end and its
//! [`rowan::Language`] binding.
//!
//! This is the shared vocabulary of the concrete syntax tree: every **token** the [`crate::lexer`]
//! can emit plus every **node** the tolerant parser will build. Tokens and nodes live in one enum
//! because rowan keys both by a single `u16` [`rowan::SyntaxKind`]. The CST is *lossless* — the
//! token stream (including trivia and the zero-width layout markers below) reproduces the source
//! byte-for-byte — which is what gives the language server precise spans and error recovery without
//! changing the semantic [`crate::ast::Node`] AST.
//!
//! The token set is complete. The **node** set is intentionally minimal here ([`SyntaxKind::ROOT`]
//! plus [`SyntaxKind::ERROR`]) — the grammar's node kinds arrive with the tolerant parser (L-58);
//! new variants are appended before [`SyntaxKind::__LAST`] so the `u16` round-trip stays valid.

/// Every token and node kind in the Flux-Lang CST.
///
/// `#[repr(u16)]` with contiguous discriminants (no gaps) so [`FluxLang`] can round-trip a kind
/// through rowan's `u16` by a checked transmute. Keep [`SyntaxKind::__LAST`] the final variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum SyntaxKind {
    // ---- trivia (carry text; ignored by the parser but kept in the tree) ----
    /// A run of spaces/tabs within or leading a line.
    WHITESPACE = 0,
    /// A `# …` line comment (up to, but not including, the line break).
    COMMENT,
    /// A line break (`\n` or `\r\n`).
    NEWLINE,

    // ---- layout (zero-width synthetic markers; empty text, so still lossless) ----
    /// Emitted before the first token of a line indented deeper than the enclosing block.
    INDENT,
    /// Emitted before the first token of a line dedented out of a block (one per level closed).
    DEDENT,

    // ---- names & literals ----
    /// An identifier run `[A-Za-z_][A-Za-z0-9_]*` — also every (contextual) keyword; the parser
    /// classifies keywords, the lexer does not.
    IDENT,
    /// A `$symbol` reference.
    VAR,
    /// An `@annotation` (e.g. `@effect`, `@json`).
    ANNOTATION,
    /// A `"…"` or `"""…"""` string literal (one token, delimiters included).
    STRING,
    /// A numeric literal `[0-9][0-9_]*(\.[0-9]+)?`.
    NUMBER,

    // ---- single-character punctuation ----
    L_PAREN,  // (
    R_PAREN,  // )
    L_BRACK,  // [
    R_BRACK,  // ]
    L_BRACE,  // {
    R_BRACE,  // }
    COMMA,    // ,
    COLON,    // :
    DOT,      // .
    QUESTION, // ?
    PIPE,     // |
    BANG,     // !
    EQ,       // =
    PLUS,     // +
    MINUS,    // -
    STAR,     // *
    SLASH,    // /
    LT,       // <
    GT,       // >

    // ---- multi-character operators ----
    ARROW,     // ->
    PLUS_EQ,   // +=
    EQ_EQ,     // ==
    NEQ,       // !=
    LT_EQ,     // <=
    GT_EQ,     // >=
    AMP_AMP,   // &&
    PIPE_PIPE, // ||

    // ---- sentinels ----
    /// One or more bytes the lexer could not classify (kept, so the tree stays lossless).
    ERROR,
    /// End of input (never materialized into the tree; a parser convenience).
    EOF,

    // ---- nodes (expanded by the tolerant parser, L-58) ----
    /// The document root.
    ROOT,

    /// Sentinel marking the end of the enum — keep last. Not a real kind.
    #[doc(hidden)]
    __LAST,
}

impl SyntaxKind {
    /// Trivia is text the parser attaches to the tree but never branches on.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE
        )
    }

    /// The zero-width layout markers that carry the indentation grammar.
    pub fn is_layout(self) -> bool {
        matches!(self, SyntaxKind::INDENT | SyntaxKind::DEDENT)
    }
}

/// The rowan [`rowan::Language`] for Flux-Lang: the bridge between [`SyntaxKind`] and rowan's raw
/// `u16` kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FluxLang {}

impl rowan::Language for FluxLang {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        assert!(
            raw.0 < SyntaxKind::__LAST as u16,
            "raw SyntaxKind {} out of range",
            raw.0
        );
        // SAFETY: `SyntaxKind` is `#[repr(u16)]` with contiguous discriminants `0..__LAST`, and the
        // assert above bounds `raw.0` into that range.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

/// A resolved node in the red tree.
pub type SyntaxNode = rowan::SyntaxNode<FluxLang>;
/// A resolved token in the red tree.
pub type SyntaxToken = rowan::SyntaxToken<FluxLang>;
/// Either a node or a token.
pub type SyntaxElement = rowan::SyntaxElement<FluxLang>;

#[cfg(test)]
mod tests {
    use super::*;
    use rowan::Language;

    #[test]
    fn every_kind_round_trips_through_rowan_u16() {
        // Walk every real kind (0..__LAST) and confirm the raw round-trip is the identity.
        for raw in 0..(SyntaxKind::__LAST as u16) {
            let kind = FluxLang::kind_from_raw(rowan::SyntaxKind(raw));
            assert_eq!(FluxLang::kind_to_raw(kind).0, raw);
        }
    }

    #[test]
    fn trivia_and_layout_classification() {
        assert!(SyntaxKind::WHITESPACE.is_trivia());
        assert!(SyntaxKind::COMMENT.is_trivia());
        assert!(SyntaxKind::NEWLINE.is_trivia());
        assert!(!SyntaxKind::IDENT.is_trivia());
        assert!(SyntaxKind::INDENT.is_layout());
        assert!(SyntaxKind::DEDENT.is_layout());
        assert!(!SyntaxKind::NEWLINE.is_layout());
    }
}

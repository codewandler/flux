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

    // ---- nodes (built by the tolerant parser, L-58) ----
    /// The document root: a whole `.flux` module.
    ROOT,
    /// A single top-level `flow` declaration (the common case) or the module wrapper.
    FLOW_DECL,
    /// The `flow [name]([params]) [-> type]` header line.
    FLOW_HEADER,
    /// The parenthesized parameter list of a flow header.
    PARAM_LIST,
    /// One `name: Type` parameter.
    PARAM,
    /// A pure-data top-level declaration the L6 host interprets (`agent`/`channel`/`datasource`/
    /// `trigger`/`journey`/`op`). Kept as one opaque node here; the module loader owns their grammar.
    DECL,
    /// An indented block of statements (a flow/clause body).
    BLOCK,

    // statements
    BIND_STMT,       // $x = expr  /  $x: T = expr  (also `memo`)
    CALL_STMT,       // do op args  /  op(args)
    WHEN_STMT,       // when cond … [else …]
    ELSE_CLAUSE,     // else …
    UNLESS_STMT,     // unless cond …
    EACH_STMT,       // each $x in src [-> [flat] $c] …
    REPEAT_STMT,     // repeat n [-> $c] …
    UNTIL_CLAUSE,    // until cond  (first body line of repeat/loop)
    MATCH_STMT,      // match subj … (case/default arms)
    CASE_ARM,        // case v …
    DEFAULT_ARM,     // default …
    ROUTE_STMT,      // route sel … (case arms)
    FALLBACK_STMT,   // fallback [-> $b] … (branch arms)
    BRANCH_ARM,      // branch [$name] …
    PARALLEL_STMT,   // parallel … (branch arms)
    LOOP_STMT,       // loop for <ms> every <ms> [-> $b] …
    TIMEOUT_STMT,    // timeout <ms> [-> $b] …
    BUDGET_STMT,     // budget <n> [-> $b] …
    WITH_TOOLS_STMT, // with_tools [names] …
    RETRY_STMT,      // retry <n> [backoff …] [-> $b] …
    SEQ_STMT,        // seq [-> $b] …
    CTX_STMT,        // ctx $p … (purpose/include/exclude/budget sub-lines)
    CTX_APPEND_STMT, // $pack += $a, $b
    RETURN_STMT,     // return expr
    ASSERT_STMT,     // assert cond [: "msg"]
    EFFECT_ANNOT,    // @effect(tag) on the line above a bind
    JSON_ESCAPE,     // @json <compact-json> (statement position)

    // new native statements (L-60..L-63) — currently @json-only
    MEMO_STMT,       // memo $x = expr
    ONCE_STMT,       // once "label" [-> $b] …
    CHECKPOINT_STMT, // checkpoint "label"
    AWAIT_STMT,      // await [$b =] "source"
    CONFIRM_STMT,    // confirm "msg" [risk r] …
    THROTTLE_STMT,   // throttle "name" <max> per <window> …
    DEBOUNCE_STMT,   // debounce "name" <ms> …
    VERIFY_STMT,     // verify <cmd> contains <expect> [: "msg"]
    TRY_STMT,        // try … catch [$e] …
    CATCH_CLAUSE,    // catch [$e] …
    RACE_STMT,       // race <ms> [-> $b] … (branch arms)
    SCOPE_STMT,      // scope [$r = acquire] … finally …
    FINALLY_CLAUSE,  // finally …
    SAGA_STMT,       // saga … (step/undo arms)
    STEP_ARM,        // step …
    UNDO_CLAUSE,     // undo …
    PIPE_STMT,       // pipe [-> $b] … (call steps)

    // expressions
    CALL_EXPR,  // op(args…)
    VAR_EXPR,   // $sym
    FIELD_EXPR, // $sym.path  (field-access sugar → jq)
    LIT_EXPR,   // number / string / true / false / null literal
    FMT_EXPR,   // fmt("…")
    PARSE_EXPR, // parse(v, as: "…")   (L-61)
    PEEK_EXPR,  // peek $sym            (L-61)
    THING_EXPR, // thing <kind> "…"     (L-63)
    JSON_EXPR,  // @json <compact-json> (inline)
    OBJ_EXPR,   // { k: v, … }
    OBJ_FIELD,  // k: v
    LIST_EXPR,  // [ a, b, … ]
    BIN_EXPR,   // a <op> b
    UNARY_EXPR, // !a  /  -a
    PAREN_EXPR, // ( expr )
    ARG_LIST,   // (a, b, …)  or bare `a, b`
    NAME,       // an identifier in name position (op names, keys, types)

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

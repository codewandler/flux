//! `parser` — the tolerant, event-free recursive-descent parser that turns the [`crate::lexer`]
//! token stream into a lossless **rowan** green tree (stage two of the CST front-end, L-58).
//!
//! It is *total* and *error-recovering*: it never aborts. Unexpected input is wrapped in
//! [`SyntaxKind::ERROR`] nodes and the parser resyncs at the next `NEWLINE`/`DEDENT`, so an
//! in-progress buffer still yields a complete tree plus a list of [`ParseError`]s with real spans —
//! exactly what the language server needs. Every non-layout token is fed to the builder in order, so
//! the tree round-trips to the source byte-for-byte.
//!
//! The tree structures the complete writable surface: flow/composite bodies contain typed statement
//! and expression nodes; pure-data module declarations expose explicit headers and attributes; and
//! journeys contain a nested flow declaration. The semantic lowerer therefore follows CST ownership
//! directly instead of rebuilding indentation or statement boundaries from source text.

use crate::lexer::{lex, LexToken};
use crate::syntax::{FluxLang, SyntaxKind};
use rowan::{GreenNode, GreenNodeBuilder, Language, TextRange, TextSize};

/// A parse problem located at a source range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub range: TextRange,
    pub message: String,
}

/// The result of [`parse_cst`]: a lossless green tree plus recovered errors.
#[derive(Debug, Clone)]
pub struct Parse {
    pub green: GreenNode,
    pub errors: Vec<ParseError>,
}

impl Parse {
    /// The typed root node of the tree.
    pub fn syntax(&self) -> crate::syntax::SyntaxNode {
        crate::syntax::SyntaxNode::new_root(self.green.clone())
    }
}

/// Parse Flux-Lang source into a lossless CST.
pub fn parse_cst(src: &str) -> Parse {
    let lexed = lex(src);
    // Seed lexer diagnostics (unterminated strings, tab indentation, …) into the parse errors.
    let lex_errors: Vec<ParseError> = lexed
        .errors
        .iter()
        .map(|e| ParseError {
            range: e.range,
            message: e.message.clone(),
        })
        .collect();
    let mut p = Parser::new(src, lexed.tokens);
    p.errors = lex_errors;
    p.module();
    let (green, mut errors) = p.finish();
    // Keep errors in source order for stable diagnostics.
    errors.sort_by_key(|e| e.range.start());
    Parse { green, errors }
}

/// Incrementally reparse after one contiguous edit, reusing the green node of every top-level
/// declaration the edit did not touch (L-90).
///
/// The root's children are exactly the top-level declarations (`flow`/`op`/data `decl`), each
/// covering a contiguous span up to the next declaration's first token — so an edit contained
/// *strictly inside* one declaration can be answered by reparsing that declaration's text alone and
/// splicing the fresh green node back into the old root. Every other declaration's subtree is
/// shared, not rebuilt.
///
/// `replaced` is the edited range **in `old_text`** and `inserted` is the byte length of the text
/// that took its place, so `new_text` is `old_text` with `replaced` swapped for those bytes.
///
/// Returns `None` when the edit cannot be localized — the caller then does a full [`parse_cst`].
/// This is a fast path, never a semantic one: the result is validated to reproduce `new_text`
/// byte-for-byte and to carry a same-kinded declaration, so a `Some` is always the tree a full
/// reparse would have produced (pinned by `incremental_reparse_matches_full_reparse`).
pub fn reparse(
    old: &Parse,
    old_text: &str,
    new_text: &str,
    replaced: TextRange,
    inserted: usize,
) -> Option<Parse> {
    use crate::syntax::SyntaxNode;

    // The caller's edit must describe `new_text` exactly; a mismatched range means the cached tree
    // and the buffer have drifted apart, and only a full reparse can be trusted.
    if usize::from(replaced.end()) > old_text.len()
        || old_text.len() + inserted != new_text.len() + usize::from(replaced.len())
    {
        return None;
    }

    let root = SyntaxNode::new_root(old.green.clone());
    // Bail unless the root is a pure list of declaration nodes (no stray top-level tokens): the
    // child index we splice at must be the same index in `children_with_tokens`.
    if root.children_with_tokens().count() != root.children().count() {
        return None;
    }
    let (index, decl) = root.children().enumerate().find(|(_, decl)| {
        decl.text_range().start() < replaced.start() && replaced.end() < decl.text_range().end()
    })?;

    // The edited declaration's span in the *new* text: everything before it is unmoved, and the
    // edit's size delta lands inside it.
    let start = usize::from(decl.text_range().start());
    let old_end = usize::from(decl.text_range().end());
    let end = old_end + inserted - usize::from(replaced.len());
    let slice = new_text.get(start..end)?;

    // Reparse the declaration standalone. It must still be exactly one declaration of the same
    // kind spanning the whole slice — otherwise the edit changed the top-level structure (opened a
    // new declaration, dedented the body out of this one) and only a full reparse is correct.
    let fresh = parse_cst(slice);
    let mut children = fresh.green.children();
    let child = children.next()?;
    if children.next().is_some() {
        return None;
    }
    let rowan::NodeOrToken::Node(fresh_decl) = child else {
        return None;
    };
    if FluxLang::kind_from_raw(fresh_decl.kind()) != decl.kind() {
        return None;
    }

    let green = old
        .green
        .replace_child(index, rowan::NodeOrToken::Node(fresh_decl.to_owned()));
    // Losslessness guard: the spliced tree must reproduce the new buffer exactly.
    if SyntaxNode::new_root(green.clone()).text() != new_text {
        return None;
    }

    // Errors: keep the untouched declarations' errors (shifting those that follow the edit) and
    // take the reparsed declaration's errors, rebased onto the document.
    let delta = inserted as i64 - i64::from(u32::from(replaced.len()));
    let shift = |offset: TextSize| -> TextSize {
        if usize::from(offset) >= old_end {
            TextSize::new((u32::from(offset) as i64 + delta) as u32)
        } else {
            offset
        }
    };
    let mut errors: Vec<ParseError> = old
        .errors
        .iter()
        .filter(|e| usize::from(e.range.start()) < start || usize::from(e.range.start()) >= old_end)
        .map(|e| ParseError {
            range: TextRange::new(shift(e.range.start()), shift(e.range.end())),
            message: e.message.clone(),
        })
        .collect();
    let base = TextSize::new(start as u32);
    errors.extend(fresh.errors.into_iter().map(|e| ParseError {
        range: TextRange::new(e.range.start() + base, e.range.end() + base),
        message: e.message,
    }));
    errors.sort_by_key(|e| e.range.start());
    Some(Parse { green, errors })
}

struct Parser<'s> {
    src: &'s str,
    tokens: Vec<LexToken>,
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    errors: Vec<ParseError>,
    /// L-81: current expression/type recursion depth. The recursive-descent expression and type
    /// grammars have no natural bound, so deeply nested `(((…`, `!!!…`, or `List<List<…>>` input
    /// (reachable from any `.flux` source or LLM plan) would overflow the stack and `SIGABRT`.
    /// Threaded through the recursion entry points and capped at [`MAX_PARSE_DEPTH`], turning an
    /// abort into a recoverable `ParseError`.
    depth: usize,
}

/// L-81: the recursion-depth ceiling for the expression/type parsers. Each nested syntactic level
/// costs a handful of `enter()` frames, so this admits tens of real nesting levels — far more than
/// any hand-written program needs — while stopping the thousands-deep adversarial input that aborts.
// Delimited expressions now retain a little more per-level parser state (continuation trivia and
// optional named-argument ownership). Keep the guard comfortably below the default test-thread stack
// while still allowing far more nesting than authored Flux needs.
// L-114 shares this ceiling with `cst_decode`, whose own block-walk cap must stay above it.
pub(crate) const MAX_PARSE_DEPTH: usize = 128;

fn to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
    FluxLang::kind_to_raw(kind)
}

impl<'s> Parser<'s> {
    fn new(src: &'s str, tokens: Vec<LexToken>) -> Self {
        Parser {
            src,
            tokens,
            pos: 0,
            builder: GreenNodeBuilder::new(),
            errors: Vec::new(),
            depth: 0,
        }
    }

    /// L-81: enter one recursion level, returning `false` when [`MAX_PARSE_DEPTH`] is already
    /// reached (in which case the depth is *not* incremented, so no matching [`leave`](Self::leave)
    /// is owed). A caller that gets `false` must bail without recursing — recording an error and
    /// consuming a token so the parse still makes forward progress.
    fn enter(&mut self) -> bool {
        if self.depth >= MAX_PARSE_DEPTH {
            return false;
        }
        self.depth += 1;
        true
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn finish(mut self) -> (GreenNode, Vec<ParseError>) {
        (self.builder.finish(), std::mem::take(&mut self.errors))
    }

    // --- token access ---------------------------------------------------

    fn raw_kind_at(&self, i: usize) -> SyntaxKind {
        self.tokens
            .get(i)
            .map(|t| t.kind)
            .unwrap_or(SyntaxKind::EOF)
    }

    /// Kind of the `n`-th *significant* token from `pos` (skips WHITESPACE/COMMENT;
    /// NEWLINE/INDENT/DEDENT are significant). `EOF` past the end.
    fn nth(&self, n: usize) -> SyntaxKind {
        let mut seen = 0;
        let mut i = self.pos;
        loop {
            let k = self.raw_kind_at(i);
            if k == SyntaxKind::EOF {
                return SyntaxKind::EOF;
            }
            if !matches!(k, SyntaxKind::WHITESPACE | SyntaxKind::COMMENT) {
                if seen == n {
                    return k;
                }
                seen += 1;
            }
            i += 1;
        }
    }

    fn at(&self, k: SyntaxKind) -> bool {
        self.nth(0) == k
    }

    fn at_eof(&self) -> bool {
        self.nth(0) == SyntaxKind::EOF
    }

    /// The source text of the current significant token (for keyword dispatch / names).
    fn cur_text(&self) -> &'s str {
        let mut i = self.pos;
        loop {
            let k = self.raw_kind_at(i);
            if k == SyntaxKind::EOF {
                return "";
            }
            if !matches!(k, SyntaxKind::WHITESPACE | SyntaxKind::COMMENT) {
                return &self.src[self.tokens[i].range];
            }
            i += 1;
        }
    }

    fn at_kw(&self, kw: &str) -> bool {
        self.nth(0) == SyntaxKind::IDENT && self.cur_text() == kw
    }

    // --- header/block boundary lookahead --------------------------------
    //
    // Blank and comment-only lines are semantically insignificant at a header→block or
    // block→clause boundary (`flow f` + blank + body; `try` body + blank + `catch`). `nth`/`at`
    // deliberately treat NEWLINE as significant (the grammar is line-oriented), so boundaries use
    // these dedicated helpers instead.

    /// True when the next non-blank content is an INDENT — an indented block follows, possibly
    /// after blank or comment-only lines.
    fn at_block(&self) -> bool {
        let mut i = self.pos;
        loop {
            match self.raw_kind_at(i) {
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE => i += 1,
                k => return k == SyntaxKind::INDENT,
            }
        }
    }

    /// `at_kw` looking across blank/comment-only lines (clause keywords after a block's DEDENT).
    fn at_kw_past_blank(&self, kw: &str) -> bool {
        let mut i = self.pos;
        loop {
            match self.raw_kind_at(i) {
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE => i += 1,
                SyntaxKind::IDENT => return &self.src[self.tokens[i].range] == kw,
                _ => return false,
            }
        }
    }

    /// Feed the blank/comment run up to the boundary content — call only after `at_block` /
    /// `at_kw_past_blank` committed to crossing it (the trivia attaches to the current node).
    fn skip_blank_lines(&mut self) {
        while matches!(
            self.raw_kind_at(self.pos),
            SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE
        ) {
            self.feed_one();
        }
    }

    /// The boundary idiom: if an indented block follows (past blanks), enter it via `f`.
    ///
    /// L-114: this is the single cut through the statement-nesting recursion — every block a
    /// statement owns is entered here, and the cycle is `block` → `statement` → a block statement →
    /// here — so it carries the same [`MAX_PARSE_DEPTH`] guard the expression grammar uses. Over the
    /// cap the block is swallowed whole by [`error_block`](Self::error_block) instead of recursed
    /// into, which keeps the tree lossless *and* shallow enough for every consumer that walks it
    /// recursively (semantic lowering, the formatter, the language server).
    fn block_if_indented(&mut self, f: impl FnOnce(&mut Self)) {
        if !self.at_block() {
            return;
        }
        self.skip_blank_lines();
        if !self.enter() {
            self.error_block("statement nesting too deep");
            return;
        }
        f(self);
        self.leave();
    }

    /// Swallow one whole indented region — the INDENT at the cursor through its matching DEDENT —
    /// into a single `ERROR` node, iteratively. The depth guard's bail-out: it makes forward
    /// progress without recursing, so the declarations after the over-deep block still parse.
    fn error_block(&mut self, message: &str) {
        self.error(message);
        self.start(SyntaxKind::ERROR);
        let mut nesting = 0usize;
        loop {
            match self.raw_kind_at(self.pos) {
                SyntaxKind::EOF => break,
                SyntaxKind::INDENT => {
                    nesting += 1;
                    self.feed_one();
                }
                SyntaxKind::DEDENT => {
                    nesting = nesting.saturating_sub(1);
                    self.feed_one();
                    if nesting == 0 {
                        break;
                    }
                }
                _ => self.feed_one(),
            }
        }
        self.finish_node();
    }

    // --- builder feeding ------------------------------------------------

    fn feed_one(&mut self) {
        let t = &self.tokens[self.pos];
        if !t.kind.is_layout() {
            self.builder.token(to_raw(t.kind), &self.src[t.range]);
        }
        self.pos += 1;
    }

    /// Feed leading WHITESPACE/COMMENT trivia into the current node.
    fn eat_trivia(&mut self) {
        while matches!(
            self.raw_kind_at(self.pos),
            SyntaxKind::WHITESPACE | SyntaxKind::COMMENT
        ) {
            self.feed_one();
        }
    }

    /// Consume the current significant token into the tree.
    fn bump(&mut self) {
        self.eat_trivia();
        if self.raw_kind_at(self.pos) != SyntaxKind::EOF {
            self.feed_one();
        }
    }

    fn eat(&mut self, k: SyntaxKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn start(&mut self, k: SyntaxKind) {
        self.eat_trivia();
        self.builder.start_node(to_raw(k));
    }

    fn checkpoint(&mut self) -> rowan::Checkpoint {
        self.eat_trivia();
        self.builder.checkpoint()
    }

    fn wrap(&mut self, cp: rowan::Checkpoint, k: SyntaxKind) {
        self.builder.start_node_at(cp, to_raw(k));
        self.builder.finish_node();
    }

    fn finish_node(&mut self) {
        self.builder.finish_node();
    }

    fn error(&mut self, message: impl Into<String>) {
        let range = self
            .tokens
            .get(self.pos)
            .map(|t| t.range)
            .unwrap_or_else(|| TextRange::empty(TextSize::new(self.src.len() as u32)));
        self.errors.push(ParseError {
            range,
            message: message.into(),
        });
    }

    /// Wrap the current significant token in an ERROR node and consume it, recording `message`.
    fn err_and_bump(&mut self, message: impl Into<String>) {
        self.error(message);
        self.start(SyntaxKind::ERROR);
        self.bump();
        self.finish_node();
    }

    fn expect(&mut self, k: SyntaxKind, what: &str) {
        if !self.eat(k) {
            self.error(format!("expected {what}"));
        }
    }

    // --- structural helpers --------------------------------------------

    fn at_newline(&self) -> bool {
        // A DEDENT is a line end too: the last line of a block (or of an unterminated file) is
        // followed by DEDENT rather than NEWLINE, and header parsing must stop there the same way.
        matches!(self.nth(0), SyntaxKind::NEWLINE | SyntaxKind::DEDENT)
    }

    /// Consume tokens up to and including the next NEWLINE (or EOF), feeding them to the current
    /// node. Used to finish a statement header line (and to recover a bad line).
    fn eat_to_end_of_line(&mut self) {
        while !self.at_eof() && !self.at_newline() {
            self.bump();
        }
        self.eat(SyntaxKind::NEWLINE);
    }

    // --- canonical named-option header tails (L-96) ----------------------

    /// Consume a control header's positional operands: every token up to the first *top-level*
    /// comma, which is where the `, name: value` option tail starts. Bracketed commas
    /// (`with_tools ["a", "b"]`) belong to the operand, so nesting is tracked.
    fn eat_positional_operands(&mut self) {
        let mut depth = 0usize;
        while !self.at_eof() && !self.at_newline() {
            match self.nth(0) {
                SyntaxKind::COMMA if depth == 0 => return,
                SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE => depth += 1,
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE => {
                    depth = depth.saturating_sub(1)
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Parse the canonical `, name: value` option tail of a control header (L-96), one
    /// [`SyntaxKind::HEADER_OPTION`] node per option — leading comma included, so the text on
    /// either side of the run is exactly the legacy space-keyword header.
    ///
    /// `expr_valued` names the options whose value is a full expression (`until`, `when`) and is
    /// therefore parsed into a real expression node. Every other value is a scalar the semantic
    /// lowerer decodes from its tokens (a duration like `500ms` is *not* one expression), so it is
    /// captured verbatim up to the next top-level `,` / `->` / end of line.
    /// The start of an option run that is *not* preceded by a comma — a header whose only value is
    /// named, with no positional operand in front of it (`race timeout: 5s`).
    fn at_option_start(&self) -> bool {
        self.at(SyntaxKind::IDENT) && self.nth(1) == SyntaxKind::COLON
    }

    fn header_options(&mut self, expr_valued: &[&str]) {
        while self.at(SyntaxKind::COMMA) || self.at_option_start() {
            self.start(SyntaxKind::HEADER_OPTION);
            self.eat(SyntaxKind::COMMA);
            let name = self.cur_text().to_string();
            self.expect(SyntaxKind::IDENT, "an option name after `,`");
            self.expect(SyntaxKind::COLON, "`:` after an option name");
            if expr_valued.contains(&name.as_str()) {
                self.expr(0);
            } else {
                self.eat_option_value();
            }
            self.finish_node();
        }
    }

    /// A scalar option value: tokens up to the next top-level `,` / `->` / end of line.
    fn eat_option_value(&mut self) {
        let mut depth = 0usize;
        while !self.at_eof() && !self.at_newline() {
            match self.nth(0) {
                SyntaxKind::COMMA | SyntaxKind::ARROW if depth == 0 => return,
                SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_BRACE => depth += 1,
                SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_BRACE => {
                    depth = depth.saturating_sub(1)
                }
                _ => {}
            }
            self.bump();
        }
    }

    // --- grammar --------------------------------------------------------

    fn module(&mut self) {
        self.builder.start_node(to_raw(SyntaxKind::ROOT));
        loop {
            self.eat_trivia();
            match self.nth(0) {
                SyntaxKind::EOF => break,
                SyntaxKind::NEWLINE => self.bump(), // blank line
                SyntaxKind::INDENT | SyntaxKind::DEDENT => {
                    // Stray layout at top level — consume without emitting (should not happen for
                    // well-formed input; keeps the parser total).
                    self.pos += 1;
                }
                SyntaxKind::IDENT if self.at_kw("flow") => self.flow_decl(),
                SyntaxKind::IDENT if self.at_kw("op") => self.op_decl(),
                SyntaxKind::IDENT if self.at_decl_kw() => self.data_decl(),
                _ => {
                    // Unexpected top-level content: recover a line at a time.
                    self.start(SyntaxKind::ERROR);
                    if self.at(SyntaxKind::IDENT) {
                        self.error(format!(
                            "unknown top-level declaration: `{}`",
                            self.cur_text()
                        ));
                    } else {
                        self.error("expected a `flow` header or a top-level declaration");
                    }
                    self.eat_to_end_of_line();
                    self.finish_node();
                }
            }
        }
        self.builder.finish_node();
    }

    fn at_decl_kw(&self) -> bool {
        matches!(
            self.cur_text(),
            "goal"
                | "permissions"
                | "agent_loop"
                | "agent"
                | "channel"
                | "datasource"
                | "trigger"
                | "journey"
        )
    }

    /// A pure-data top-level declaration. Its header and attribute lines are explicit CST nodes;
    /// `agent_loop` owns a normal statement block and `journey` may own a nested flow declaration.
    /// This is enough structure for semantic lowering without reconstructing indentation lines.
    fn data_decl(&mut self) {
        let keyword = self.cur_text().to_string();
        self.start(SyntaxKind::DECL);
        self.start(SyntaxKind::DECL_HEADER);
        self.eat_to_end_of_line();
        self.finish_node();
        match keyword.as_str() {
            "agent_loop" => self.block_if_indented(Self::block),
            "journey" => self.block_if_indented(Self::journey_block),
            _ => self.block_if_indented(Self::declaration_attr_block),
        }
        self.finish_node();
    }

    fn declaration_attr_block(&mut self) {
        self.start(SyntaxKind::BLOCK);
        self.pos += 1; // INDENT
        loop {
            self.eat_trivia();
            match self.nth(0) {
                SyntaxKind::DEDENT => {
                    self.pos += 1;
                    break;
                }
                SyntaxKind::EOF => break,
                SyntaxKind::NEWLINE => self.bump(),
                SyntaxKind::INDENT => {
                    // Keep malformed nested declaration content lossless and recover at its
                    // matching dedent. Strict lowering rejects the parser error; editor users still
                    // receive the complete tree.
                    self.error("declaration attributes cannot contain nested blocks");
                    self.pos += 1;
                    self.start(SyntaxKind::ERROR);
                    while !self.at_eof() && !self.at(SyntaxKind::DEDENT) {
                        self.bump();
                    }
                    if self.at(SyntaxKind::DEDENT) {
                        self.pos += 1;
                    }
                    self.finish_node();
                }
                _ => {
                    self.start(SyntaxKind::DECL_ATTR);
                    self.eat_to_end_of_line();
                    self.finish_node();
                }
            }
        }
        self.finish_node();
    }

    fn journey_block(&mut self) {
        self.start(SyntaxKind::BLOCK);
        self.pos += 1; // INDENT
        loop {
            self.eat_trivia();
            match self.nth(0) {
                SyntaxKind::DEDENT => {
                    self.pos += 1;
                    break;
                }
                SyntaxKind::EOF => break,
                SyntaxKind::NEWLINE => self.bump(),
                SyntaxKind::IDENT if self.at_kw("flow") => self.flow_decl(),
                _ => {
                    self.start(SyntaxKind::DECL_ATTR);
                    self.eat_to_end_of_line();
                    self.finish_node();
                }
            }
        }
        self.finish_node();
    }

    fn flow_decl(&mut self) {
        self.start(SyntaxKind::FLOW_DECL);
        self.flow_header();
        // Column-0 `goal "…"` directive lines are tolerated-and-ignored by semantic lowering —
        // they may appear between the header and the body (or between body chunks). Consume each
        // as an opaque line; a body block may follow any of them.
        loop {
            if self.at_kw_past_blank("goal") {
                self.skip_blank_lines();
                self.start(SyntaxKind::DECL);
                self.eat_to_end_of_line();
                self.finish_node();
                continue;
            }
            if self.at_block() {
                self.skip_blank_lines();
                self.block();
                continue;
            }
            break;
        }
        self.finish_node();
    }

    fn op_decl(&mut self) {
        self.start(SyntaxKind::OP_DECL);
        self.callable_header(SyntaxKind::OP_HEADER);
        if self.at_block() {
            self.skip_blank_lines();
            self.op_block();
        }
        self.finish_node();
    }

    fn flow_header(&mut self) {
        self.callable_header(SyntaxKind::FLOW_HEADER);
    }

    /// Shared header grammar for `flow` and composite `op` declarations.
    fn callable_header(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.bump(); // `flow` / `op`
        if matches!(self.nth(0), SyntaxKind::IDENT | SyntaxKind::NUMBER) {
            self.decl_name(); // optional alphanumeric/underscore/kebab declaration name
        }
        if self.at(SyntaxKind::L_PAREN) {
            self.param_list();
        }
        if self.at(SyntaxKind::ARROW) {
            self.bump();
            self.type_ref();
        }
        self.eat(SyntaxKind::NEWLINE);
        self.finish_node();
    }

    /// A composite body starts with zero or more flat metadata lines, followed by ordinary
    /// Flux-Lang statements at the same indentation. Metadata stays in the lossless tree but is not
    /// a statement, so the AST↔CST range walk pairs only the executable body.
    fn op_block(&mut self) {
        self.start(SyntaxKind::BLOCK);
        self.pos += 1; // INDENT (not emitted into the tree)
        let mut in_meta = true;
        loop {
            self.eat_trivia();
            match self.nth(0) {
                SyntaxKind::DEDENT => {
                    self.pos += 1;
                    break;
                }
                SyntaxKind::EOF => break,
                SyntaxKind::NEWLINE => self.bump(),
                SyntaxKind::IDENT if in_meta && self.at_composite_meta() => {
                    self.start(SyntaxKind::OP_META);
                    self.eat_to_end_of_line();
                    self.finish_node();
                }
                _ => {
                    in_meta = false;
                    self.statement();
                }
            }
        }
        self.finish_node();
    }

    fn at_composite_meta(&self) -> bool {
        matches!(
            self.cur_text(),
            "description" | "risk" | "idempotency" | "effects" | "limits" | "expose" | "view"
        )
    }

    fn param_list(&mut self) {
        self.start(SyntaxKind::PARAM_LIST);
        self.bump(); // (
        while !self.at(SyntaxKind::R_PAREN) && !self.at_eof() && !self.at_newline() {
            self.start(SyntaxKind::PARAM);
            if !self.decl_name() {
                self.error("expected a parameter name");
            }
            self.expect(SyntaxKind::COLON, "`:` after the parameter name");
            self.type_ref();
            self.finish_node();
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        self.expect(SyntaxKind::R_PAREN, "`)` to close the parameter list");
        self.finish_node();
    }

    /// A type reference: `Any`/`Bool`/`Number`/`String`, `List<T>`, or a named type. Tokenized as an
    /// IDENT possibly followed by `< … >`; captured as a TYPE reference span for now.
    fn type_ref(&mut self) {
        // L-81: `List<List<…>>` nests unboundedly; stop before the stack overflows.
        if !self.enter() {
            self.error("type nesting too deep");
            return;
        }
        self.start(SyntaxKind::NAME);
        if !self.decl_name() {
            self.error("expected a type name");
        }
        if self.eat(SyntaxKind::LT) {
            self.type_ref();
            self.expect(SyntaxKind::GT, "`>` to close the `List<…>` type");
        }
        self.finish_node();
        self.leave();
    }

    /// Consume one declaration-name token run. Flux declaration names allow ASCII letters, digits,
    /// `_`, and `-`; the lexer may therefore split a legal name such as `9lives-v2` into adjacent
    /// NUMBER/IDENT/MINUS tokens. Only raw-adjacent tokens join the name — trivia ends it.
    fn decl_name(&mut self) -> bool {
        self.eat_trivia();
        if !matches!(
            self.raw_kind_at(self.pos),
            SyntaxKind::IDENT | SyntaxKind::NUMBER
        ) {
            return false;
        }
        self.feed_one();
        loop {
            match self.raw_kind_at(self.pos) {
                SyntaxKind::IDENT | SyntaxKind::NUMBER => self.feed_one(),
                SyntaxKind::MINUS
                    if matches!(
                        self.raw_kind_at(self.pos + 1),
                        SyntaxKind::IDENT | SyntaxKind::NUMBER
                    ) =>
                {
                    self.feed_one();
                    self.feed_one();
                }
                _ => break,
            }
        }
        true
    }

    /// An indented block of statements: INDENT stmt* DEDENT.
    fn block(&mut self) {
        self.start(SyntaxKind::BLOCK);
        self.pos += 1; // INDENT (not emitted into the tree)
        loop {
            self.eat_trivia();
            match self.nth(0) {
                SyntaxKind::DEDENT => {
                    self.pos += 1; // consume the matching DEDENT
                    break;
                }
                SyntaxKind::EOF => break,
                SyntaxKind::NEWLINE => self.bump(), // blank line inside the block
                _ => self.statement(),
            }
        }
        self.finish_node();
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    fn statement(&mut self) {
        // `$sym = …` / `$sym += …` bind, or a `$sym`-led expression statement.
        if self.at(SyntaxKind::VAR) {
            return self.bind_or_expr_stmt();
        }
        if self.at(SyntaxKind::ANNOTATION) {
            let txt = self.cur_text();
            if txt == "@effect" {
                return self.effect_annotated();
            }
            if txt == "@json" {
                self.start(SyntaxKind::JSON_ESCAPE);
                self.eat_to_end_of_line();
                self.finish_node();
                return;
            }
        }
        if self.at(SyntaxKind::IDENT) {
            let kw = self.cur_text();
            if crate::ast::is_bare_symbol_name(kw)
                && matches!(
                    self.nth(1),
                    SyntaxKind::EQ | SyntaxKind::COLON | SyntaxKind::PLUS_EQ
                )
            {
                return self.bind_or_expr_stmt();
            }
            match kw {
                "do" => return self.do_call_stmt(),
                "when" => return self.when_stmt(),
                "unless" => return self.simple_block_stmt(SyntaxKind::UNLESS_STMT, true),
                "each" => return self.each_stmt(),
                "repeat" => return self.repeat_like(SyntaxKind::REPEAT_STMT),
                "loop" => return self.repeat_like(SyntaxKind::LOOP_STMT),
                "match" => return self.match_like(SyntaxKind::MATCH_STMT),
                "route" => return self.match_like(SyntaxKind::ROUTE_STMT),
                "fallback" => return self.branch_block(SyntaxKind::FALLBACK_STMT),
                "parallel" => return self.branch_block(SyntaxKind::PARALLEL_STMT),
                "race" => return self.branch_block(SyntaxKind::RACE_STMT),
                "timeout" => return self.simple_block_stmt(SyntaxKind::TIMEOUT_STMT, false),
                "budget" => return self.simple_block_stmt(SyntaxKind::BUDGET_STMT, false),
                "with_tools" => return self.simple_block_stmt(SyntaxKind::WITH_TOOLS_STMT, false),
                "retry" => return self.option_block_stmt(SyntaxKind::RETRY_STMT),
                "seq" => return self.simple_block_stmt(SyntaxKind::SEQ_STMT, false),
                "ctx" => return self.ctx_stmt(),
                "return" => return self.return_stmt(),
                "assert" => return self.assert_stmt(),
                "memo" => return self.memo_stmt(),
                "once" => return self.simple_block_stmt(SyntaxKind::ONCE_STMT, false),
                "checkpoint" => return self.header_only_stmt(SyntaxKind::CHECKPOINT_STMT),
                "await" => return self.await_stmt(),
                "confirm" => return self.option_block_stmt(SyntaxKind::CONFIRM_STMT),
                "throttle" => return self.option_block_stmt(SyntaxKind::THROTTLE_STMT),
                "debounce" => return self.option_block_stmt(SyntaxKind::DEBOUNCE_STMT),
                "verify" => return self.verify_stmt(),
                "try" => return self.try_stmt(),
                "scope" => return self.scope_stmt(),
                "saga" => return self.saga_stmt(),
                "pipe" => return self.simple_block_stmt(SyntaxKind::PIPE_STMT, false),
                "else" => return self.orphan_clause("`else` without a matching `when`"),
                _ => {}
            }
        }
        // Otherwise: a bare call (`op(args)`) or an expression statement.
        self.expr_stmt();
    }

    fn orphan_clause(&mut self, message: &str) {
        self.start(SyntaxKind::ERROR);
        self.error(message);
        self.eat_to_end_of_line();
        self.finish_node();
    }

    /// `$x = expr` / `$x: T = expr` / `$x += a, b` / a `$x`-led expression, dispatched on the token
    /// after the leading `$var`.
    fn bind_or_expr_stmt(&mut self) {
        match self.nth(1) {
            SyntaxKind::PLUS_EQ => {
                self.start(SyntaxKind::CTX_APPEND_STMT);
                self.bump(); // $var / bare name
                self.bump(); // +=
                self.expr_list_to_eol();
                self.eat(SyntaxKind::NEWLINE);
                self.finish_node();
            }
            SyntaxKind::EQ | SyntaxKind::COLON => {
                self.start(SyntaxKind::BIND_STMT);
                self.bump(); // $var / bare name
                if self.eat(SyntaxKind::COLON) {
                    self.type_ref();
                }
                self.expect(SyntaxKind::EQ, "`=` in a bind");
                let missing_rhs = self.at_newline() || self.at_eof();
                self.expr(0);
                if !missing_rhs {
                    if !self.at_newline() && !self.at_eof() {
                        self.error("unexpected trailing tokens");
                        self.eat_to_end_of_line();
                    } else {
                        self.eat(SyntaxKind::NEWLINE);
                    }
                }
                // With a missing RHS, `expr` owns the recovery newline. Leave the following line
                // to the block parser so the tolerant CST retains later statements.
                self.finish_node();
            }
            _ => self.expr_stmt(),
        }
    }

    fn expr_list_to_eol(&mut self) {
        // An empty list is legal (`$pack +=` appends nothing; `do op` with no args).
        if self.at_newline() || self.at_eof() {
            return;
        }
        loop {
            self.expr(0);
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
    }

    fn effect_annotated(&mut self) {
        // `@effect(tag)` header line, then the bind it annotates on the next line.
        self.start(SyntaxKind::EFFECT_ANNOT);
        self.eat_to_end_of_line();
        self.finish_node();
        // The following statement is parsed normally by the block loop.
    }

    fn do_call_stmt(&mut self) {
        self.start(SyntaxKind::CALL_STMT);
        self.bump(); // `do`
                     // `do op arg, arg` — the op name then a comma-separated arg list to end of line.
        self.start(SyntaxKind::NAME);
        self.expect(SyntaxKind::IDENT, "an operation name after `do`");
        while matches!(self.nth(0), SyntaxKind::DOT | SyntaxKind::MINUS)
            && self.nth(1) == SyntaxKind::IDENT
        {
            self.bump(); // . / -
            self.bump(); // ident — dotted/hyphenated operation names are one NAME
        }
        self.finish_node();
        if !self.at_newline() && !self.at_eof() {
            self.start(SyntaxKind::ARG_LIST);
            self.expr_list_to_eol();
            self.finish_node();
        }
        self.eat(SyntaxKind::NEWLINE);
        self.finish_node();
    }

    fn when_stmt(&mut self) {
        self.start(SyntaxKind::WHEN_STMT);
        self.bump(); // when
        self.expr(0); // condition
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block);
        // optional `else` clause at the same level (blank/comment-only lines before it are
        // insignificant, exactly as elsewhere in the CST grammar)
        if self.at_kw_past_blank("else") {
            self.skip_blank_lines();
            self.start(SyntaxKind::ELSE_CLAUSE);
            self.bump();
            self.eat(SyntaxKind::NEWLINE);
            self.block_if_indented(Self::block);
            self.finish_node();
        }
        self.finish_node();
    }

    fn each_stmt(&mut self) {
        self.start(SyntaxKind::EACH_STMT);
        self.bump(); // each
        self.expect_symbol("the loop variable, e.g. `x`");
        if self.at_kw("in") {
            self.bump();
        } else {
            self.error("expected `in` in an `each` header");
        }
        self.expr(0); // source
        if self.eat(SyntaxKind::ARROW) {
            if self.at_kw("flat") {
                self.bump();
            }
            self.expect_symbol("a collect variable after `->`");
        }
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block);
        self.finish_node();
    }

    /// `repeat`/`loop`: the count/interval operands, an optional `, until: <cond>` (and, for
    /// `loop`, `, every: <ms>`) option tail, the `-> $bind` clause, then a body whose first line
    /// may still be the legacy `until <cond>` clause.
    fn repeat_like(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.bump(); // repeat / loop
        self.eat_positional_operands();
        self.header_options(&["until"]);
        while !self.at_newline() && !self.at_eof() {
            self.bump(); // `-> $bind`
        }
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block_with_optional_until);
        self.finish_node();
    }

    fn block_with_optional_until(&mut self) {
        self.start(SyntaxKind::BLOCK);
        self.pos += 1; // INDENT
        if self.at_kw_past_blank("until") {
            self.skip_blank_lines();
            self.start(SyntaxKind::UNTIL_CLAUSE);
            self.bump(); // until
            self.expr(0);
            self.eat(SyntaxKind::NEWLINE);
            self.finish_node();
        }
        loop {
            self.eat_trivia();
            match self.nth(0) {
                SyntaxKind::DEDENT => {
                    self.pos += 1;
                    break;
                }
                SyntaxKind::EOF => break,
                SyntaxKind::NEWLINE => self.bump(),
                _ => self.statement(),
            }
        }
        self.finish_node();
    }

    /// `match`/`route`: header to end of line, then a body of `case …` / `default` arms.
    fn match_like(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.bump(); // match / route
        self.expr(0);
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(|p| p.arm_block(&["case", "default"]));
        self.finish_node();
    }

    fn branch_block(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.bump(); // parallel / race / fallback
        if !self.at_option_start() {
            // `race 5s` spells its timeout positionally; `race timeout: 5s` names it instead.
            self.eat_positional_operands();
        }
        self.header_options(&[]);
        while !self.at_newline() && !self.at_eof() {
            self.bump(); // `-> $bind`
        }
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(|p| p.arm_block(&["branch"]));
        self.finish_node();
    }

    /// A block whose direct children are arm headers (`case`/`default`/`branch`/`step`/…) each with
    /// their own indented body.
    fn arm_block(&mut self, arms: &[&str]) {
        self.start(SyntaxKind::BLOCK);
        self.pos += 1; // INDENT
        loop {
            self.eat_trivia();
            match self.nth(0) {
                SyntaxKind::DEDENT => {
                    self.pos += 1;
                    break;
                }
                SyntaxKind::EOF => break,
                SyntaxKind::NEWLINE => self.bump(),
                SyntaxKind::IDENT if arms.contains(&self.cur_text()) => {
                    let kind = arm_kind(self.cur_text());
                    self.start(kind);
                    self.bump(); // case / default / branch / step / undo
                    if kind == SyntaxKind::CASE_ARM {
                        self.expr(0);
                        self.eat(SyntaxKind::NEWLINE);
                    } else {
                        self.eat_to_end_of_line();
                    }
                    self.block_if_indented(Self::block);
                    self.finish_node();
                }
                _ => {
                    // Statements directly under an arm-block that aren't arms are recovered.
                    self.statement();
                }
            }
        }
        self.finish_node();
    }

    fn try_stmt(&mut self) {
        self.start(SyntaxKind::TRY_STMT);
        self.bump(); // try
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block);
        if self.at_kw_past_blank("catch") {
            self.skip_blank_lines();
            self.start(SyntaxKind::CATCH_CLAUSE);
            self.bump();
            if self.at_symbol() {
                self.bump();
            }
            self.eat(SyntaxKind::NEWLINE);
            self.block_if_indented(Self::block);
            self.finish_node();
        }
        self.finish_node();
    }

    fn scope_stmt(&mut self) {
        self.start(SyntaxKind::SCOPE_STMT);
        self.bump(); // scope
        if self.at_symbol() && self.nth(1) == SyntaxKind::EQ {
            self.bump();
            self.expect(SyntaxKind::EQ, "`=` after the scope binding");
            self.expr(0);
        }
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block);
        if self.at_kw_past_blank("finally") {
            self.skip_blank_lines();
            self.start(SyntaxKind::FINALLY_CLAUSE);
            self.bump();
            self.eat(SyntaxKind::NEWLINE);
            self.block_if_indented(Self::block);
            self.finish_node();
        }
        self.finish_node();
    }

    fn saga_stmt(&mut self) {
        self.start(SyntaxKind::SAGA_STMT);
        self.eat_to_end_of_line(); // saga
        self.block_if_indented(|p| p.arm_block(&["step", "undo"]));
        self.finish_node();
    }

    fn memo_stmt(&mut self) {
        self.start(SyntaxKind::MEMO_STMT);
        self.bump(); // memo
        self.expect_symbol("a symbol after `memo`");
        if self.eat(SyntaxKind::COLON) {
            self.type_ref();
        }
        self.expect(SyntaxKind::EQ, "`=` in a `memo` bind");
        self.expr(0);
        self.eat(SyntaxKind::NEWLINE);
        self.finish_node();
    }

    fn return_stmt(&mut self) {
        self.start(SyntaxKind::RETURN_STMT);
        self.bump(); // return
        if !self.at_newline() && !self.at_eof() {
            self.expr(0);
        }
        self.eat(SyntaxKind::NEWLINE);
        self.finish_node();
    }

    fn assert_stmt(&mut self) {
        self.start(SyntaxKind::ASSERT_STMT);
        self.bump(); // assert
        self.expr(0);
        if self.eat(SyntaxKind::COMMA) {
            self.expect(SyntaxKind::STRING, "a quoted `assert` message");
        }
        self.eat(SyntaxKind::NEWLINE);
        self.finish_node();
    }

    fn await_stmt(&mut self) {
        self.start(SyntaxKind::AWAIT_STMT);
        self.bump(); // await
        if self.at_symbol() && matches!(self.nth(1), SyntaxKind::COLON | SyntaxKind::EQ) {
            self.bump();
            if self.eat(SyntaxKind::COLON) {
                self.type_ref();
            }
            self.expect(SyntaxKind::EQ, "`=` after the await binding");
        }
        self.expect(SyntaxKind::STRING, "an await source string");
        if self.at_kw("when") {
            self.bump();
            self.expr(0);
        }
        self.header_options(&["when"]);
        self.eat(SyntaxKind::NEWLINE);
        self.finish_node();
    }

    fn verify_stmt(&mut self) {
        self.start(SyntaxKind::VERIFY_STMT);
        self.bump(); // verify
        self.expr(0);
        if self.at_kw("contains") {
            self.bump();
        } else {
            self.error("expected `contains` in `verify`");
        }
        self.expr(0);
        if self.eat(SyntaxKind::COLON) {
            self.expect(SyntaxKind::STRING, "a quoted `verify` message");
        }
        self.eat(SyntaxKind::NEWLINE);
        self.finish_node();
    }

    /// A statement whose header may carry inline expressions (`assert cond : "msg"`,
    /// `checkpoint "l"`, `verify … contains …`), captured to end of line, with no body.
    fn header_only_stmt(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.eat_to_end_of_line();
        self.finish_node();
    }

    /// A statement with a header line (optionally carrying an inline condition when `parse_cond`) and
    /// an indented body: `unless cond … / timeout ms … / with_tools […] …` and the new guard-rails.
    /// `ctx $pack` + an indented block of `purpose`/`include`/`exclude`/`budget` sub-lines. The
    /// sub-lines are NOT statements — each becomes one opaque [`SyntaxKind::CTX_ENTRY`] node so the
    /// generic statement grammar never sees (and mis-flags) them.
    fn ctx_stmt(&mut self) {
        self.start(SyntaxKind::CTX_STMT);
        self.bump(); // `ctx`
        while !self.at_newline() && !self.at_eof() {
            self.bump(); // header rest (`$pack`), captured verbatim
        }
        self.eat(SyntaxKind::NEWLINE);
        if self.at_block() {
            self.skip_blank_lines();
            self.start(SyntaxKind::BLOCK);
            self.pos += 1; // INDENT
            loop {
                self.eat_trivia();
                match self.nth(0) {
                    SyntaxKind::DEDENT => {
                        self.pos += 1;
                        break;
                    }
                    SyntaxKind::EOF => break,
                    SyntaxKind::NEWLINE => self.bump(),
                    _ => {
                        self.start(SyntaxKind::CTX_ENTRY);
                        self.eat_to_end_of_line();
                        self.finish_node();
                    }
                }
            }
            self.finish_node();
        }
        self.finish_node();
    }

    /// A header statement whose non-primary values use the canonical `, name: value` vocabulary
    /// (`confirm`/`retry`/`throttle`/`debounce`, L-96): positional operands, then the structured
    /// option tail, then the verbatim `-> $bind` clause, then an indented body.
    fn option_block_stmt(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.bump(); // the keyword
        self.eat_positional_operands();
        self.header_options(&[]);
        while !self.at_newline() && !self.at_eof() {
            self.bump(); // `-> $bind` and anything else the lowerer must reject
        }
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block);
        self.finish_node();
    }

    fn simple_block_stmt(&mut self, kind: SyntaxKind, parse_cond: bool) {
        self.start(kind);
        self.bump(); // the keyword
        if parse_cond && !self.at_newline() && !self.at_eof() {
            self.expr(0);
        } else {
            // header args captured verbatim to end of line
            while !self.at_newline() && !self.at_eof() {
                self.bump();
            }
        }
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block);
        self.finish_node();
    }

    fn expr_stmt(&mut self) {
        let cp = self.checkpoint();
        if self.at_newline() || self.at_eof() {
            self.err_and_bump("expected a statement");
            return;
        }
        self.expr(0);
        self.wrap(cp, SyntaxKind::CALL_STMT);
        // Recover any trailing tokens on the line, then the newline.
        if !self.at_newline() && !self.at_eof() {
            self.error("unexpected trailing tokens");
            self.eat_to_end_of_line();
        } else {
            self.eat(SyntaxKind::NEWLINE);
        }
    }

    // ------------------------------------------------------------------
    // Expressions (precedence climbing)
    // ------------------------------------------------------------------

    fn expr(&mut self, min_bp: u8) {
        // L-81: recursion cap — nested parens / operands re-enter here; bail with a recoverable
        // error instead of overflowing the stack.
        if !self.enter() {
            self.err_and_bump("expression nesting too deep");
            return;
        }
        let cp = self.checkpoint();
        self.prefix();
        while let Some((lbp, _rbp)) = infix_bp(self.nth(0)) {
            if lbp < min_bp {
                break;
            }
            self.bump(); // the operator
            self.expr(lbp + 1);
            self.wrap(cp, SyntaxKind::BIN_EXPR);
        }
        self.leave();
    }

    fn prefix(&mut self) {
        // L-81: `!!!…` / `---…` chains recurse here directly (not via `expr`), so guard it too.
        if !self.enter() {
            self.err_and_bump("expression nesting too deep");
            return;
        }
        match self.nth(0) {
            SyntaxKind::BANG | SyntaxKind::MINUS => {
                let cp = self.checkpoint();
                self.bump();
                self.prefix();
                self.wrap(cp, SyntaxKind::UNARY_EXPR);
            }
            _ => self.postfix(),
        }
        self.leave();
    }

    fn postfix(&mut self) {
        let cp = self.checkpoint();
        self.primary();
        while self.at(SyntaxKind::DOT)
            || (self.at(SyntaxKind::L_BRACK)
                && matches!(self.nth(1), SyntaxKind::NUMBER | SyntaxKind::STRING)
                && self.nth(2) == SyntaxKind::R_BRACK)
        {
            if self.eat(SyntaxKind::DOT) {
                // a field name (IDENT) or compact dotted index (NUMBER)
                if self.at(SyntaxKind::IDENT) || self.at(SyntaxKind::NUMBER) {
                    self.bump();
                } else {
                    self.error("expected a field name or index after `.`");
                }
            } else {
                self.bump(); // [
                if self.at(SyntaxKind::STRING) {
                    self.bump(); // JSON-string object key
                } else {
                    self.expect(
                        SyntaxKind::NUMBER,
                        "an integer index or quoted key after `[` ",
                    );
                }
                self.expect(SyntaxKind::R_BRACK, "`]` to close the field or index");
            }
            self.eat(SyntaxKind::QUESTION); // optional-access marker
            self.wrap(cp, SyntaxKind::FIELD_EXPR);
        }
    }

    fn primary(&mut self) {
        // L-81: `primary` dispatches into `obj_expr`/`list_expr`/parenthesised `expr`, each of which
        // re-enters the expression grammar; cap here so a deep `((({[…` chain fails recoverably.
        if !self.enter() {
            self.err_and_bump("expression nesting too deep");
            return;
        }
        self.primary_inner();
        self.leave();
    }

    fn primary_inner(&mut self) {
        match self.nth(0) {
            SyntaxKind::VAR => {
                self.start(SyntaxKind::VAR_EXPR);
                self.bump();
                self.finish_node();
            }
            SyntaxKind::NUMBER | SyntaxKind::STRING => {
                self.start(SyntaxKind::LIT_EXPR);
                self.bump();
                self.finish_node();
            }
            SyntaxKind::L_BRACE => self.obj_expr(),
            SyntaxKind::L_BRACK => self.list_expr(),
            SyntaxKind::L_PAREN => {
                self.start(SyntaxKind::PAREN_EXPR);
                self.bump();
                self.expr(0);
                self.expect(SyntaxKind::R_PAREN, "`)`");
                self.finish_node();
            }
            SyntaxKind::ANNOTATION if self.cur_text() == "@json" => {
                self.start(SyntaxKind::JSON_EXPR);
                self.bump(); // @json
                self.json_value();
                self.finish_node();
            }
            SyntaxKind::IDENT => self.ident_expr(),
            _ => {
                self.err_and_bump("expected an expression");
            }
        }
    }

    fn ident_expr(&mut self) {
        let text = self.cur_text();
        match text {
            "true" | "false" | "null" => {
                self.start(SyntaxKind::LIT_EXPR);
                self.bump();
                self.finish_node();
            }
            "peek" => {
                self.start(SyntaxKind::PEEK_EXPR);
                self.bump();
                self.expect_symbol("a symbol after `peek`");
                self.finish_node();
            }
            "thing" => {
                self.start(SyntaxKind::THING_EXPR);
                self.eat_to_end_of_thing();
                self.finish_node();
            }
            _ => {
                // `op(args)` call, `fmt(...)`, `parse(...)`, or a bare name. Op names may be
                // dotted (`slack.message.send`, `ai.extract`) — the whole `IDENT (. IDENT)*` run
                // is one NAME.
                let kind = match text {
                    "fmt" => SyntaxKind::FMT_EXPR,
                    "parse" => SyntaxKind::PARSE_EXPR,
                    _ => SyntaxKind::CALL_EXPR,
                };
                if !self.call_follows() {
                    self.start(SyntaxKind::VAR_EXPR);
                    self.bump();
                    self.finish_node();
                    return;
                }
                let cp = self.checkpoint();
                self.start(SyntaxKind::NAME);
                self.bump(); // first ident
                while matches!(self.nth(0), SyntaxKind::DOT | SyntaxKind::MINUS)
                    && self.nth(1) == SyntaxKind::IDENT
                {
                    self.bump(); // . / -
                    self.bump(); // ident
                }
                self.finish_node();
                if self.at(SyntaxKind::L_PAREN) {
                    self.call_args();
                    self.wrap(cp, kind);
                }
            }
        }
    }

    /// `thing <kind> [<custom-kind-str>] <selector-kw> "<value>"`.
    ///
    /// Consume the four semantic fields explicitly instead of greedily taking every following
    /// identifier. In particular, `verify thing … contains …` must leave `contains` to the verify
    /// statement grammar.
    fn eat_to_end_of_thing(&mut self) {
        self.bump(); // thing
        if !self.at(SyntaxKind::IDENT) {
            self.error("expected a thing kind");
            return;
        }
        let custom = self.cur_text() == "custom";
        self.bump(); // kind
        if custom {
            self.expect(SyntaxKind::STRING, "a quoted custom thing kind");
        }
        if self.at(SyntaxKind::IDENT) {
            self.bump();
        } else {
            self.error("expected a thing selector kind");
        }
        self.expect(SyntaxKind::STRING, "a quoted thing selector");
    }

    fn call_args(&mut self) {
        self.start(SyntaxKind::ARG_LIST);
        self.bump(); // (
        self.delimited_trivia();
        while !self.at(SyntaxKind::R_PAREN) && !self.at_eof() {
            // named arg `name:` (e.g. `parse(v, as: "f64")`)
            if self.nth(0) == SyntaxKind::IDENT && self.nth(1) == SyntaxKind::COLON {
                self.start(SyntaxKind::NAMED_ARG);
                self.start(SyntaxKind::NAME);
                self.bump();
                self.finish_node();
                self.bump(); // :
                self.expr(0);
                self.finish_node();
            } else {
                self.expr(0);
            }
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
            self.delimited_trivia();
        }
        self.expect(SyntaxKind::R_PAREN, "`)` to close the call");
        self.finish_node();
    }

    fn obj_expr(&mut self) {
        // L-81: nested `{k:{k:{…}}}` recurses through each field's value `expr`; cap before `start`
        // so the tree stays balanced on the recoverable-error path.
        if !self.enter() {
            self.err_and_bump("object nesting too deep");
            return;
        }
        self.start(SyntaxKind::OBJ_EXPR);
        self.bump(); // {
        self.delimited_trivia();
        while !self.at(SyntaxKind::R_BRACE) && !self.at_eof() {
            self.start(SyntaxKind::OBJ_FIELD);
            if self.at(SyntaxKind::IDENT) || self.at(SyntaxKind::STRING) {
                self.start(SyntaxKind::NAME);
                self.bump();
                self.finish_node();
            } else {
                self.error("expected an object key");
            }
            if self.eat(SyntaxKind::COLON) {
                self.expr(0);
            }
            self.finish_node();
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
            self.delimited_trivia();
        }
        self.expect(SyntaxKind::R_BRACE, "`}` to close the object");
        self.finish_node();
        self.leave();
    }

    fn list_expr(&mut self) {
        // L-81: nested `[[[…]]]` recurses through each element `expr`; cap before `start`.
        if !self.enter() {
            self.err_and_bump("list nesting too deep");
            return;
        }
        self.start(SyntaxKind::LIST_EXPR);
        self.bump(); // [
        self.delimited_trivia();
        while !self.at(SyntaxKind::R_BRACK) && !self.at_eof() {
            self.expr(0);
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
            self.delimited_trivia();
        }
        self.expect(SyntaxKind::R_BRACK, "`]` to close the list");
        self.finish_node();
        self.leave();
    }

    fn at_symbol(&self) -> bool {
        self.at(SyntaxKind::VAR)
            || (self.at(SyntaxKind::IDENT) && crate::ast::is_bare_symbol_name(self.cur_text()))
    }

    fn expect_symbol(&mut self, what: &str) {
        if self.at_symbol() {
            self.bump();
        } else {
            self.error(format!("expected {what}"));
        }
    }

    /// Whether the current identifier begins an op call. Dotted/hyphenated operation names are
    /// consumed only when the complete name is followed by `(`; otherwise postfix parsing owns
    /// the dot as ordinary value access.
    fn call_follows(&self) -> bool {
        let mut n = 1;
        while matches!(self.nth(n), SyntaxKind::DOT | SyntaxKind::MINUS)
            && self.nth(n + 1) == SyntaxKind::IDENT
        {
            n += 2;
        }
        self.nth(n) == SyntaxKind::L_PAREN
    }

    /// Trivia/layout inside delimiters is semantically whitespace, not an outer statement-block
    /// boundary. Zero-width INDENT/DEDENT tokens are consumed without entering the CST; textual
    /// newlines/comments/spacing remain lossless children of the delimited node.
    fn delimited_trivia(&mut self) {
        loop {
            match self.raw_kind_at(self.pos) {
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE => {
                    self.feed_one()
                }
                SyntaxKind::INDENT | SyntaxKind::DEDENT => self.pos += 1,
                _ => break,
            }
        }
    }

    /// A JSON value for the `@json` escape: a balanced object/array/string/number/keyword.
    fn json_value(&mut self) {
        match self.nth(0) {
            SyntaxKind::L_BRACE => self.json_balanced(SyntaxKind::L_BRACE, SyntaxKind::R_BRACE),
            SyntaxKind::L_BRACK => self.json_balanced(SyntaxKind::L_BRACK, SyntaxKind::R_BRACK),
            SyntaxKind::STRING | SyntaxKind::NUMBER | SyntaxKind::IDENT | SyntaxKind::MINUS => {
                self.bump();
            }
            _ => self.error("expected a JSON value after `@json`"),
        }
    }

    fn json_balanced(&mut self, open: SyntaxKind, close: SyntaxKind) {
        self.bump(); // open
        let mut depth = 1;
        while depth > 0 && !self.at_eof() && !self.at_newline() {
            let k = self.nth(0);
            if k == open {
                depth += 1;
            } else if k == close {
                depth -= 1;
            }
            self.bump();
        }
    }
}

fn arm_kind(kw: &str) -> SyntaxKind {
    match kw {
        "case" => SyntaxKind::CASE_ARM,
        "default" => SyntaxKind::DEFAULT_ARM,
        "branch" => SyntaxKind::BRANCH_ARM,
        "step" => SyntaxKind::STEP_ARM,
        "undo" => SyntaxKind::UNDO_CLAUSE,
        _ => SyntaxKind::ERROR,
    }
}

/// Left/right binding power for an infix operator, or `None` if `k` is not an infix operator.
fn infix_bp(k: SyntaxKind) -> Option<(u8, u8)> {
    use SyntaxKind::*;
    Some(match k {
        PIPE_PIPE => (1, 2),
        AMP_AMP => (3, 4),
        EQ_EQ | NEQ | LT | LT_EQ | GT | GT_EQ => (5, 6),
        PLUS | MINUS => (7, 8),
        STAR | SLASH => (9, 10),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_text(p: &Parse) -> String {
        p.syntax().text().to_string()
    }

    #[test]
    fn tree_is_lossless() {
        let srcs = [
            "flow greet($name: String) -> String\n  $g = fmt(\"hi {name}\")\n  when $g != \"\"\n    do write \"out.txt\", $g\n  return $g\n",
            "flow f\n  # comment\n  $x = 1\n\n  $y = $x.a.b\n",
            "flow f\n  each $it in $items -> flat $all\n    do process $it\n",
            "flow f\n  match $s\n    case \"a\"\n      do a\n    default\n      do b\n",
            "journey handle\n  agent \"guide\"\n  flow\n    $claims = ai.extract($input)\n    return $claims\n",
        ];
        for src in srcs {
            let p = parse_cst(src);
            assert_eq!(tree_text(&p), src, "tree not lossless for:\n{src}");
        }
    }

    #[test]
    fn clean_flow_has_no_errors() {
        let src = "flow f(x: Number) -> Number\n  $y = $x\n  return $y\n";
        let p = parse_cst(src);
        assert!(p.errors.is_empty(), "unexpected errors: {:?}", p.errors);
        assert_eq!(p.syntax().kind(), SyntaxKind::ROOT);
    }

    #[test]
    fn recovers_and_reports_multiple_errors() {
        // Two independent broken lines: a bind with no RHS, and an unclosed call. The parser must
        // still produce a complete tree covering the whole source and report >= 1 error.
        let src = "flow f\n  $a =\n  do read(\n  $b = 2\n";
        let p = parse_cst(src);
        assert_eq!(tree_text(&p), src, "tree must stay lossless under errors");
        assert!(!p.errors.is_empty(), "expected recovery errors");
        // The good final bind still parsed: the tree contains a BIND_STMT for $b.
        let has_bind = p
            .syntax()
            .descendants()
            .any(|n| n.kind() == SyntaxKind::BIND_STMT);
        assert!(
            has_bind,
            "later good statements should still parse after recovery"
        );
    }

    /// L-81 (failing-first): deeply nested expression, list, object, and type input must parse to a
    /// bounded `ParseError` instead of overflowing the stack and aborting the process. The depth here
    /// (many thousands of levels) is well past the ~60–80k-token empirical crash threshold; before the
    /// depth guard each of these `SIGABRT`ed. Reaching the assertions at all proves no abort occurred.
    #[test]
    fn deeply_nested_input_is_bounded_not_aborting() {
        let depth = 20_000;
        // Nested parentheses in an expression position.
        let parens = format!(
            "flow f\n  $x = {}1{}\n",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let p = parse_cst(&parens);
        assert!(
            !p.errors.is_empty(),
            "deep parens must yield a bounded parse error, not a crash"
        );

        // Nested list and object literals.
        let lists = format!(
            "flow f\n  $x = {}1{}\n",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let list_parse = parse_cst(&lists);
        assert!(!list_parse.errors.is_empty(), "deep lists bounded");

        // Nested `List<List<…>>` in a parameter type.
        let ty = format!(
            "flow f(x: {}Number{})\n  return null\n",
            "List<".repeat(depth),
            ">".repeat(depth)
        );
        assert!(
            !parse_cst(&ty).errors.is_empty(),
            "deep type nesting bounded"
        );

        // L-114: nested *statement blocks* — the axis the legs above omit. `block` → `statement` →
        // `when_stmt` → `block` is its own unbounded recursion, and it aborted at ~900 levels on an
        // 8 MiB stack (~200 on a 2 MiB tokio worker) while the expression guard sat right next to it.
        let nested = nested_statements(STATEMENT_NESTING_DEPTH);
        let deep = parse_cst(&nested);
        assert!(
            !deep.errors.is_empty(),
            "deep statement nesting must yield a bounded parse error, not a crash"
        );
        assert_eq!(
            tree_text(&deep),
            nested,
            "the tree stays lossless when the depth guard refuses a block"
        );

        // A well-formed shallow program is unaffected by the guard.
        let ok = parse_cst("flow f\n  $x = (((1 + 2)))\n  return $x\n");
        assert!(
            ok.errors.is_empty(),
            "shallow nesting still parses: {:?}",
            ok.errors
        );
        let ok_nested = parse_cst(&nested_statements(8));
        assert!(
            ok_nested.errors.is_empty(),
            "hand-writable statement nesting still parses: {:?}",
            ok_nested.errors
        );
    }

    /// L-114: the depth the nested-*statement* legs use. Statement blocks are indentation-delimited,
    /// so a `depth`-level fixture costs O(depth²) source bytes — 20,000 levels would be a 400 MiB
    /// string. 2,000 levels is a ~4 MiB fixture and already twice the ~900 levels that `SIGABRT`ed
    /// `fluxlang compile` on the default 8 MiB stack.
    const STATEMENT_NESTING_DEPTH: usize = 2_000;

    /// `depth` nested `when` blocks around a `return`, each one indented one step further.
    fn nested_statements(depth: usize) -> String {
        let mut src = String::from("flow f\n");
        for level in 0..depth {
            src.push_str(&"  ".repeat(level + 1));
            src.push_str("when $x\n");
        }
        src.push_str(&"  ".repeat(depth + 1));
        src.push_str("return 1\n");
        src
    }

    /// L-114: everything downstream of the tolerant parse must survive the same input — the strict
    /// AST entries (`parse`/`parse_program`, which recurse per block through `cst_decode`) and the
    /// editor paths that consume the tolerant tree directly (`highlight`, `format_source`).
    #[test]
    fn deeply_nested_statements_are_bounded_through_every_consumer() {
        let nested = nested_statements(STATEMENT_NESTING_DEPTH);

        // The strict entries refuse it as a bounded error rather than recursing into the tree.
        assert!(
            crate::parse::parse(&nested).is_err(),
            "the strict flow entry must return a bounded error"
        );
        assert!(
            crate::parse::parse_program(&nested).is_err(),
            "the strict module entry must return a bounded error"
        );

        // `cst_decode` recurses per block over whatever the parser built, so the guard is only worth
        // anything if the tree handed over is shallow. Assert that directly rather than inferring it
        // from the error above.
        let parsed = parse_cst(&nested);
        let depth = tree_depth(&parsed.syntax());
        assert!(
            depth <= MAX_PARSE_DEPTH * 8,
            "the guard must bound the tree the lowerer recurses over, but it is {depth} deep"
        );

        // The tolerant editor path never sees the strict error, so it has to survive on its own.
        assert!(
            !crate::highlight::highlight(&nested).is_empty(),
            "the editor still gets highlight spans for an over-deep buffer"
        );
        assert_eq!(
            crate::format_cst::format_source(&nested),
            None,
            "a buffer the depth guard refused is not formattable"
        );
    }

    /// The longest root-to-leaf node path in a tree, computed iteratively so the measurement itself
    /// cannot overflow the stack it is measuring.
    fn tree_depth(root: &crate::syntax::SyntaxNode) -> usize {
        let mut deepest = 0;
        for node in root.descendants() {
            deepest = deepest.max(node.ancestors().count());
        }
        deepest
    }

    // ---- L-90: incremental reparse (green-node reuse) ----------------------

    /// Apply one contiguous edit and reparse it incrementally, asserting the result is identical to
    /// a full parse of the resulting text — same tree, same errors.
    fn assert_incremental(src: &str, replaced: std::ops::Range<usize>, inserted: &str) -> bool {
        let mut new_text = String::from(src);
        new_text.replace_range(replaced.clone(), inserted);
        let old = parse_cst(src);
        let range = TextRange::new(
            TextSize::new(replaced.start as u32),
            TextSize::new(replaced.end as u32),
        );
        let Some(incremental) = reparse(&old, src, &new_text, range, inserted.len()) else {
            return false;
        };
        let full = parse_cst(&new_text);
        assert_eq!(
            incremental.green, full.green,
            "incremental tree differs from a full reparse of {new_text:?}"
        );
        assert_eq!(
            incremental.errors, full.errors,
            "incremental errors differ from a full reparse of {new_text:?}"
        );
        true
    }

    #[test]
    fn incremental_reparse_matches_full_reparse() {
        let src = "flow a\n  $x = 1\n  return $x\n\nop b() -> String\n  return \"x\"\n\nflow c\n  $y = 2\n  return $y\n";
        // Edit inside the first flow's body: replace the literal `1` with a call.
        let one = src.find("= 1").unwrap() + 2;
        assert!(
            assert_incremental(src, one..one + 1, "read(\"a.txt\")"),
            "an edit inside a declaration body takes the incremental path"
        );
        // Edit inside the middle op: insert a comment line.
        let at = src.find("  return \"x\"").unwrap();
        assert!(
            assert_incremental(src, at..at, "  # note\n"),
            "comment insert reuses the neighbours"
        );
        // Edit inside the last flow: rename a bind (leaves a temporarily unbound use).
        let y = src.rfind("$y = 2").unwrap();
        assert!(
            assert_incremental(src, y..y + 2, "$z"),
            "a rename edit reuses the neighbours"
        );
    }

    #[test]
    fn incremental_reparse_declines_a_structural_edit() {
        let src = "flow a\n  return 1\n\nflow b\n  return 2\n";
        // Typing a new declaration at the very start is not contained in any declaration.
        assert!(
            !assert_incremental(src, 0..0, "flow z\n  return 0\n\n"),
            "a new top-level declaration must fall back to a full reparse"
        );
        // Dedenting a body line out of its declaration changes the top-level structure.
        let body = src.find("  return 1").unwrap();
        assert!(
            !assert_incremental(src, body..body + 2, "flow mid\n  return 9\n"),
            "an edit that splits a declaration must fall back to a full reparse"
        );
    }

    #[test]
    fn incremental_reparse_reuses_untouched_declaration_nodes() {
        // The point of the fast path: the neighbours' green subtrees are shared, not rebuilt.
        let src = "flow a\n  return 1\n\nflow b\n  $x = 1\n  return $x\n";
        let old = parse_cst(src);
        let at = src.rfind('1').unwrap();
        let mut new_text = String::from(src);
        new_text.replace_range(at..at + 1, "42");
        let range = TextRange::new(TextSize::new(at as u32), TextSize::new(at as u32 + 1));
        let incremental = reparse(&old, src, &new_text, range, 2).expect("localized edit");
        let before = old.green.children().next().unwrap();
        let after = incremental.green.children().next().unwrap();
        let (rowan::NodeOrToken::Node(before), rowan::NodeOrToken::Node(after)) = (before, after)
        else {
            panic!("root children are declaration nodes")
        };
        assert!(
            std::ptr::eq(before as *const _, after as *const _),
            "the untouched `flow a` green node is reused, not rebuilt"
        );
    }
}

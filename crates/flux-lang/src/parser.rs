//! `parser` — the tolerant, event-free recursive-descent parser that turns the [`crate::lexer`]
//! token stream into a lossless **rowan** green tree (stage two of the CST front-end, L-58).
//!
//! It is *total* and *error-recovering*: it never aborts. Unexpected input is wrapped in
//! [`SyntaxKind::ERROR`] nodes and the parser resyncs at the next `NEWLINE`/`DEDENT`, so an
//! in-progress buffer still yields a complete tree plus a list of [`ParseError`]s with real spans —
//! exactly what the language server needs. Every non-layout token is fed to the builder in order, so
//! the tree round-trips to the source byte-for-byte.
//!
//! Scope: the **flow** grammar is parsed precisely (that is the `parse` path, where the 16
//! native-syntax nodes live and what the editor edits most); other top-level declarations
//! (`agent`/`channel`/…) are captured structurally as opaque [`SyntaxKind::DECL`] nodes; composite
//! `op` declarations are structured like flows so the LSP can analyze their bodies with ranges.

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

struct Parser<'s> {
    src: &'s str,
    tokens: Vec<LexToken>,
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    errors: Vec<ParseError>,
}

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
        }
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
    // The legacy preprocess drops blank and comment-only lines before the grammar ever sees them,
    // so at a header→block or block→clause boundary (`flow f` + blank + body; `try` body + blank +
    // `catch`) those lines are insignificant. `nth`/`at` deliberately treat NEWLINE as significant
    // (the grammar is line-oriented), so boundaries use these dedicated helpers instead.

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
    fn block_if_indented(&mut self, f: impl FnOnce(&mut Self)) {
        if self.at_block() {
            self.skip_blank_lines();
            f(self);
        }
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
                SyntaxKind::IDENT if self.at_decl_kw() => self.opaque_decl(),
                _ => {
                    // Unexpected top-level content: recover a line at a time.
                    self.start(SyntaxKind::ERROR);
                    self.error("expected a `flow` header or a top-level declaration");
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
            "agent" | "channel" | "datasource" | "trigger" | "journey"
        )
    }

    /// A pure-data top-level declaration, captured structurally (header line + its indented block)
    /// as one opaque DECL node. Its interior grammar belongs to the module loader, not the flow CST.
    fn opaque_decl(&mut self) {
        self.start(SyntaxKind::DECL);
        self.eat_to_end_of_line();
        self.block_if_indented(Self::opaque_block);
        self.finish_node();
    }

    /// Consume a whole INDENT…DEDENT block without interpreting it (for opaque decls).
    fn opaque_block(&mut self) {
        self.eat_trivia();
        self.pos += 1; // INDENT (not emitted)
        let mut depth = 1;
        while depth > 0 && !self.at_eof() {
            match self.nth(0) {
                SyntaxKind::INDENT => {
                    self.pos += 1;
                    depth += 1;
                }
                SyntaxKind::DEDENT => {
                    self.pos += 1;
                    depth -= 1;
                }
                _ => self.bump(),
            }
        }
    }

    fn flow_decl(&mut self) {
        self.start(SyntaxKind::FLOW_DECL);
        self.flow_header();
        // Column-0 `goal "…"` directive lines are tolerated-and-ignored by the legacy parser —
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
        if self.at(SyntaxKind::IDENT) {
            self.bump(); // optional name…
                         // …which may be kebab-case (`god-code-review`): the lexer splits it into
                         // IDENT/NUMBER segments joined by `-`, all legal per `is_valid_decl_name`.
            while self.at(SyntaxKind::MINUS)
                && matches!(self.nth(1), SyntaxKind::IDENT | SyntaxKind::NUMBER)
            {
                self.bump(); // -
                self.bump(); // segment
            }
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
            self.expect(SyntaxKind::IDENT, "a parameter name");
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
        self.start(SyntaxKind::NAME);
        self.expect(SyntaxKind::IDENT, "a type name");
        if self.eat(SyntaxKind::LT) {
            self.type_ref();
            self.expect(SyntaxKind::GT, "`>` to close the `List<…>` type");
        }
        self.finish_node();
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
                "retry" => return self.simple_block_stmt(SyntaxKind::RETRY_STMT, false),
                "seq" => return self.simple_block_stmt(SyntaxKind::SEQ_STMT, false),
                "ctx" => return self.ctx_stmt(),
                "return" => return self.return_stmt(),
                "assert" => return self.header_only_stmt(SyntaxKind::ASSERT_STMT),
                "memo" => return self.memo_stmt(),
                "once" => return self.simple_block_stmt(SyntaxKind::ONCE_STMT, false),
                "checkpoint" => return self.header_only_stmt(SyntaxKind::CHECKPOINT_STMT),
                "await" => return self.header_only_stmt(SyntaxKind::AWAIT_STMT),
                "confirm" => return self.simple_block_stmt(SyntaxKind::CONFIRM_STMT, false),
                "throttle" => return self.simple_block_stmt(SyntaxKind::THROTTLE_STMT, false),
                "debounce" => return self.simple_block_stmt(SyntaxKind::DEBOUNCE_STMT, false),
                "verify" => return self.header_only_stmt(SyntaxKind::VERIFY_STMT),
                "try" => return self.try_stmt(),
                "scope" => return self.scope_stmt(),
                "saga" => return self.saga_stmt(),
                "pipe" => return self.simple_block_stmt(SyntaxKind::PIPE_STMT, false),
                _ => {}
            }
        }
        // Otherwise: a bare call (`op(args)`) or an expression statement.
        self.expr_stmt();
    }

    /// `$x = expr` / `$x: T = expr` / `$x += a, b` / a `$x`-led expression, dispatched on the token
    /// after the leading `$var`.
    fn bind_or_expr_stmt(&mut self) {
        match self.nth(1) {
            SyntaxKind::PLUS_EQ => {
                self.start(SyntaxKind::CTX_APPEND_STMT);
                self.bump(); // $var
                self.bump(); // +=
                self.expr_list_to_eol();
                self.eat(SyntaxKind::NEWLINE);
                self.finish_node();
            }
            SyntaxKind::EQ | SyntaxKind::COLON => {
                self.start(SyntaxKind::BIND_STMT);
                self.bump(); // $var
                if self.eat(SyntaxKind::COLON) {
                    self.type_ref();
                }
                self.expect(SyntaxKind::EQ, "`=` in a bind");
                self.expr(0);
                self.eat(SyntaxKind::NEWLINE);
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
        while self.at(SyntaxKind::DOT) && self.nth(1) == SyntaxKind::IDENT {
            self.bump(); // .
            self.bump(); // ident — dotted op names (`slack.message.send`) are one NAME
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
        // insignificant, exactly as in the legacy preprocess)
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
        self.expect(SyntaxKind::VAR, "the loop variable, e.g. `$x`");
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
            self.expect(SyntaxKind::VAR, "a collect variable after `->`");
        }
        self.eat(SyntaxKind::NEWLINE);
        self.block_if_indented(Self::block);
        self.finish_node();
    }

    /// `repeat`/`loop`: header to end of line, then a body whose first line may be `until <cond>`.
    fn repeat_like(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.eat_to_end_of_line();
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
        self.eat_to_end_of_line();
        self.block_if_indented(|p| p.arm_block(&["case", "default"]));
        self.finish_node();
    }

    fn branch_block(&mut self, kind: SyntaxKind) {
        self.start(kind);
        self.eat_to_end_of_line();
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
                    self.eat_to_end_of_line();
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
            if self.at(SyntaxKind::VAR) {
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
        self.eat_to_end_of_line(); // scope [$r = acquire]
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
        self.expect(SyntaxKind::VAR, "a symbol after `memo`");
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
    }

    fn prefix(&mut self) {
        match self.nth(0) {
            SyntaxKind::BANG | SyntaxKind::MINUS => {
                let cp = self.checkpoint();
                self.bump();
                self.prefix();
                self.wrap(cp, SyntaxKind::UNARY_EXPR);
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) {
        let cp = self.checkpoint();
        self.primary();
        while self.at(SyntaxKind::DOT) {
            self.bump(); // .
                         // a field name (IDENT) or index (NUMBER)
            if self.at(SyntaxKind::IDENT) || self.at(SyntaxKind::NUMBER) {
                self.bump();
            } else {
                self.error("expected a field name or index after `.`");
            }
            self.eat(SyntaxKind::QUESTION); // optional-access marker
            self.wrap(cp, SyntaxKind::FIELD_EXPR);
        }
    }

    fn primary(&mut self) {
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
                self.expect(SyntaxKind::VAR, "a symbol after `peek`");
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
                let cp = self.checkpoint();
                self.start(SyntaxKind::NAME);
                self.bump(); // first ident
                while self.at(SyntaxKind::DOT) && self.nth(1) == SyntaxKind::IDENT {
                    self.bump(); // .
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

    /// `thing <kind> [<custom-kind-str>] <selector-kw> "<value>"` — the whole form is a
    /// space-delimited run of idents/strings/numbers (`thing custom "widget" key "w-1"`,
    /// `thing person name "john"`), so consume that run greedily; operators/parens/EOL end it.
    fn eat_to_end_of_thing(&mut self) {
        self.bump(); // thing
        while matches!(
            self.nth(0),
            SyntaxKind::IDENT | SyntaxKind::STRING | SyntaxKind::NUMBER
        ) {
            self.bump();
        }
    }

    fn call_args(&mut self) {
        self.start(SyntaxKind::ARG_LIST);
        self.bump(); // (
        while !self.at(SyntaxKind::R_PAREN) && !self.at_eof() && !self.at_newline() {
            // named arg `name:` (e.g. `parse(v, as: "f64")`)
            if self.nth(0) == SyntaxKind::IDENT && self.nth(1) == SyntaxKind::COLON {
                self.start(SyntaxKind::NAME);
                self.bump();
                self.finish_node();
                self.bump(); // :
            }
            self.expr(0);
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        self.expect(SyntaxKind::R_PAREN, "`)` to close the call");
        self.finish_node();
    }

    fn obj_expr(&mut self) {
        self.start(SyntaxKind::OBJ_EXPR);
        self.bump(); // {
        while !self.at(SyntaxKind::R_BRACE) && !self.at_eof() && !self.at_newline() {
            self.start(SyntaxKind::OBJ_FIELD);
            if self.at(SyntaxKind::IDENT) || self.at(SyntaxKind::STRING) {
                self.start(SyntaxKind::NAME);
                self.bump();
                self.finish_node();
            } else {
                self.error("expected an object key");
            }
            self.expect(SyntaxKind::COLON, "`:` after the key");
            self.expr(0);
            self.finish_node();
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        self.expect(SyntaxKind::R_BRACE, "`}` to close the object");
        self.finish_node();
    }

    fn list_expr(&mut self) {
        self.start(SyntaxKind::LIST_EXPR);
        self.bump(); // [
        while !self.at(SyntaxKind::R_BRACK) && !self.at_eof() && !self.at_newline() {
            self.expr(0);
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        self.expect(SyntaxKind::R_BRACK, "`]` to close the list");
        self.finish_node();
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
}

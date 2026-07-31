//! **Flux Glyph** — the compact, indented *opcode* projection of a [`DraftAst`] (L-97).
//!
//! One AST, several readable projections (`docs/designs/flux-notation-workbench.md`): canonical
//! `.flux` is the authored surface, [`crate::render`]'s tree is the execution-path display,
//! Railflux ([`crate::render::render_rail`]) is the dataflow view — and Glyph is the *agent-facing*
//! view: one line per statement, one short opcode per construct, indentation for structure.
//!
//! Glyph is **bidirectional** but it is not a second language. Nothing here changes the AST, the
//! analyzer, authorization, or the runtime: this module is a pure `&DraftAst -> String` /
//! `&str -> Result<DraftAst>` pair, and it is reached only by naming Glyph explicitly. Canonical
//! `.flux` loading never sniffs it — [`crate::parse::parse`] rejects a Glyph document, and
//! [`parse_glyph`] rejects canonical source.
//!
//! # The notation
//!
//! A Glyph document is a sequence of lines. Blank lines and `#` comment lines are ignored; every
//! other line is `<indent><opcode>[ <operand>]`, indented **exactly two spaces per level**, and a
//! body indents exactly one level below its construct. The opcode vocabulary is [`OPCODES`] and
//! nothing else:
//!
//! ```text
//! F triage(ticket:Ticket)>Answer
//! = kind classify(ticket)
//! &
//!   | docs
//!     search(query: ticket)
//!   | hits
//!     grep(pattern: ticket.title)
//! ?= kind
//!   | "bug"
//!     !? "Open issue?" medium
//!       = issue create_issue(hits, ticket)
//!       ^ issue
//!   |*
//!     ^ docs
//! ```
//!
//! `|` is the one **arm** opcode — a `match`/`route` case, a named `parallel`/`race` branch, an
//! anonymous `fallback` branch — and `|*` is the one **default** arm (`match`/`route` default, or a
//! conditional's `else`). Which of those an arm means is decided by its enclosing opcode and never
//! guessed: an arm in the wrong place, an unlabelled case, a duplicate branch name or a default arm
//! that is not last is a located error, not a repair.
//!
//! # Core plus escape
//!
//! Glyph spells the fourteen constructs in [`OPCODES`] natively. Two more shapes carry everything
//! else, in the epic's "core plus escape" discipline:
//!
//! - A **pass-through leaf** — any statement whose canonical Flux spelling is a single line that is
//!   not itself an escape (`read("f")`, `pack += a`, `checkpoint "x"`, `p.q`). It is written in
//!   canonical Flux, verbatim, and is always a leaf: Glyph owns block structure through its
//!   opcodes, so a pass-through line may not carry a body.
//! - The **escape** `@{…}` — the node's wire JSON, compact, on one line. Anything with no native
//!   spelling takes it: a multi-line canonical construct (`each`, `try`, `saga`, …), a node whose
//!   names or labels are unspellable, a `parallel`/`race` whose branch names collide, and any
//!   statement whose canonical one-liner would be *misread* as an opcode.
//!
//! That last guard is why the writer and the reader share one classifier ([`classify`]): a line is
//! emitted verbatim only when the reader would classify it as a pass-through, so
//! `parse_glyph(&format_glyph(&ast)) == ast` holds for **every** [`DraftAst`] body.
//!
//! # Diagnostics
//!
//! Structure is Glyph's own, so structural errors are reported here with the offending Glyph line.
//! Expressions are canonical Flux, so the expression grammar — and its diagnostics — are the
//! canonical parser's; [`parse_glyph`] expands the document to canonical Flux, keeps a line map, and
//! rewrites the canonical `line N:` prefix back to the Glyph line the reader can actually see.
//!
//! # The flow-header exception
//!
//! Like [`crate::format`], the `F` header has no escape: an unspellable flow name, parameter name or
//! header type is emitted verbatim and produces a **loud** error on the way back rather than silent
//! corruption. The analyzer rejects such names before they can reach a formatted artifact.

use std::collections::BTreeSet;

use crate::ast::{DraftAst, Node, Param, TypeRef};
use crate::error::{FlowError, Result};
use crate::format;

/// One Glyph indentation level.
const INDENT: &str = "  ";

/// The raw-AST escape's opening sigil; the rest of the line is the node's compact wire JSON.
const ESCAPE: char = '@';

/// Every Glyph opcode, paired with the canonical Flux construct it projects. This table is the
/// single source of truth for the vocabulary: the reader classifies against it, the writer emits
/// from it, and `docs/glyph.md` documents exactly these rows.
pub const OPCODES: &[(&str, &str)] = &[
    ("F", "flow header"),
    ("=", "bind"),
    ("~=", "memo"),
    ("^", "return"),
    ("?", "when"),
    ("?=", "match"),
    ("?~", "route"),
    (
        "|",
        "arm — a match/route case, a parallel/race branch, a fallback branch",
    ),
    (
        "|*",
        "default arm — a match/route default, or a conditional's else",
    ),
    ("&", "parallel"),
    ("||", "race"),
    ("??", "fallback"),
    ("!?", "confirm"),
    ("!!", "assert"),
];

/// The characters an opcode is built from. A first token made **only** of these that is not in
/// [`OPCODES`] is a typo'd opcode, not a statement — so it is rejected rather than handed to the
/// canonical parser as if it were Flux.
const SIGILS: &str = "=^?|&!~*+<>";

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Render `ast` as a Glyph document. Pure, total and deterministic: equal ASTs produce
/// byte-identical text, every node kind is covered (natively or through the `@{…}` escape), and the
/// result re-reads through [`parse_glyph`] to the same AST. Ends with a trailing newline.
pub fn format_glyph(ast: &DraftAst) -> String {
    let mut out = String::new();
    out.push_str(&flow_header(ast));
    out.push('\n');
    write_body(&ast.body, 0, &mut out);
    out
}

/// The `F` header line: `F [name][(param:Type, …)][>Return]`. Emitted even when every part is
/// absent (a bare `F`), so a document always declares which notation it is.
fn flow_header(ast: &DraftAst) -> String {
    let mut operand = String::new();
    if let Some(name) = &ast.name {
        operand.push_str(name);
    }
    if !ast.params.is_empty() {
        operand.push('(');
        let ps: Vec<String> = ast
            .params
            .iter()
            .map(|p| format!("{}:{}", p.name.0, p.ty.label()))
            .collect();
        operand.push_str(&ps.join(", "));
        operand.push(')');
    }
    if let Some(r) = &ast.returns {
        operand.push('>');
        operand.push_str(&r.label());
    }
    if operand.is_empty() {
        "F".to_string()
    } else {
        format!("F {operand}")
    }
}

fn write_body(body: &[Node], depth: usize, out: &mut String) {
    for node in body {
        write_stmt(node, depth, out);
    }
}

/// Write one line at `depth`.
fn write_line(text: &str, depth: usize, out: &mut String) {
    out.push_str(&INDENT.repeat(depth));
    out.push_str(text);
    out.push('\n');
}

/// The `@{…}` escape for `node` — its compact wire JSON, which is always one line.
fn escape(node: &Node) -> String {
    format!(
        "{ESCAPE}{}",
        serde_json::to_string(node).unwrap_or_else(|_| "null".to_string())
    )
}

/// The canonical inline spelling of `node`, or `None` when canonical Flux would itself escape it.
fn expr(node: &Node) -> Option<String> {
    let text = format::fmt_expr(node, false);
    (!text.starts_with("@json ")).then_some(text)
}

/// A bind/memo target: `name` or `name:Type`, one whitespace-free token.
fn decl(name: &crate::ast::SymbolName, ty: Option<&TypeRef>) -> Option<String> {
    if !name.is_identifier() || !ty.is_none_or(format::is_spellable_type) {
        return None;
    }
    Some(match ty {
        Some(t) => format!("{}:{}", format::fmt_symbol(name), t.label()),
        None => format::fmt_symbol(name),
    })
}

/// The optional `> bind` suffix a `race`/`fallback` carries.
fn bind_suffix(bind: Option<&crate::ast::SymbolName>) -> Option<String> {
    match bind {
        None => Some(String::new()),
        Some(b) if b.is_identifier() => Some(format!(" > {}", format::fmt_symbol(b))),
        Some(_) => None,
    }
}

/// Branch labels for a `parallel`/`race`, or `None` when one is unspellable or two collide — a
/// duplicate label has no unambiguous Glyph reading, so the whole node takes the escape.
fn branch_labels(branches: &[crate::ast::Branch]) -> Option<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut labels = Vec::with_capacity(branches.len());
    for branch in branches {
        if !branch.name.is_identifier() {
            return None;
        }
        let label = format::fmt_symbol(&branch.name);
        if !seen.insert(label.clone()) {
            return None;
        }
        labels.push(label);
    }
    Some(labels)
}

fn write_stmt(node: &Node, depth: usize, out: &mut String) {
    match glyph_stmt(node) {
        Some(written) => write_lines(&written, depth, out),
        None => write_line(&escape(node), depth, out),
    }
}

/// A written statement: its own line plus the nested blocks under it.
struct Written {
    line: String,
    blocks: Vec<Written>,
}

impl Written {
    fn leaf(line: String) -> Self {
        Written {
            line,
            blocks: Vec::new(),
        }
    }
}

fn write_lines(written: &Written, depth: usize, out: &mut String) {
    write_line(&written.line, depth, out);
    for block in &written.blocks {
        write_lines(block, depth + 1, out);
    }
}

/// A body rendered as nested [`Written`] blocks (each entry is one statement at the next level).
fn written_body(body: &[Node]) -> Vec<Written> {
    body.iter()
        .map(|node| match glyph_stmt(node) {
            Some(written) => written,
            None => Written::leaf(escape(node)),
        })
        .collect()
}

/// The native Glyph spelling of `node`, or `None` when it has none and must take the escape.
fn glyph_stmt(node: &Node) -> Option<Written> {
    match node {
        // `= name[:Type] <expr>` — a bind. An `@effect(…)` marker has no one-line Glyph form, so a
        // bind carrying one takes the escape (as does an unspellable name/type/value).
        Node::Bind {
            name,
            value,
            ty,
            effect: None,
        } => Some(Written::leaf(format!(
            "= {} {}",
            decl(name, ty.as_ref())?,
            expr(value)?
        ))),
        // `~= name[:Type] <expr>` — a memo.
        Node::Memo {
            name,
            value,
            ty,
            effect: None,
        } => Some(Written::leaf(format!(
            "~= {} {}",
            decl(name, ty.as_ref())?,
            expr(value)?
        ))),
        Node::Return { value } => Some(Written::leaf(format!("^ {}", expr(value)?))),
        // `? <cond>` + the then-body; a trailing `|*` arm carries the else-body.
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            let mut blocks = written_body(then);
            if !otherwise.is_empty() {
                blocks.push(Written {
                    line: "|*".to_string(),
                    blocks: written_body(otherwise),
                });
            }
            Some(Written {
                line: format!("? {}", expr(cond)?),
                blocks,
            })
        }
        // `?= <subject>` + `| <value>` cases + an optional `|*` default.
        Node::Match {
            subject,
            cases,
            default,
        } => {
            let subject = expr(subject)?;
            let mut blocks = Vec::with_capacity(cases.len() + 1);
            for case in cases {
                blocks.push(Written {
                    line: format!("| {}", expr(&case.value)?),
                    blocks: written_body(&case.body),
                });
            }
            if !default.is_empty() {
                blocks.push(Written {
                    line: "|*".to_string(),
                    blocks: written_body(default),
                });
            }
            Some(Written {
                line: format!("?= {subject}"),
                blocks,
            })
        }
        // `?~ <selector>` + `| "<label>"` cases + an optional `|*` default. A route label is a
        // string, spelled exactly as canonical `case` spells it.
        Node::Route {
            selector,
            cases,
            default,
        } => {
            let selector = expr(selector)?;
            let mut blocks = Vec::with_capacity(cases.len() + 1);
            for case in cases {
                blocks.push(Written {
                    line: format!("| {}", format::compact_str(&case.label, false)),
                    blocks: written_body(&case.body),
                });
            }
            if !default.is_empty() {
                blocks.push(Written {
                    line: "|*".to_string(),
                    blocks: written_body(default),
                });
            }
            Some(Written {
                line: format!("?~ {selector}"),
                blocks,
            })
        }
        // `&` + `| <name>` branches.
        Node::Parallel { branches } => {
            let labels = branch_labels(branches)?;
            Some(Written {
                line: "&".to_string(),
                blocks: labels
                    .into_iter()
                    .zip(branches)
                    .map(|(label, branch)| Written {
                        line: format!("| {label}"),
                        blocks: written_body(&branch.body),
                    })
                    .collect(),
            })
        }
        // `|| <timeout>[ > bind]` + `| <name>` branches.
        Node::Race {
            timeout_ms,
            branches,
            bind,
        } => {
            let labels = branch_labels(branches)?;
            Some(Written {
                line: format!(
                    "|| {}{}",
                    format::fmt_duration(*timeout_ms),
                    bind_suffix(bind.as_ref())?
                ),
                blocks: labels
                    .into_iter()
                    .zip(branches)
                    .map(|(label, branch)| Written {
                        line: format!("| {label}"),
                        blocks: written_body(&branch.body),
                    })
                    .collect(),
            })
        }
        // `??[ > bind]` + unlabelled `|` branches.
        Node::Fallback { branches, bind } => Some(Written {
            line: format!("??{}", bind_suffix(bind.as_ref())?),
            blocks: branches
                .iter()
                .map(|branch| Written {
                    line: "|".to_string(),
                    blocks: written_body(&branch.body),
                })
                .collect(),
        }),
        // `!? "<message>"[ <risk>]` + the guarded body.
        Node::Confirm {
            message,
            risk,
            body,
        } => {
            let risk = match risk {
                None => String::new(),
                Some(r) if format::is_word_token(r) => format!(" {r}"),
                Some(_) => return None,
            };
            Some(Written {
                line: format!("!? {}{risk}", format::compact_str(message, false)),
                blocks: written_body(body),
            })
        }
        // `!! <cond>[, "<message>"]` — the same tail canonical `assert` carries.
        Node::Assert { cond, message } => {
            let mut line = format!("!! {}", expr(cond)?);
            if let Some(m) = message {
                line.push_str(", ");
                line.push_str(&format::compact_str(m, false));
            }
            Some(Written::leaf(line))
        }
        // Everything else: a canonical one-liner if the reader would read it back as one.
        other => pass_through(other).map(Written::leaf),
    }
}

/// The canonical Flux one-liner for `node`, when it is a single line, is not itself a canonical
/// `@json` escape, and the reader would classify it as a pass-through statement rather than as an
/// opcode. Otherwise `None` — the node takes the `@{…}` escape.
fn pass_through(node: &Node) -> Option<String> {
    let mut canonical = String::new();
    // Level 0 with an empty indent unit: the line carries no leading whitespace of its own.
    // `multiline: false` forbids the `"""…"""` string spelling, so the result cannot contain a
    // newline of its own — one statement is always one Glyph line.
    format::fmt_stmt(node, 0, "", false, &mut canonical);
    let line = canonical.strip_suffix('\n')?;
    if line.contains('\n') || line.starts_with("@json ") {
        return None;
    }
    let (head, _) = split_head(line);
    matches!(classify(head), Ok(Op::PassThrough)).then(|| line.to_string())
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Read a Glyph document into a [`DraftAst`]. Total: malformed input returns
/// [`FlowError::Parse`] naming the offending Glyph line, never a panic and never a guess.
///
/// Glyph must be selected explicitly — this is the only entry point that accepts it, and it accepts
/// nothing else: canonical `.flux` source is *not* a Glyph document (its `flow` header line would
/// have to be a leaf) and is rejected here.
pub fn parse_glyph(src: &str) -> Result<DraftAst> {
    let lines = lex(src)?;
    let mut cursor = 0usize;
    let nodes = tree(&lines, &mut cursor, 0);

    let mut emit = Emit::default();
    let header_line = lines.first().map_or(1, |l| l.no);
    let (header, body) = match nodes.first() {
        Some(first) if first.line.op == Op::Flow => {
            if !first.children.is_empty() {
                return Err(err(
                    first.children[0].line.no,
                    "the `F` flow header takes no body",
                ));
            }
            (flow_decl(&first.line)?, &nodes[1..])
        }
        _ => ("flow".to_string(), nodes.as_slice()),
    };
    emit.push(0, &header, header_line);
    emit_body(body, 1, &mut emit)?;

    crate::parse::parse(&emit.text()).map_err(|e| emit.remap(e))
}

/// A structural Glyph diagnostic, in the crate's `line N:` vocabulary.
fn err(line: usize, message: impl std::fmt::Display) -> FlowError {
    FlowError::Parse(format!("line {line}: {message}"))
}

/// What a line's first token means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Flow,
    Bind,
    Memo,
    Return,
    When,
    Match,
    Route,
    Parallel,
    Race,
    Fallback,
    Confirm,
    Assert,
    /// `|` — an arm of the enclosing construct.
    Arm,
    /// `|*` — the enclosing construct's default arm.
    Default,
    /// `@{…}` — the raw-AST escape.
    Escape,
    /// A canonical Flux one-liner, carried through verbatim.
    PassThrough,
}

/// Classify a line's first token. Shared by the reader and the writer, so the writer can only emit
/// a verbatim canonical line the reader will read back as one.
fn classify(head: &str) -> std::result::Result<Op, String> {
    Ok(match head {
        "F" => Op::Flow,
        "=" => Op::Bind,
        "~=" => Op::Memo,
        "^" => Op::Return,
        "?" => Op::When,
        "?=" => Op::Match,
        "?~" => Op::Route,
        "&" => Op::Parallel,
        "||" => Op::Race,
        "??" => Op::Fallback,
        "!?" => Op::Confirm,
        "!!" => Op::Assert,
        "|" => Op::Arm,
        "|*" => Op::Default,
        _ if head.starts_with(ESCAPE) => Op::Escape,
        // A token made only of opcode characters is a mistyped opcode, never a statement.
        _ if !head.is_empty() && head.chars().all(|c| SIGILS.contains(c)) => {
            return Err(format!("unknown opcode `{head}`"))
        }
        _ => Op::PassThrough,
    })
}

/// Split a statement into its first token and the trimmed remainder.
fn split_head(text: &str) -> (&str, &str) {
    match text.find(char::is_whitespace) {
        Some(at) => (&text[..at], text[at..].trim()),
        None => (text, ""),
    }
}

/// One classified Glyph line.
#[derive(Clone, Copy)]
struct Line<'a> {
    /// 1-based line number in the Glyph source — every diagnostic names it.
    no: usize,
    depth: usize,
    op: Op,
    head: &'a str,
    /// The text after the opcode, trimmed; empty when the opcode stands alone.
    operand: &'a str,
    /// The whole statement, without indentation (a pass-through line's verbatim text).
    text: &'a str,
}

/// Classify every significant line and validate the indentation shape. Blank lines and `#` comment
/// lines carry no structure and are dropped here, which is why a Glyph line number is not a
/// canonical line number and the two have to be mapped.
fn lex(src: &str) -> Result<Vec<Line<'_>>> {
    let mut lines = Vec::new();
    let mut previous: Option<usize> = None;
    for (index, raw) in src.lines().enumerate() {
        let no = index + 1;
        let line = raw.trim_end();
        let text = line.trim_start();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let leading = &line[..line.len() - text.len()];
        if leading.contains('\t') {
            return Err(err(
                no,
                "Glyph indents with spaces; a tab is never indentation",
            ));
        }
        if !leading.len().is_multiple_of(2) {
            return Err(err(
                no,
                format!(
                    "Glyph indents two spaces per level; this line starts with {}",
                    leading.len()
                ),
            ));
        }
        let depth = leading.len() / 2;
        match previous {
            None if depth != 0 => {
                return Err(err(no, "the first Glyph line must not be indented"));
            }
            Some(prev) if depth > prev + 1 => {
                return Err(err(
                    no,
                    format!("a body indents exactly one level; this line is {depth} levels deep under level {prev}"),
                ));
            }
            _ => {}
        }
        previous = Some(depth);
        let (head, operand) = split_head(text);
        let op = classify(head).map_err(|message| err(no, message))?;
        lines.push(Line {
            no,
            depth,
            op,
            head,
            operand,
            text,
        });
    }
    Ok(lines)
}

/// A Glyph line plus the block indented under it.
struct GNode<'a> {
    line: Line<'a>,
    children: Vec<GNode<'a>>,
}

/// Group the flat line list into a tree by indentation. `lex` already proved no level is skipped,
/// so every line either continues this level or belongs to a deeper block.
fn tree<'a>(lines: &[Line<'a>], cursor: &mut usize, depth: usize) -> Vec<GNode<'a>> {
    let mut nodes = Vec::new();
    while let Some(line) = lines.get(*cursor) {
        if line.depth != depth {
            break;
        }
        *cursor += 1;
        let children = tree(lines, cursor, depth + 1);
        nodes.push(GNode {
            line: *line,
            children,
        });
    }
    nodes
}

/// The canonical Flux expansion under construction, with the Glyph line every canonical line came
/// from — the map that lets a canonical diagnostic name a line the Glyph author can see.
#[derive(Default)]
struct Emit {
    lines: Vec<String>,
    map: Vec<usize>,
}

impl Emit {
    fn push(&mut self, level: usize, text: &str, line: usize) {
        self.lines.push(format!("{}{text}", INDENT.repeat(level)));
        self.map.push(line);
    }

    fn text(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }

    /// Rewrite a canonical `line N:` diagnostic to name the Glyph line instead.
    fn remap(&self, error: FlowError) -> FlowError {
        let FlowError::Parse(message) = &error else {
            return error;
        };
        let Some((number, tail)) = message
            .strip_prefix("line ")
            .and_then(|rest| rest.split_once(": "))
        else {
            return error;
        };
        let Ok(canonical) = number.trim().parse::<usize>() else {
            return error;
        };
        match self.map.get(canonical.wrapping_sub(1)) {
            Some(glyph) => FlowError::Parse(format!("line {glyph}: {tail}")),
            None => error,
        }
    }
}

/// The canonical `flow` header for an `F` line: `F [name][(param:Type, …)][>Return]`.
fn flow_decl(line: &Line<'_>) -> Result<String> {
    let operand = line.operand;
    let name_len = operand.find(['(', '>']).unwrap_or(operand.len());
    let name = operand[..name_len].trim();
    let mut rest = &operand[name_len..];

    let mut params: Vec<Param> = Vec::new();
    if let Some(after) = rest.strip_prefix('(') {
        let close = after
            .find(')')
            .ok_or_else(|| err(line.no, "the `F` parameter list is missing its `)`"))?;
        let inner = after[..close].trim();
        rest = &after[close + 1..];
        if !inner.is_empty() {
            for part in inner.split(',') {
                let part = part.trim();
                let (pname, ty) = part.split_once(':').ok_or_else(|| {
                    err(
                        line.no,
                        format!("the `F` parameter `{part}` needs a `name:Type`"),
                    )
                })?;
                params.push(Param {
                    name: pname.trim().into(),
                    ty: parse_type(ty.trim()),
                });
            }
        }
    }

    let returns = match rest.trim() {
        "" => None,
        other => match other.strip_prefix('>') {
            Some(ty) if !ty.trim().is_empty() => Some(parse_type(ty.trim())),
            _ => {
                return Err(err(
                    line.no,
                    format!("the `F` header does not understand `{other}`"),
                ))
            }
        },
    };

    let mut header = String::from("flow");
    if !name.is_empty() {
        header.push(' ');
        header.push_str(name);
    }
    if !params.is_empty() {
        header.push('(');
        let ps: Vec<String> = params
            .iter()
            .map(|p| format!("{}: {}", p.name.0, p.ty.label()))
            .collect();
        header.push_str(&ps.join(", "));
        header.push(')');
    }
    if let Some(ty) = &returns {
        header.push_str(" -> ");
        header.push_str(&ty.label());
    }
    Ok(header)
}

/// Read a header type label. The canonical header grammar re-derives the same [`TypeRef`] from the
/// label, so this only has to hand the label back through unchanged.
fn parse_type(label: &str) -> TypeRef {
    match label {
        "Any" => TypeRef::Any,
        "Bool" => TypeRef::Bool,
        "Number" => TypeRef::Number,
        "String" => TypeRef::String,
        other => match other
            .strip_prefix("List<")
            .and_then(|inner| inner.strip_suffix('>'))
        {
            Some(inner) => TypeRef::List(Box::new(parse_type(inner))),
            None => TypeRef::Named(other.to_string()),
        },
    }
}

fn emit_body(nodes: &[GNode<'_>], level: usize, out: &mut Emit) -> Result<()> {
    for node in nodes {
        emit_stmt(node, level, out)?;
    }
    Ok(())
}

/// The arms of an arm-taking construct, already validated for placement, ordering and uniqueness.
struct Arms<'a, 'b> {
    cases: Vec<&'b GNode<'a>>,
    default: Option<&'b GNode<'a>>,
}

/// Split a construct's children into arms. `labelled` is whether `|` arms are accepted at all (a
/// conditional takes only `|*`); `defaulted` is whether `|*` is. A statement child, a rejected arm
/// kind, a repeated default, or a default that is not last is an error — never a repair.
fn split_arms<'a, 'b>(
    parent: &'b GNode<'a>,
    labelled: bool,
    defaulted: bool,
) -> Result<Arms<'a, 'b>> {
    let mut cases = Vec::new();
    let mut default = None;
    for child in &parent.children {
        match child.line.op {
            Op::Arm if labelled => {
                if default.is_some() {
                    return Err(err(
                        child.line.no,
                        "the `|*` default arm must come last".to_string(),
                    ));
                }
                cases.push(child);
            }
            Op::Arm => {
                return Err(err(
                    child.line.no,
                    format!(
                        "`{}` takes only a `|*` default arm, not a labelled `|` arm",
                        parent.line.head
                    ),
                ))
            }
            Op::Default if defaulted => {
                if default.is_some() {
                    return Err(err(child.line.no, "duplicate `|*` default arm"));
                }
                default = Some(child);
            }
            Op::Default => {
                return Err(err(
                    child.line.no,
                    format!("`{}` has no default arm", parent.line.head),
                ))
            }
            _ => {
                return Err(err(
                    child.line.no,
                    format!(
                        "`{}` takes only `|` arms here, not a statement",
                        parent.line.head
                    ),
                ))
            }
        }
    }
    Ok(Arms { cases, default })
}

/// Reject a body under a line that cannot have one.
fn leaf_only(node: &GNode<'_>) -> Result<()> {
    match node.children.first() {
        None => Ok(()),
        Some(child) => Err(err(
            child.line.no,
            format!("`{}` is a leaf statement and takes no body", node.line.head),
        )),
    }
}

/// The operand of an opcode that requires one.
fn operand_of<'a>(node: &GNode<'a>, what: &str) -> Result<&'a str> {
    if node.line.operand.is_empty() {
        return Err(err(
            node.line.no,
            format!("`{}` needs {what}", node.line.head),
        ));
    }
    Ok(node.line.operand)
}

fn emit_stmt(node: &GNode<'_>, level: usize, out: &mut Emit) -> Result<()> {
    let at = node.line.no;
    match node.line.op {
        Op::Flow => Err(err(
            at,
            "the `F` flow header may appear only once, as the first Glyph line",
        )),
        Op::Arm | Op::Default => Err(err(
            at,
            format!(
                "`{}` is an arm: it needs an enclosing `?=`, `?~`, `&`, `||`, `??` or `?`",
                node.line.head
            ),
        )),
        Op::Bind | Op::Memo => {
            leaf_only(node)?;
            let operand = operand_of(node, "a name and a value")?;
            let (target, value) = split_head(operand);
            if value.is_empty() {
                return Err(err(
                    at,
                    format!("`{}` needs a value after `{target}`", node.line.head),
                ));
            }
            let target = match target.split_once(':') {
                Some((name, ty)) => format!("{name}: {ty}"),
                None => target.to_string(),
            };
            let keyword = if node.line.op == Op::Memo {
                "memo "
            } else {
                ""
            };
            out.push(level, &format!("{keyword}{target} = {value}"), at);
            Ok(())
        }
        Op::Return => {
            leaf_only(node)?;
            out.push(
                level,
                &format!("return {}", operand_of(node, "a value")?),
                at,
            );
            Ok(())
        }
        Op::Assert => {
            leaf_only(node)?;
            out.push(
                level,
                &format!("assert {}", operand_of(node, "a condition")?),
                at,
            );
            Ok(())
        }
        Op::Escape => {
            leaf_only(node)?;
            let json = node.line.text[ESCAPE.len_utf8()..].trim();
            if !json.starts_with('{') {
                return Err(err(
                    at,
                    "a malformed `@{…}` escape: it carries a compact JSON node",
                ));
            }
            serde_json::from_str::<Node>(json)
                .map_err(|e| err(at, format!("a malformed `@{{…}}` escape: {e}")))?;
            out.push(level, &format!("@json {json}"), at);
            Ok(())
        }
        Op::PassThrough => {
            leaf_only(node)?;
            out.push(level, node.line.text, at);
            Ok(())
        }
        // `? <cond>` — the then-body is every statement child; a trailing `|*` carries the else.
        Op::When => {
            let cond = operand_of(node, "a condition")?;
            out.push(level, &format!("when {cond}"), at);
            let mut then_end = node.children.len();
            for (index, child) in node.children.iter().enumerate() {
                match child.line.op {
                    Op::Default if index + 1 == node.children.len() => then_end = index,
                    Op::Default => {
                        return Err(err(child.line.no, "the `|*` else arm must come last"));
                    }
                    Op::Arm => {
                        return Err(err(
                            child.line.no,
                            "`?` takes only a `|*` else arm, not a labelled `|` arm",
                        ));
                    }
                    _ => {}
                }
            }
            emit_body(&node.children[..then_end], level + 1, out)?;
            if let Some(otherwise) = node.children.get(then_end) {
                if !otherwise.line.operand.is_empty() {
                    return Err(err(otherwise.line.no, "a `|*` else arm carries no label"));
                }
                out.push(level, "else", otherwise.line.no);
                emit_body(&otherwise.children, level + 1, out)?;
            }
            Ok(())
        }
        // `?= <subject>` / `?~ <selector>` — `| <value>` cases and an optional `|*` default.
        Op::Match | Op::Route => {
            let keyword = if node.line.op == Op::Match {
                "match"
            } else {
                "route"
            };
            let subject = operand_of(node, "a subject")?;
            out.push(level, &format!("{keyword} {subject}"), at);
            let arms = split_arms(node, true, true)?;
            for case in arms.cases {
                let value = operand_of(case, "a case value")?;
                out.push(level + 1, &format!("case {value}"), case.line.no);
                emit_body(&case.children, level + 2, out)?;
            }
            if let Some(default) = arms.default {
                out.push(level + 1, "default", default.line.no);
                emit_body(&default.children, level + 2, out)?;
            }
            Ok(())
        }
        // `&` / `|| <timeout>[ > bind]` — `| <name>` branches, whose names must be distinct.
        Op::Parallel | Op::Race => {
            let header = if node.line.op == Op::Parallel {
                if !node.line.operand.is_empty() {
                    return Err(err(at, "`&` takes no operand"));
                }
                "parallel".to_string()
            } else {
                let (timeout, bind) = split_bind(operand_of(node, "a timeout")?);
                match bind {
                    Some(b) => format!("race {timeout} -> {b}"),
                    None => format!("race {timeout}"),
                }
            };
            out.push(level, &header, at);
            let arms = split_arms(node, true, false)?;
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for branch in arms.cases {
                let name = operand_of(branch, "a branch name")?;
                if !seen.insert(name) {
                    return Err(err(
                        branch.line.no,
                        format!("duplicate branch name `{name}`"),
                    ));
                }
                out.push(level + 1, &format!("branch {name}"), branch.line.no);
                emit_body(&branch.children, level + 2, out)?;
            }
            Ok(())
        }
        // `??[ > bind]` — unlabelled `|` branches, tried in order.
        Op::Fallback => {
            let header = match split_bind(node.line.operand) {
                ("", Some(bind)) => format!("fallback -> {bind}"),
                ("", None) => "fallback".to_string(),
                (other, _) => {
                    return Err(err(
                        at,
                        format!("`??` takes only an optional `> bind`, not `{other}`"),
                    ))
                }
            };
            out.push(level, &header, at);
            let arms = split_arms(node, true, false)?;
            for branch in arms.cases {
                if !branch.line.operand.is_empty() {
                    return Err(err(
                        branch.line.no,
                        "a `??` fallback arm carries no label — it is tried in order",
                    ));
                }
                out.push(level + 1, "branch", branch.line.no);
                emit_body(&branch.children, level + 2, out)?;
            }
            Ok(())
        }
        // `!? "<message>"[ <risk>]` — the human-in-the-loop gate.
        Op::Confirm => {
            let operand = operand_of(node, "a message")?;
            let (message, rest) =
                split_string(operand).ok_or_else(|| err(at, "`!?` needs a quoted message"))?;
            let header = match rest.trim() {
                "" => format!("confirm {message}"),
                risk if format::is_word_token(risk) => format!("confirm {message}, risk: {risk}"),
                other => {
                    return Err(err(
                        at,
                        format!("`!?` takes a single-word risk after its message, not `{other}`"),
                    ))
                }
            };
            out.push(level, &header, at);
            emit_body(&node.children, level + 1, out)
        }
    }
}

/// Split an operand's trailing `> bind` off its head.
fn split_bind(operand: &str) -> (&str, Option<&str>) {
    match operand.split_once('>') {
        Some((head, bind)) => (head.trim(), Some(bind.trim()).filter(|b| !b.is_empty())),
        None => (operand.trim(), None),
    }
}

/// Split a leading JSON string literal off `text`, honouring backslash escapes so a `\"` inside the
/// string does not end it. Returns the literal (quotes included) and the remainder.
fn split_string(text: &str) -> Option<(&str, &str)> {
    let mut chars = text.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut escaped = false;
    for (at, ch) in chars {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            let end = at + ch.len_utf8();
            return Some((&text[..end], &text[end..]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SymbolName;

    /// The vocabulary is exactly the design's fourteen opcodes — no accidental thirteenth or
    /// fifteenth, and every one of them classifies back to an opcode rather than to a statement.
    #[test]
    fn every_opcode_classifies_as_an_opcode() {
        assert_eq!(OPCODES.len(), 14);
        for (op, _) in OPCODES {
            assert_ne!(
                classify(op).expect("a documented opcode classifies"),
                Op::PassThrough,
                "`{op}` must not read as a canonical statement"
            );
        }
    }

    /// A mistyped opcode is refused rather than handed to the canonical parser as if it were Flux.
    #[test]
    fn a_sigil_only_token_that_is_not_an_opcode_is_unknown() {
        for bad in ["?!", "=>", "&&", "|?", "^^"] {
            assert!(classify(bad).is_err(), "`{bad}` must not classify");
        }
    }

    /// A canonical statement whose first token happens to be sigil-shaped (`[]`, `{}`) is still a
    /// statement — the sigil set deliberately excludes bracket and brace.
    #[test]
    fn json_literal_statements_are_pass_through() {
        for text in ["[]", "{}", "-3", "read(\"a\")", "p += a", "$x"] {
            let (head, _) = split_head(text);
            assert_eq!(classify(head), Ok(Op::PassThrough), "{text}");
        }
    }

    /// `F` is the header opcode, so a statement that would be spelled `F` in canonical Flux (a read
    /// of a symbol named `F`) has to take the escape instead of being emitted verbatim.
    #[test]
    fn a_statement_that_would_read_as_an_opcode_takes_the_escape() {
        let node = Node::Var {
            name: SymbolName("F".into()),
        };
        assert!(pass_through(&node).is_none());
        let ast = DraftAst {
            body: vec![node],
            ..Default::default()
        };
        assert_eq!(
            format_glyph(&ast),
            "F\n@{\"kind\":\"var\",\"name\":\"F\"}\n"
        );
        assert_eq!(parse_glyph(&format_glyph(&ast)).unwrap(), ast);
    }

    /// Blank lines and `#` comments carry no structure, and a Glyph line number survives them.
    #[test]
    fn blank_and_comment_lines_are_ignored() {
        let ast = parse_glyph("# a note\n\nF f\n\n# another\n^ 1\n").unwrap();
        assert_eq!(ast.name.as_deref(), Some("f"));
        assert_eq!(ast.body.len(), 1);
    }

    /// The header round-trips its name, parameters and return type — including a generic label.
    #[test]
    fn the_flow_header_round_trips() {
        let ast = DraftAst {
            name: Some("triage".into()),
            params: vec![
                Param {
                    name: "ticket".into(),
                    ty: TypeRef::Named("Ticket".into()),
                },
                Param {
                    name: "tags".into(),
                    ty: TypeRef::List(Box::new(TypeRef::String)),
                },
            ],
            returns: Some(TypeRef::List(Box::new(TypeRef::Named("Answer".into())))),
            body: vec![],
        };
        assert_eq!(
            format_glyph(&ast),
            "F triage(ticket:Ticket, tags:List<String>)>List<Answer>\n"
        );
        assert_eq!(parse_glyph(&format_glyph(&ast)).unwrap(), ast);
    }

    /// A document with no `F` line is an anonymous flow; an empty document is the empty flow.
    #[test]
    fn the_header_is_optional() {
        assert_eq!(parse_glyph("").unwrap(), DraftAst::default());
        let ast = parse_glyph("^ 1\n").unwrap();
        assert!(ast.name.is_none() && ast.body.len() == 1);
    }

    /// A quoted message is scanned with escape awareness, so an embedded quote does not end it.
    #[test]
    fn a_confirm_message_may_contain_a_quote() {
        let ast = DraftAst {
            body: vec![Node::Confirm {
                message: "say \"hi\"".into(),
                risk: Some("high".into()),
                body: vec![],
            }],
            ..Default::default()
        };
        assert_eq!(format_glyph(&ast), "F\n!? \"say \\\"hi\\\"\" high\n");
        assert_eq!(parse_glyph(&format_glyph(&ast)).unwrap(), ast);
    }
}

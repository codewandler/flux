//! **Railflux** — the terminal-first, 7-bit ASCII *dataflow* projection of a [`DraftAst`] (L-95).
//!
//! One AST, several readable projections (`docs/designs/flux-notation-workbench.md`): canonical
//! `.flux` is the authored surface, [`crate::render`]'s indented tree is the execution-path view,
//! and Railflux is the **dataflow** view — what flows in, what stage consumes it, what it binds.
//!
//! This module is **output only and total**. There is no reader (that is L-100, deliberately
//! deferred until this shape has stabilized), no AST change, and no runtime behavior: it is a pure
//! `&DraftAst -> String` function, and equal ASTs produce byte-identical output.
//!
//! # The notation
//!
//! Exactly two line shapes, distinguished by their first character:
//!
//! - A **rail** — `sources --> stage --> sink` — is any line that does *not* start with `[`. The
//!   source list is the distinct symbol reads the stage consumes, in walk order; a stage is either
//!   a call `op(args)` or a bracketed pure expression `[…]`; the sink is a bound name, `memo name`,
//!   or `RETURN`. A stage with no inputs opens the line with a bare `--> `. When a value is just a
//!   symbol read there is no stage at all (`docs --> RETURN`).
//! - A **region** — `[label]`, optionally `[label] --> sink` — is any line that *starts* with `[`.
//!   Its body is the following lines indented two further spaces. Every construct that does not fit
//!   a horizontal rail (control flow, scopes, arms) is a region, and its arms are regions in turn
//!   (`[then]`, `[else]`, `[case …]`, `[branch]`, `[catch e]`, `[do]`, `[undo]`, `[finally]`, …).
//!
//! Region labels put the construct's **primary** field positionally and every other field as
//! `key: value` — the same "structural words stay visible, options carry labels" discipline the
//! design sets for canonical Flux headers. Nothing is elided: an absent optional field means the
//! AST's field is `None`/empty, never that the renderer ran out of room.
//!
//! Inside a stage, arguments and expressions use **canonical Flux expression syntax** (named inputs
//! `key: value`, punning, field access, `@json` for a node with no inline spelling) so a future
//! reader can reuse the existing expression grammar rather than grow a second one. The one Railflux
//! addition is `.` — "the value on the incoming rail" — used only when the stage has exactly one
//! source and the argument *is* that source, which keeps it unambiguous.
//!
//! # ASCII
//!
//! Canonical output is strictly 7-bit ASCII. Structural glyphs are ASCII by construction, and every
//! span of embedded content (names, literals, messages, selectors) passes through [`ascii_escape`],
//! which rewrites any non-ASCII scalar as a `\u`-escape — UTF-16 surrogate pairs above the BMP, so
//! an escaped JSON literal stays valid JSON — and any control byte likewise, so no content can
//! break the line-per-statement structure the diagram depends on.

use crate::ast::{
    is_bare_symbol_name, DraftAst, FlowEffect, Node, Selector, SymbolName, ThingKind, ThingRef,
    TypeRef,
};
use crate::format::{fmt_duration, fmt_field_path, fmt_obj_key, fmt_symbol};
use crate::render::{spans_to_string, Palette, Role};

/// One colored fragment of a rendered line — the same span form [`crate::render`] uses, so a
/// surface that already maps [`Role`]s to colors decorates Railflux without a second styling model.
type Span = (String, Role);

/// One nesting level. Regions indent their body by exactly this much.
const INDENT: &str = "  ";
/// The rail connector between two columns.
const ARROW: &str = " --> ";
/// The rail connector opening a line whose stage has no inputs.
const ARROW_HEAD: &str = "--> ";

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Render `ast` as a Railflux diagram (plain, no color). Pure and total: every node kind and every
/// semantically relevant field appears, equal ASTs render byte-identically, and the result is
/// strictly 7-bit ASCII. Ends with a trailing newline, like [`crate::render::render_pretty`].
pub fn render_rail(ast: &DraftAst) -> String {
    render_rail_styled(ast, &Palette::PLAIN)
}

/// Render `ast` as a Railflux diagram, wrapping each span with `p`'s role colors — the ANSI
/// stringification of [`render_rail_spans`], exactly as [`crate::render::render_styled`] relates to
/// [`crate::render::render_styled_spans`].
pub fn render_rail_styled(ast: &DraftAst, p: &Palette) -> String {
    let mut out = String::new();
    for line in render_rail_spans(ast) {
        out.push_str(&spans_to_string(&line, p));
        out.push('\n');
    }
    out
}

/// Render `ast` as Railflux in `(text, Role)` span form, one `Vec` per line — the single walk both
/// presentations build on. Concatenating a line's fragments yields exactly that line of
/// [`render_rail`]; every connector (`[`, `]`, `-->`, the indent runs) carries [`Role::Connector`],
/// so a non-ANSI surface can color the diagram without re-parsing it.
pub fn render_rail_spans(ast: &DraftAst) -> Vec<Vec<Span>> {
    let mut lines: Vec<Vec<Span>> = vec![flow_header(ast)];
    for node in &ast.body {
        stmt(node, 1, &mut lines);
    }
    for line in &mut lines {
        for (textual, _) in line.iter_mut() {
            *textual = ascii_escape(textual);
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// Span helpers
// ---------------------------------------------------------------------------

fn kw(s: &str) -> Span {
    (s.to_string(), Role::Keyword)
}

fn glue(s: impl Into<String>) -> Span {
    (s.into(), Role::Text)
}

fn conn(s: &str) -> Span {
    (s.to_string(), Role::Connector)
}

fn name_span(s: impl Into<String>) -> Span {
    (s.into(), Role::Symbol)
}

fn num_span(s: impl Into<String>) -> Span {
    (s.into(), Role::Lit)
}

/// A JSON-quoted string span (`"Open issue?"`).
fn text_span(s: &str) -> Span {
    (json_string(s), Role::String)
}

fn json_string(s: &str) -> String {
    serde_json::to_string(&serde_json::Value::String(s.to_string()))
        .unwrap_or_else(|_| "\"\"".to_string())
}

fn json_compact<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// Rewrite every non-ASCII scalar and every control byte as a `\u` escape. Applied once, to every
/// span, so the canonical-ASCII guarantee cannot be lost by forgetting it at one call site.
/// Astral-plane scalars become UTF-16 surrogate pairs, which keeps an escaped JSON string literal
/// valid JSON.
fn ascii_escape(s: &str) -> String {
    if s.is_ascii() && !s.chars().any(char::is_control) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii() && !ch.is_control() {
            out.push(ch);
        } else {
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// Prefix `spans` with this depth's indent run.
fn line(depth: usize, spans: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len() + 1);
    if depth > 0 {
        out.push(conn(&INDENT.repeat(depth)));
    }
    out.extend(spans);
    out
}

/// Emit `[label]`, optionally followed by ` --> sink`.
fn region(lines: &mut Vec<Vec<Span>>, depth: usize, label: Vec<Span>, sink: Option<Vec<Span>>) {
    let mut spans = vec![conn("[")];
    spans.extend(label);
    spans.push(conn("]"));
    if let Some(sink) = sink {
        spans.push(conn(ARROW));
        spans.extend(sink);
    }
    lines.push(line(depth, spans));
}

/// Emit `[label]` and then `body`, indented one level further.
fn block(
    lines: &mut Vec<Vec<Span>>,
    depth: usize,
    label: Vec<Span>,
    sink: Option<Vec<Span>>,
    body: &[Node],
) {
    region(lines, depth, label, sink);
    for node in body {
        stmt(node, depth + 1, lines);
    }
}

/// Append ` key: <value>` to a region label — the labelled-option form every non-primary field
/// uses.
fn opt(label: &mut Vec<Span>, key: &str, value: Vec<Span>) {
    label.push(glue(format!(" {key}: ")));
    label.extend(value);
}

// ---------------------------------------------------------------------------
// The flow header
// ---------------------------------------------------------------------------

fn flow_header(ast: &DraftAst) -> Vec<Span> {
    let mut label = vec![kw("flow")];
    if let Some(name) = &ast.name {
        label.push(glue(" "));
        label.push(name_span(name.clone()));
    }
    if !ast.params.is_empty() {
        label.push(glue(" ("));
        for (i, param) in ast.params.iter().enumerate() {
            if i > 0 {
                label.push(glue(", "));
            }
            label.push(name_span(fmt_symbol(&param.name)));
            label.push(glue(format!(": {}", param.ty.label())));
        }
        label.push(glue(")"));
    }
    if let Some(returns) = &ast.returns {
        label.push(glue(format!(" -> {}", returns.label())));
    }
    let mut spans = vec![conn("[")];
    spans.extend(label);
    spans.push(conn("]"));
    spans
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

/// Whether `node` renders as a rail — a call or a pure value expression. Everything else is a
/// region. Exhaustive on purpose (no `_` arm): a new node kind fails compilation here rather than
/// silently picking a shape.
fn is_rail_shaped(node: &Node) -> bool {
    match node {
        Node::Call { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Obj { .. }
        | Node::List { .. }
        | Node::Jq { .. }
        | Node::Fmt { .. }
        | Node::Expr { .. }
        | Node::Parse { .. }
        | Node::Peek { .. }
        | Node::Thing { .. } => true,
        Node::Bind { .. }
        | Node::When { .. }
        | Node::Repeat { .. }
        | Node::Each { .. }
        | Node::Assert { .. }
        | Node::Pipe { .. }
        | Node::Seq { .. }
        | Node::Memo { .. }
        | Node::Parallel { .. }
        | Node::Await { .. }
        | Node::Retry { .. }
        | Node::Try { .. }
        | Node::Confirm { .. }
        | Node::Loop { .. }
        | Node::Race { .. }
        | Node::Throttle { .. }
        | Node::Debounce { .. }
        | Node::Unless { .. }
        | Node::Verify { .. }
        | Node::Return { .. }
        | Node::Ctx { .. }
        | Node::CtxAppend { .. }
        | Node::Match { .. }
        | Node::Route { .. }
        | Node::Fallback { .. }
        | Node::Timeout { .. }
        | Node::Budget { .. }
        | Node::CapScope { .. }
        | Node::Scope { .. }
        | Node::Saga { .. }
        | Node::Once { .. }
        | Node::Checkpoint { .. } => false,
    }
}

fn stmt(node: &Node, depth: usize, lines: &mut Vec<Vec<Span>>) {
    match node {
        // --- rails -----------------------------------------------------------------------
        Node::Call { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Obj { .. }
        | Node::List { .. }
        | Node::Jq { .. }
        | Node::Fmt { .. }
        | Node::Expr { .. }
        | Node::Parse { .. }
        | Node::Peek { .. }
        | Node::Thing { .. } => rail(lines, depth, node, None),

        Node::Bind {
            name,
            value,
            ty,
            effect,
        } => valued(
            lines,
            depth,
            value,
            "bind",
            bind_sink(false, name, ty, effect),
        ),
        Node::Memo {
            name,
            value,
            ty,
            effect,
        } => valued(
            lines,
            depth,
            value,
            "memo",
            bind_sink(true, name, ty, effect),
        ),
        Node::Return { value } => valued(lines, depth, value, "return", vec![kw("RETURN")]),

        // --- control flow ----------------------------------------------------------------
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            let mut label = vec![kw("when"), glue(" ")];
            label.extend(inline(cond, None));
            region(lines, depth, label, None);
            block(lines, depth + 1, vec![kw("then")], None, then);
            if !otherwise.is_empty() {
                block(lines, depth + 1, vec![kw("else")], None, otherwise);
            }
        }
        Node::Unless { cond, body } => {
            let mut label = vec![kw("unless"), glue(" ")];
            label.extend(inline(cond, None));
            block(lines, depth, label, None, body);
        }
        Node::Match {
            subject,
            cases,
            default,
        } => {
            let mut label = vec![kw("match"), glue(" ")];
            label.extend(inline(subject, None));
            region(lines, depth, label, None);
            for case in cases {
                let mut arm = vec![kw("case"), glue(" ")];
                arm.extend(inline(&case.value, None));
                block(lines, depth + 1, arm, None, &case.body);
            }
            if !default.is_empty() {
                block(lines, depth + 1, vec![kw("default")], None, default);
            }
        }
        Node::Route {
            selector,
            cases,
            default,
        } => {
            let mut label = vec![kw("route"), glue(" ")];
            label.extend(inline(selector, None));
            region(lines, depth, label, None);
            for case in cases {
                let arm = vec![kw("case"), glue(" "), text_span(&case.label)];
                block(lines, depth + 1, arm, None, &case.body);
            }
            if !default.is_empty() {
                block(lines, depth + 1, vec![kw("default")], None, default);
            }
        }

        // --- fan-out ---------------------------------------------------------------------
        Node::Parallel { branches } => {
            region(lines, depth, vec![kw("parallel")], None);
            for branch in branches {
                block(
                    lines,
                    depth + 1,
                    vec![kw("branch")],
                    Some(vec![name_span(fmt_symbol(&branch.name))]),
                    &branch.body,
                );
            }
        }
        Node::Race {
            timeout_ms,
            branches,
            bind,
        } => {
            let label = vec![kw("race"), glue(" "), num_span(fmt_duration(*timeout_ms))];
            region(lines, depth, label, bind_only(bind));
            for branch in branches {
                block(
                    lines,
                    depth + 1,
                    vec![kw("branch")],
                    Some(vec![name_span(fmt_symbol(&branch.name))]),
                    &branch.body,
                );
            }
        }
        Node::Fallback { branches, bind } => {
            region(lines, depth, vec![kw("fallback")], bind_only(bind));
            for branch in branches {
                block(lines, depth + 1, vec![kw("branch")], None, &branch.body);
            }
        }

        // --- sequencing and scopes -------------------------------------------------------
        Node::Pipe { steps, bind } => block(lines, depth, vec![kw("pipe")], bind_only(bind), steps),
        Node::Seq { body, bind } => block(lines, depth, vec![kw("seq")], bind_only(bind), body),
        Node::Try {
            body,
            catch,
            handler,
        } => {
            region(lines, depth, vec![kw("try")], None);
            block(lines, depth + 1, vec![kw("do")], None, body);
            if catch.is_some() || !handler.is_empty() {
                let mut arm = vec![kw("catch")];
                if let Some(name) = catch {
                    arm.push(glue(" "));
                    arm.push(name_span(fmt_symbol(name)));
                }
                block(lines, depth + 1, arm, None, handler);
            }
        }
        Node::Scope {
            acquire,
            bind,
            body,
            finally,
        } => {
            region(lines, depth, vec![kw("scope")], bind_only(bind));
            if let Some(acquire) = acquire {
                region(lines, depth + 1, vec![kw("acquire")], None);
                stmt(acquire, depth + 2, lines);
            }
            block(lines, depth + 1, vec![kw("do")], None, body);
            if !finally.is_empty() {
                block(lines, depth + 1, vec![kw("finally")], None, finally);
            }
        }
        Node::Saga { steps } => {
            region(lines, depth, vec![kw("saga")], None);
            for step in steps {
                region(lines, depth + 1, vec![kw("step")], None);
                block(lines, depth + 2, vec![kw("do")], None, &step.body);
                if !step.undo.is_empty() {
                    block(lines, depth + 2, vec![kw("undo")], None, &step.undo);
                }
            }
        }

        // --- loops -----------------------------------------------------------------------
        Node::Repeat {
            max,
            until,
            body,
            collect,
        } => {
            let mut label = vec![kw("repeat"), glue(" "), num_span(max.to_string())];
            if let Some(until) = until {
                opt(&mut label, "until", inline(until, None));
            }
            if let Some(collect) = collect {
                opt(&mut label, "collect", vec![name_span(fmt_symbol(collect))]);
            }
            block(lines, depth, label, None, body);
        }
        Node::Each {
            source,
            item,
            body,
            collect,
            flat,
        } => {
            let mut label = vec![
                kw("each"),
                glue(" "),
                name_span(fmt_symbol(item)),
                glue(" "),
                kw("in"),
                glue(" "),
            ];
            label.extend(inline(source, None));
            if let Some(collect) = collect {
                opt(&mut label, "collect", vec![name_span(fmt_symbol(collect))]);
            }
            if *flat {
                opt(&mut label, "flat", vec![num_span("true")]);
            }
            block(lines, depth, label, None, body);
        }
        Node::Loop {
            for_ms,
            every_ms,
            until,
            body,
            bind,
        } => {
            let mut label = vec![kw("loop"), glue(" "), num_span(fmt_duration(*for_ms))];
            opt(&mut label, "every", vec![num_span(fmt_duration(*every_ms))]);
            if let Some(until) = until {
                opt(&mut label, "until", inline(until, None));
            }
            block(lines, depth, label, bind_only(bind), body);
        }

        // --- reliability guard-rails -----------------------------------------------------
        Node::Retry {
            max,
            backoff,
            delay_ms,
            body,
            bind,
        } => {
            let mut label = vec![kw("retry"), glue(" "), num_span(max.to_string())];
            if let Some(backoff) = backoff {
                opt(&mut label, "backoff", vec![text_span(backoff)]);
            }
            if let Some(delay) = delay_ms {
                opt(&mut label, "delay", vec![num_span(fmt_duration(*delay))]);
            }
            block(lines, depth, label, bind_only(bind), body);
        }
        Node::Timeout { ms, body, bind } => {
            let label = vec![kw("timeout"), glue(" "), num_span(fmt_duration(*ms))];
            block(lines, depth, label, bind_only(bind), body);
        }
        Node::Budget { limit, body, bind } => {
            let label = vec![kw("budget"), glue(" "), num_span(limit.to_string())];
            block(lines, depth, label, bind_only(bind), body);
        }
        Node::Throttle {
            name,
            max,
            window_ms,
            body,
        } => {
            let mut label = vec![kw("throttle"), glue(" "), text_span(name)];
            opt(&mut label, "max", vec![num_span(max.to_string())]);
            opt(
                &mut label,
                "window",
                vec![num_span(fmt_duration(*window_ms))],
            );
            block(lines, depth, label, None, body);
        }
        Node::Debounce {
            name,
            wait_ms,
            body,
        } => {
            let mut label = vec![kw("debounce"), glue(" "), text_span(name)];
            opt(&mut label, "wait", vec![num_span(fmt_duration(*wait_ms))]);
            block(lines, depth, label, None, body);
        }
        Node::CapScope { tools, body, bind } => {
            let label = vec![
                kw("with_tools"),
                glue(" "),
                (
                    format!(
                        "[{}]",
                        tools
                            .iter()
                            .map(|t| json_string(t))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Role::String,
                ),
            ];
            block(lines, depth, label, bind_only(bind), body);
        }
        Node::Once { label, body, bind } => {
            let head = vec![kw("once"), glue(" "), text_span(label)];
            block(lines, depth, head, bind_only(bind), body);
        }

        // --- gates and markers -----------------------------------------------------------
        Node::Confirm {
            message,
            risk,
            body,
        } => {
            let mut label = vec![kw("confirm"), glue(" "), text_span(message)];
            if let Some(risk) = risk {
                opt(&mut label, "risk", vec![text_span(risk)]);
            }
            block(lines, depth, label, None, body);
        }
        Node::Assert { cond, message } => {
            let mut label = vec![kw("assert"), glue(" ")];
            label.extend(inline(cond, None));
            if let Some(message) = message {
                opt(&mut label, "message", vec![text_span(message)]);
            }
            region(lines, depth, label, None);
        }
        Node::Verify {
            cmd,
            expect,
            message,
        } => {
            let mut label = vec![kw("verify"), glue(" ")];
            label.extend(inline(cmd, None));
            label.push(glue(" "));
            label.push(kw("contains"));
            label.push(glue(" "));
            label.extend(inline(expect, None));
            if let Some(message) = message {
                opt(&mut label, "message", vec![text_span(message)]);
            }
            region(lines, depth, label, None);
        }
        Node::Await {
            binding,
            source,
            as_type,
            condition,
        } => {
            let mut label = vec![kw("await"), glue(" "), text_span(source)];
            if let Some(ty) = as_type {
                opt(&mut label, "as", vec![glue(ty.label())]);
            }
            if let Some(condition) = condition {
                opt(&mut label, "when", inline(condition, None));
            }
            region(lines, depth, label, bind_only(binding));
        }
        Node::Checkpoint { label } => region(
            lines,
            depth,
            vec![kw("checkpoint"), glue(" "), text_span(label)],
            None,
        ),

        // --- context packs ---------------------------------------------------------------
        Node::Ctx {
            name,
            purpose,
            include,
            exclude,
            budget,
        } => {
            let mut label = vec![kw("ctx")];
            if let Some(purpose) = purpose {
                opt(&mut label, "purpose", vec![text_span(purpose)]);
            }
            if !include.is_empty() {
                opt(&mut label, "include", vec![sym_list(include)]);
            }
            if !exclude.is_empty() {
                opt(&mut label, "exclude", vec![sym_list(exclude)]);
            }
            if let Some(budget) = budget {
                opt(&mut label, "budget", vec![num_span(budget.to_string())]);
            }
            region(lines, depth, label, Some(vec![name_span(fmt_symbol(name))]));
        }
        Node::CtxAppend { ctx, add } => {
            let mut label = vec![kw("ctx_append")];
            opt(&mut label, "add", vec![sym_list(add)]);
            region(lines, depth, label, Some(vec![name_span(fmt_symbol(ctx))]));
        }
    }
}

/// A `bind`/`memo`/`return` whose value may itself be a block construct: a rail when the value is
/// a call or pure expression, otherwise a labelled region carrying the sink with the value nested
/// inside it. Keeps the projection total for a host-built AST that binds, say, a `seq`.
fn valued(lines: &mut Vec<Vec<Span>>, depth: usize, value: &Node, word: &str, sink: Vec<Span>) {
    if is_rail_shaped(value) {
        rail(lines, depth, value, Some(sink));
    } else {
        let sink = (word != "return").then_some(sink);
        region(lines, depth, vec![kw(word)], sink);
        stmt(value, depth + 1, lines);
    }
}

/// The sink column of a bind or memo: `name`, `name: Type`, `memo name !effect`, …
fn bind_sink(
    memo: bool,
    name: &SymbolName,
    ty: &Option<TypeRef>,
    effect: &Option<FlowEffect>,
) -> Vec<Span> {
    let mut spans = Vec::new();
    if memo {
        spans.push(kw("memo"));
        spans.push(glue(" "));
    }
    spans.push(name_span(fmt_symbol(name)));
    if let Some(ty) = ty {
        spans.push(glue(format!(": {}", ty.label())));
    }
    if let Some(effect) = effect {
        spans.push((format!(" !{}", effect.tag()), Role::Effect));
    }
    spans
}

/// The optional `--> name` sink a block construct carries when it binds its body's result.
fn bind_only(bind: &Option<SymbolName>) -> Option<Vec<Span>> {
    bind.as_ref().map(|name| vec![name_span(fmt_symbol(name))])
}

fn sym_list(names: &[SymbolName]) -> Span {
    (
        format!(
            "[{}]",
            names.iter().map(fmt_symbol).collect::<Vec<_>>().join(", ")
        ),
        Role::Symbol,
    )
}

/// Emit one rail: `sources --> stage --> sink`. A value that is itself just a symbol read has no
/// stage at all (`docs --> RETURN`); a stage with no inputs opens with a bare `--> `.
fn rail(lines: &mut Vec<Vec<Span>>, depth: usize, value: &Node, sink: Option<Vec<Span>>) {
    let sources = sources_of(value);
    let mut spans: Vec<Span> = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        if i > 0 {
            spans.push(glue(", "));
        }
        spans.push(name_span(source.clone()));
    }

    // A pure move: the whole value is one symbol read and it goes straight to the sink.
    if let (Some(_), Some(sink)) = (read_chain(value), sink.as_ref()) {
        spans.push(conn(ARROW));
        spans.extend(sink.clone());
        lines.push(line(depth, spans));
        return;
    }

    spans.push(conn(if sources.is_empty() {
        ARROW_HEAD
    } else {
        ARROW
    }));
    let sole = (sources.len() == 1).then(|| sources[0].as_str());
    spans.extend(stage(value, sole));
    if let Some(sink) = sink {
        spans.push(conn(ARROW));
        spans.extend(sink);
    }
    lines.push(line(depth, spans));
}

/// The stage column: a call renders as `op(args)`, every other value as a bracketed expression.
fn stage(value: &Node, sole: Option<&str>) -> Vec<Span> {
    match value {
        Node::Call { .. } => inline(value, sole),
        _ => {
            let mut spans = vec![conn("[")];
            spans.extend(inline(value, sole));
            spans.push(conn("]"));
            spans
        }
    }
}

// ---------------------------------------------------------------------------
// Sources — the left column
// ---------------------------------------------------------------------------

/// The distinct symbol reads a stage consumes, in walk order. This is the dataflow the diagram
/// exists to show; the stage still spells its own arguments, so the two columns are a summary and
/// its detail rather than a division of the truth.
fn sources_of(value: &Node) -> Vec<String> {
    let mut out = Vec::new();
    collect_reads(value, &mut out);
    out
}

fn collect_reads(node: &Node, out: &mut Vec<String>) {
    if let Some(chain) = read_chain(node) {
        if !out.contains(&chain) {
            out.push(chain);
        }
        return;
    }
    match node {
        Node::Call { args, .. } => args.iter().for_each(|a| collect_reads(a, out)),
        Node::Obj { fields } => fields.values().for_each(|v| collect_reads(v, out)),
        Node::List { items } => items.iter().for_each(|i| collect_reads(i, out)),
        Node::Jq { input, .. } => collect_reads(input, out),
        Node::Parse { value, .. } => collect_reads(value, out),
        Node::Expr { vars, .. } => vars.values().for_each(|v| collect_reads(v, out)),
        // `peek` reads a symbol out of the session store — a rail edge like any other. The name is
        // a declaration inside the stage, so it is never replaced by `.`.
        Node::Peek { name } => {
            let chain = fmt_symbol(name);
            if !out.contains(&chain) {
                out.push(chain);
            }
        }
        // Everything else is either a leaf with no reads or a block construct, which is a region
        // and therefore never occupies a stage column.
        _ => {}
    }
}

/// The canonical text of a symbol read — `ticket`, `ticket.title`, `raw.items[0].name?` — or `None`
/// when `node` is not a read chain.
fn read_chain(node: &Node) -> Option<String> {
    match node {
        Node::Var { name } => Some(fmt_symbol(name)),
        Node::Jq {
            path,
            input,
            optional,
        } => {
            let base = read_chain(input)?;
            let path = fmt_field_path(path)?;
            Some(format!("{base}{path}{}", if *optional { "?" } else { "" }))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Inline expressions
// ---------------------------------------------------------------------------

/// Render `node` as a one-line expression in canonical Flux spelling. `sole` is the stage's only
/// source, if it has exactly one: an argument that *is* that source renders as `.`, the single
/// Railflux-specific token, meaning "the value on the incoming rail".
fn inline(node: &Node, sole: Option<&str>) -> Vec<Span> {
    if let (Some(chain), Some(sole)) = (read_chain(node), sole) {
        return vec![if chain == sole {
            conn(".")
        } else {
            name_span(chain)
        }];
    }
    if let Some(chain) = read_chain(node) {
        return vec![name_span(chain)];
    }
    match node {
        Node::Call { op, args } => {
            let mut spans = vec![(op.clone(), Role::Op), glue("(")];
            spans.extend(call_args(args, sole));
            spans.push(glue(")"));
            spans
        }
        Node::Lit { value } => vec![(
            json_compact(value),
            if value.is_string() {
                Role::String
            } else {
                Role::Lit
            },
        )],
        Node::Obj { fields } => {
            if fields.is_empty() {
                return vec![glue("{}")];
            }
            let mut spans = vec![glue("{ ")];
            for (i, (key, value)) in fields.iter().enumerate() {
                if i > 0 {
                    spans.push(glue(", "));
                }
                if is_pun(key, value) {
                    spans.push(name_span(key.clone()));
                } else {
                    spans.push(glue(format!("{}: ", fmt_obj_key(key))));
                    spans.extend(inline(value, sole));
                }
            }
            spans.push(glue(" }"));
            spans
        }
        Node::List { items } => {
            if items.is_empty() {
                return vec![glue("[]")];
            }
            let mut spans = vec![glue("[ ")];
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    spans.push(glue(", "));
                }
                spans.extend(inline(item, sole));
            }
            spans.push(glue(" ]"));
            spans
        }
        Node::Fmt { template } => vec![
            ("fmt".to_string(), Role::Op),
            glue("("),
            text_span(template),
            glue(")"),
        ],
        Node::Expr { formula, vars } => {
            let mut spans = vec![
                ("expr".to_string(), Role::Op),
                glue("("),
                text_span(formula),
            ];
            for (key, value) in vars {
                spans.push(glue(format!(", {key}: ")));
                spans.extend(inline(value, sole));
            }
            spans.push(glue(")"));
            spans
        }
        Node::Parse { value, as_type } => {
            let mut spans = vec![("parse".to_string(), Role::Op), glue("(")];
            spans.extend(inline(value, sole));
            spans.push(glue(", as: "));
            spans.push(text_span(as_type));
            spans.push(glue(")"));
            spans
        }
        // A `jq` that is not a spellable field chain keeps its explicit form, traversal flag and
        // all — the flag decides whether a missing key is an error, so it is never dropped.
        Node::Jq {
            path,
            input,
            optional,
        } => {
            let mut spans = vec![
                ("jq".to_string(), Role::Op),
                glue("("),
                text_span(path),
                glue(", "),
            ];
            spans.extend(inline(input, sole));
            spans.push(glue(format!(", optional: {optional})")));
            spans
        }
        Node::Peek { name } => vec![kw("peek"), glue(" "), name_span(fmt_symbol(name))],
        Node::Thing { thing } => vec![thing_span(thing)],
        // A block construct in expression position has no inline spelling. Fall back to the
        // language's own raw-node escape rather than inventing a lossy summary — `@json` carries
        // the whole subtree, which is what "never omit semantic fields" requires.
        other => vec![kw("@json"), glue(" "), (json_compact(other), Role::Lit)],
    }
}

/// Whether an object field is the canonical pun `{ name }` — key and value name agree, so the key
/// alone spells it. A punned key is a declaration, never replaced by `.`.
fn is_pun(key: &str, value: &Node) -> bool {
    is_bare_symbol_name(key) && matches!(value, Node::Var { name } if name.0 == *key)
}

/// Call arguments in canonical Flux spelling: a single object argument projects as named inputs
/// (`query: ticket`, or the bare pun `ticket`), anything else stays positional. Two or more bare
/// symbol arguments keep their `$` sigil, exactly as the formatter does, so an explicitly
/// positional legacy call stays distinguishable from the named-input pun surface.
fn call_args(args: &[Node], sole: Option<&str>) -> Vec<Span> {
    if let [Node::Obj { fields }] = args {
        let mut spans = Vec::new();
        for (i, (key, value)) in fields.iter().enumerate() {
            if i > 0 {
                spans.push(glue(", "));
            }
            if fields.len() > 1 && is_pun(key, value) {
                spans.push(name_span(key.clone()));
            } else {
                spans.push(glue(format!("{}: ", fmt_obj_key(key))));
                spans.extend(inline(value, sole));
            }
        }
        return spans;
    }
    let multiple = args.len() > 1;
    let mut spans = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            spans.push(glue(", "));
        }
        match arg {
            Node::Var { name } if multiple && name.is_identifier() => {
                spans.push(name_span(format!("${}", name.0)));
            }
            _ => spans.extend(inline(arg, sole)),
        }
    }
    spans
}

fn thing_span(thing: &ThingRef) -> Span {
    let kind = match &thing.kind {
        ThingKind::Context => "context",
        ThingKind::File => "file",
        ThingKind::Person => "person",
        ThingKind::Ticket => "ticket",
        ThingKind::Email => "email",
        ThingKind::Repo => "repo",
        ThingKind::Dataset => "dataset",
        ThingKind::CalendarEvent => "calendar_event",
        ThingKind::Url => "url",
        ThingKind::Secret => "secret",
        ThingKind::Custom(custom) => custom.as_str(),
    };
    let (word, value) = match &thing.selector {
        Selector::Id(v) => ("id", v),
        Selector::Name(v) => ("name", v),
        Selector::Path(v) => ("path", v),
        Selector::Query(v) => ("query", v),
        Selector::Key(v) => ("key", v),
    };
    (
        format!("thing {kind} {word} {}", json_string(value)),
        Role::Thing,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Param;

    fn flow(body: Vec<Node>) -> DraftAst {
        DraftAst {
            body,
            ..Default::default()
        }
    }

    #[test]
    fn a_rail_is_sources_stage_sink_and_a_region_starts_the_line() {
        // The one lexical rule the notation rests on: a line starting with `[` is a region, and
        // every other line is a rail carrying `-->`. A future reader disambiguates on that alone.
        let ast = flow(vec![Node::Seq {
            body: vec![Node::Bind {
                name: SymbolName("doc".into()),
                value: Box::new(Node::Call {
                    op: "read".into(),
                    args: vec![Node::Var {
                        name: SymbolName("path".into()),
                    }],
                }),
                ty: None,
                effect: None,
            }],
            bind: None,
        }]);
        let out = render_rail(&ast);
        for line in out.lines() {
            let trimmed = line.trim_start();
            assert_eq!(
                trimmed.starts_with('['),
                !trimmed.contains("-->"),
                "line is either a region or a rail, never both: {line:?}"
            );
        }
        assert!(out.contains("path --> read(.) --> doc"), "got: {out}");
    }

    #[test]
    fn the_dot_stands_for_the_sole_source_only() {
        // `.` means "the value on the incoming rail". With two sources there is no single incoming
        // value, so every argument is spelled by name and the shorthand stays unambiguous.
        let one = flow(vec![Node::Call {
            op: "search".into(),
            args: vec![Node::Obj {
                fields: std::collections::BTreeMap::from([(
                    "query".to_string(),
                    Box::new(Node::Var {
                        name: SymbolName("ticket".into()),
                    }),
                )]),
            }],
        }]);
        assert!(
            render_rail(&one).contains("ticket --> search(query: .)"),
            "got: {}",
            render_rail(&one)
        );

        let two = flow(vec![Node::Call {
            op: "diff".into(),
            args: vec![Node::Obj {
                fields: std::collections::BTreeMap::from([
                    (
                        "left".to_string(),
                        Box::new(Node::Var {
                            name: SymbolName("a".into()),
                        }),
                    ),
                    (
                        "right".to_string(),
                        Box::new(Node::Var {
                            name: SymbolName("b".into()),
                        }),
                    ),
                ]),
            }],
        }]);
        let rendered = render_rail(&two);
        assert!(
            rendered.contains("a, b --> diff(left: a, right: b)"),
            "got: {rendered}"
        );
        assert!(!rendered.contains('.'), "no ambiguous dot: {rendered}");
    }

    #[test]
    fn a_block_construct_in_expression_position_escapes_to_json() {
        // Totality over a host-built AST: binding a `seq` has no rail shape, so the bind becomes a
        // labelled region carrying the sink, and nothing about the nested node is dropped.
        let ast = flow(vec![Node::Bind {
            name: SymbolName("out".into()),
            value: Box::new(Node::Seq {
                body: vec![Node::Call {
                    op: "read".into(),
                    args: vec![],
                }],
                bind: None,
            }),
            ty: None,
            effect: None,
        }]);
        let out = render_rail(&ast);
        assert_eq!(
            out,
            "[flow]\n  [bind] --> out\n    [seq]\n      --> read()\n"
        );

        // And in a genuine expression slot (a call argument) the raw-node escape carries it.
        let arg = flow(vec![Node::Call {
            op: "wrap".into(),
            args: vec![Node::Checkpoint { label: "x".into() }],
        }]);
        assert!(
            render_rail(&arg).contains("@json {\"kind\":\"checkpoint\",\"label\":\"x\"}"),
            "got: {}",
            render_rail(&arg)
        );
    }

    #[test]
    fn spans_join_to_the_plain_render_and_connectors_carry_their_role() {
        let ast = DraftAst {
            name: Some("t".into()),
            params: vec![Param {
                name: SymbolName("p".into()),
                ty: TypeRef::String,
            }],
            returns: Some(TypeRef::String),
            body: vec![Node::When {
                cond: Box::new(Node::Var {
                    name: SymbolName("ok".into()),
                }),
                then: vec![Node::Return {
                    value: Box::new(Node::Var {
                        name: SymbolName("p".into()),
                    }),
                }],
                otherwise: vec![Node::Call {
                    op: "log".into(),
                    args: vec![],
                }],
            }],
        };
        let lines = render_rail_spans(&ast);
        let joined: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|(text, _)| text.as_str()).collect())
            .collect();
        let plain = render_rail(&ast);
        assert_eq!(joined, plain.lines().collect::<Vec<_>>());
        for (text, role) in lines.iter().flatten() {
            if text.contains("-->") || text.starts_with('[') || text.ends_with(']') {
                assert_eq!(*role, Role::Connector, "{text:?} must be a connector");
            }
        }
    }

    #[test]
    fn a_colored_palette_wraps_the_same_walk() {
        let ast = flow(vec![Node::Bind {
            name: SymbolName("x".into()),
            value: Box::new(Node::Call {
                op: "read".into(),
                args: vec![Node::Lit {
                    value: serde_json::json!("f"),
                }],
            }),
            ty: None,
            effect: Some(FlowEffect::Read),
        }]);
        assert_eq!(render_rail_styled(&ast, &Palette::PLAIN), render_rail(&ast));
        let palette = Palette {
            op: ("<op>", "</op>"),
            symbol: ("<s>", "</s>"),
            string: ("<str>", "</str>"),
            ..Palette::PLAIN
        };
        let styled = render_rail_styled(&ast, &palette);
        assert!(styled.contains("<op>read</op>"), "got: {styled}");
        assert!(styled.contains("<s>x</s>"), "got: {styled}");
        assert!(styled.contains("<str>\"f\"</str>"), "got: {styled}");
    }
}

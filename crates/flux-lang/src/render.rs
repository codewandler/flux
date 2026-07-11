//! Pretty-rendering of a [`DraftAst`] as a human-readable execution-path tree — the `pretty` output of
//! `--plan` / `--compile-only`, and the live plan view the engine surfaces before executing.
//!
//! One tree walk, two presentations: [`render_styled_spans`] produces lines of `(text, Role)` spans —
//! the form a non-ANSI surface (SVG, GUI) consumes directly — and [`render_styled`] stringifies those
//! spans through a [`Palette`] so a terminal surface can syntax-highlight the tree. The plain
//! [`render_pretty`] is exactly `render_styled(_, &Palette::PLAIN)` (used for `-o pretty`, logs, tests).

use crate::ast::{DraftAst, FlowEffect, Node, Selector, SymbolName, ThingKind, ThingRef, TypeRef};

/// The syntactic role of one rendered span — the presentation-independent form of a [`Palette`]
/// field. [`render_styled_spans`] tags every fragment with its role; a surface maps roles to colors
/// (ANSI open/close pairs here, SVG fills in `flow_render`). [`Role::Text`] is the structural glue
/// (spaces, `=`, parentheses, names) that no palette colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Text,
    Keyword,
    Op,
    Symbol,
    String,
    Lit,
    Effect,
    Connector,
    Thing,
}

/// One colored fragment of a rendered line.
type Span = (String, Role);

/// ANSI `(open, close)` wrappers per syntactic role. [`Palette::PLAIN`] is all-empty (no color); a
/// terminal surface builds a colored one. Rendering wraps each leaf span with its role's pair, so the
/// rendering logic stays presentation-agnostic.
#[derive(Clone, Copy)]
pub struct Palette {
    pub keyword: (&'static str, &'static str),
    pub op: (&'static str, &'static str),
    pub symbol: (&'static str, &'static str),
    pub string: (&'static str, &'static str),
    pub lit: (&'static str, &'static str),
    pub effect: (&'static str, &'static str),
    pub connector: (&'static str, &'static str),
    pub thing: (&'static str, &'static str),
}

impl Palette {
    /// No color — every span passes through unchanged (so styled output == plain output).
    pub const PLAIN: Palette = Palette {
        keyword: ("", ""),
        op: ("", ""),
        symbol: ("", ""),
        string: ("", ""),
        lit: ("", ""),
        effect: ("", ""),
        connector: ("", ""),
        thing: ("", ""),
    };
}

impl Palette {
    /// The `(open, close)` wrapper for `role` — [`Role::Text`] is structural glue, never wrapped.
    fn pair(&self, role: Role) -> (&'static str, &'static str) {
        match role {
            Role::Text => ("", ""),
            Role::Keyword => self.keyword,
            Role::Op => self.op,
            Role::Symbol => self.symbol,
            Role::String => self.string,
            Role::Lit => self.lit,
            Role::Effect => self.effect,
            Role::Connector => self.connector,
            Role::Thing => self.thing,
        }
    }
}

fn paint((open, close): (&str, &str), s: &str) -> String {
    if open.is_empty() && close.is_empty() {
        s.to_string()
    } else {
        format!("{open}{s}{close}")
    }
}

fn plain(s: impl Into<String>) -> Span {
    (s.into(), Role::Text)
}

fn kw(s: &str) -> Span {
    (s.to_string(), Role::Keyword)
}

fn sym(name: &str) -> Span {
    (format!("${name}"), Role::Symbol)
}

/// The `keyword` / `keyword -> $bind` head shared by `pipe`/`seq`/`fallback`/`scope`.
fn bound_kw(word: &str, bind: &Option<SymbolName>) -> Vec<Span> {
    match bind {
        Some(b) => vec![kw(word), plain(" -> "), sym(&b.0)],
        None => vec![kw(word)],
    }
}

/// Stringify one line's spans, wrapping each with its role's palette pair.
fn spans_to_string(spans: &[Span], p: &Palette) -> String {
    let mut out = String::new();
    for (text, role) in spans {
        out.push_str(&paint(p.pair(*role), text));
    }
    out
}

/// Render a flow AST as an indented tree (plain, no color).
pub fn render_pretty(ast: &DraftAst) -> String {
    render_styled(ast, &Palette::PLAIN)
}

/// Render a flow AST as an indented tree, wrapping spans with `p`'s role colors — the ANSI
/// stringification of [`render_styled_spans`].
pub fn render_styled(ast: &DraftAst, p: &Palette) -> String {
    let mut out = String::new();
    for line in render_styled_spans(ast) {
        out.push_str(&spans_to_string(&line, p));
        out.push('\n');
    }
    out
}

/// Render a flow AST as an indented tree of `(text, Role)` spans, one `Vec` per line — the single
/// tree walk both presentations build on: [`render_styled`] wraps each span with an ANSI palette
/// pair; an SVG/GUI surface maps roles to fills. Concatenating a line's fragments yields exactly
/// that line of [`render_pretty`].
pub fn render_styled_spans(ast: &DraftAst) -> Vec<Vec<(String, Role)>> {
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut header = vec![kw("flow")];
    if let Some(name) = &ast.name {
        header.push(plain(format!(" {name}")));
    }
    if !ast.params.is_empty() {
        header.push(plain("  (in: "));
        for (i, pm) in ast.params.iter().enumerate() {
            if i > 0 {
                header.push(plain(", "));
            }
            header.push(sym(&pm.name.0));
            header.push(plain(format!(": {}", type_str(&pm.ty))));
        }
        header.push(plain(")"));
    }
    if let Some(r) = &ast.returns {
        header.push(plain(format!(" -> {}", type_str(r))));
    }
    lines.push(header);

    let branches: Vec<Branch> = ast.body.iter().map(Branch::Node).collect();
    render_branches(&branches, &[], &mut lines);
    lines
}

/// One top-level statement's one-line summary — the same head [`render_styled`] shows for each node
/// in its tree, made available standalone (it does not recurse into children) so a host can prefix it
/// with a ✓ (completed) / ✗ (failed) / · (not yet run) marker when rendering a halted or resumed plan
/// (the resumable-mode feedback contract in `docs/designs/multipass-agent-loop.md` Part 2, wired by
/// A-16/A-17). Pass [`Palette::PLAIN`] for plain text or a colored palette for a terminal surface.
pub fn render_statement(node: &Node, p: &Palette) -> String {
    spans_to_string(&head(node), p)
}

/// A child in the render tree: a real node, the `else` arm of a `when` (whose children are the
/// otherwise-nodes), or a labeled group (a `parallel` branch: the `$name:` header over its body).
enum Branch<'a> {
    Node(&'a Node),
    Else(&'a [Node]),
    Group(&'a str, &'a [Node]),
}

fn render_branches(branches: &[Branch], prefix: &[Span], lines: &mut Vec<Vec<Span>>) {
    let n = branches.len();
    for (i, b) in branches.iter().enumerate() {
        let last = i + 1 == n;
        let connector = if last { "└─ " } else { "├─ " };
        let (head_spans, kids): (Vec<Span>, Vec<Branch>) = match b {
            Branch::Node(node) => (head(node), children(node)),
            Branch::Else(nodes) => (vec![kw("else")], nodes.iter().map(Branch::Node).collect()),
            Branch::Group(name, nodes) => (
                vec![sym(name), plain(":")],
                nodes.iter().map(Branch::Node).collect(),
            ),
        };
        let mut line = prefix.to_vec();
        line.push((connector.to_string(), Role::Connector));
        line.extend(head_spans);
        lines.push(line);
        // The indent run under a branch keeps the connector role even when it is all spaces —
        // exactly what the string form painted, so the ANSI bytes don't move.
        let mut child_prefix = prefix.to_vec();
        child_prefix.push((
            (if last { "   " } else { "│  " }).to_string(),
            Role::Connector,
        ));
        render_branches(&kids, &child_prefix, lines);
    }
}

fn children(node: &Node) -> Vec<Branch<'_>> {
    match node {
        Node::When {
            then, otherwise, ..
        } => {
            let mut v: Vec<Branch> = then.iter().map(Branch::Node).collect();
            if !otherwise.is_empty() {
                v.push(Branch::Else(otherwise));
            }
            v
        }
        Node::Repeat { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Each { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Seq { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Pipe { steps, .. } => steps.iter().map(Branch::Node).collect(),
        Node::Parallel { branches } => branches
            .iter()
            .map(|b| Branch::Group(b.name.0.as_str(), &b.body))
            .collect(),
        Node::Retry { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Try { body, handler, .. } => {
            let mut v: Vec<Branch> = body.iter().map(Branch::Node).collect();
            if !handler.is_empty() {
                v.push(Branch::Group("catch", handler));
            }
            v
        }
        Node::Confirm { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Loop { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Race { branches, .. } => branches
            .iter()
            .map(|b| Branch::Group(b.name.0.as_str(), &b.body))
            .collect(),
        Node::Throttle { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Debounce { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Unless { body, .. } => body.iter().map(Branch::Node).collect(),
        Node::Match { cases, default, .. } => {
            let mut v: Vec<Branch> = cases
                .iter()
                .flat_map(|c| c.body.iter().map(Branch::Node))
                .collect();
            v.extend(default.iter().map(Branch::Node));
            v
        }
        Node::Route { cases, default, .. } => {
            let mut v: Vec<Branch> = cases
                .iter()
                .map(|c| Branch::Group(c.label.as_str(), &c.body))
                .collect();
            if !default.is_empty() {
                v.push(Branch::Else(default));
            }
            v
        }
        Node::Fallback { branches, .. } => branches
            .iter()
            .flat_map(|b| b.body.iter().map(Branch::Node))
            .collect(),
        Node::Timeout { body, .. } | Node::Budget { body, .. } | Node::CapScope { body, .. } => {
            body.iter().map(Branch::Node).collect()
        }
        Node::Scope { body, finally, .. } => {
            let mut v: Vec<Branch> = body.iter().map(Branch::Node).collect();
            if !finally.is_empty() {
                v.push(Branch::Group("finally", finally));
            }
            v
        }
        Node::Saga { steps } => {
            let mut v: Vec<Branch> = Vec::new();
            for step in steps {
                v.extend(step.body.iter().map(Branch::Node));
                if !step.undo.is_empty() {
                    v.push(Branch::Group("undo", &step.undo));
                }
            }
            v
        }
        Node::Once { body, .. } => body.iter().map(Branch::Node).collect(),
        // Leaf / inline-rendered kinds: no tree children — everything they carry is rendered in
        // full by `head`/`expr`. Exhaustive on purpose (no `_` wildcard): a future node kind fails
        // compilation here instead of silently rendering as a headless leaf.
        Node::Bind { .. }
        | Node::Call { .. }
        | Node::Assert { .. }
        | Node::Memo { .. }
        | Node::Await { .. }
        | Node::Return { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::Verify { .. }
        | Node::Peek { .. }
        | Node::Expr { .. }
        | Node::Fmt { .. }
        | Node::Jq { .. }
        | Node::Parse { .. }
        | Node::Ctx { .. }
        | Node::CtxAppend { .. }
        | Node::Checkpoint { .. }
        | Node::Obj { .. }
        | Node::List { .. } => Vec::new(),
    }
}

fn head(node: &Node) -> Vec<Span> {
    match node {
        Node::Bind {
            name,
            value,
            effect,
            ..
        } => {
            let mut v = vec![sym(&name.0), plain(" = ")];
            v.extend(expr(value));
            v.extend(eff(effect));
            v
        }
        Node::Call { .. } => expr(node),
        Node::When { cond, .. } => {
            let mut v = vec![kw("when"), plain(" ")];
            v.extend(expr(cond));
            v
        }
        Node::Repeat { max, until, .. } => match until {
            Some(u) => {
                let mut v = vec![
                    kw("repeat"),
                    plain(format!(" max {max} ")),
                    kw("until"),
                    plain(" "),
                ];
                v.extend(expr(u));
                v
            }
            None => vec![kw("repeat"), plain(format!(" max {max}"))],
        },
        Node::Each {
            source,
            item,
            collect,
            ..
        } => {
            let mut v = vec![
                kw("each"),
                plain(" "),
                sym(&item.0),
                plain(" "),
                kw("in"),
                plain(" "),
            ];
            v.extend(expr(source));
            if let Some(c) = collect {
                v.push(plain(" -> "));
                v.push(sym(&c.0));
            }
            v
        }
        Node::Assert { cond, .. } => {
            let mut v = vec![kw("assert"), plain(" ")];
            v.extend(expr(cond));
            v
        }
        Node::Pipe { bind, .. } => bound_kw("pipe", bind),
        Node::Seq { bind, .. } => bound_kw("seq", bind),
        Node::Memo {
            name,
            value,
            effect,
            ..
        } => {
            let mut v = vec![sym(&name.0), plain(" = "), kw("memo"), plain(" ")];
            v.extend(expr(value));
            v.extend(eff(effect));
            v
        }
        Node::Parallel { .. } => vec![kw("parallel")],
        Node::Retry { max, backoff, .. } => {
            let b = backoff.as_deref().unwrap_or("none");
            vec![kw("retry"), plain(format!(" max {max} backoff={b}"))]
        }
        Node::Try { catch, .. } => match catch {
            Some(c) => vec![kw("try"), plain(format!(" catch ${}", c.0))],
            None => vec![kw("try")],
        },
        Node::Confirm { message, risk, .. } => {
            let r = risk.as_deref().unwrap_or("medium");
            vec![
                kw("confirm"),
                plain(format!(" [{r}] ")),
                (message.clone(), Role::String),
            ]
        }
        Node::Loop {
            for_ms,
            every_ms,
            until,
            ..
        } => {
            let mut v = vec![
                kw("loop"),
                plain(format!(" for {for_ms}ms every {every_ms}ms")),
            ];
            if let Some(u) = until {
                v.push(plain(" until "));
                v.extend(expr(u));
            }
            v
        }
        Node::Await {
            binding, source, ..
        } => match binding {
            Some(b) => vec![
                sym(&b.0),
                plain(" = "),
                kw("await"),
                plain(format!(" {source}")),
            ],
            None => vec![kw("await"), plain(format!(" {source}"))],
        },
        Node::Race {
            timeout_ms, bind, ..
        } => match bind {
            Some(b) => vec![
                kw("race"),
                plain(format!(" timeout={timeout_ms}ms -> ")),
                sym(&b.0),
            ],
            None => vec![kw("race"), plain(format!(" timeout={timeout_ms}ms"))],
        },
        Node::Throttle { max, window_ms, .. } => {
            vec![
                kw("throttle"),
                plain(format!(" max={max} window={window_ms}ms")),
            ]
        }
        Node::Debounce { wait_ms, .. } => {
            vec![kw("debounce"), plain(format!(" wait={wait_ms}ms"))]
        }
        Node::Unless { cond, .. } => {
            let mut v = vec![kw("unless"), plain(" ")];
            v.extend(expr(cond));
            v
        }
        Node::Verify {
            cmd,
            expect,
            message,
        } => {
            let msg = message.as_deref().unwrap_or("");
            let mut v = vec![kw("verify"), plain(" ")];
            v.extend(expr(cmd));
            v.push(plain(" contains "));
            v.extend(expr(expect));
            v.push(plain(" "));
            v.push((msg.to_string(), Role::String));
            v
        }
        Node::Peek { name } => vec![kw("peek"), plain(" "), sym(&name.0)],
        Node::Expr { formula, vars } => {
            let mut v = vec![
                kw("expr"),
                plain(" "),
                (format!("\"{formula}\""), Role::String),
            ];
            if !vars.is_empty() {
                v.push(plain(" ("));
                for (i, (k, val)) in vars.iter().enumerate() {
                    if i > 0 {
                        v.push(plain(", "));
                    }
                    v.push(plain(format!("{k}=")));
                    v.extend(expr(val));
                }
                v.push(plain(")"));
            }
            v
        }
        Node::Fmt { template } => vec![
            kw("fmt"),
            plain(" "),
            (format!("\"{template}\""), Role::String),
        ],
        Node::Jq { path, input, .. } => {
            let mut v = vec![
                kw("jq"),
                plain(" "),
                (format!("\"{path}\""), Role::String),
                plain(" "),
            ];
            v.extend(expr(input));
            v
        }
        Node::Return { value } => {
            let mut v = vec![kw("return"), plain(" ")];
            v.extend(expr(value));
            v
        }
        Node::Var { name } => vec![sym(&name.0)],
        Node::Lit { value } => vec![lit(value)],
        Node::Thing { thing } => vec![thing_span(thing)],
        Node::Parse { value, as_type } => {
            let mut v = vec![kw("parse"), plain(" ")];
            v.extend(expr(value));
            v.push(plain(" "));
            v.push(kw("as"));
            v.push(plain(format!(" {as_type}")));
            v
        }
        Node::Ctx { name, budget, .. } => {
            let mut v = vec![kw("ctx"), plain(" "), sym(&name.0)];
            if let Some(b) = budget {
                v.push(plain(format!(" budget {b}")));
            }
            v
        }
        Node::CtxAppend { ctx, .. } => vec![sym(&ctx.0), plain(" += …")],
        Node::Match { subject, .. } => {
            let mut v = vec![kw("match"), plain(" ")];
            v.extend(expr(subject));
            v
        }
        Node::Route { selector, .. } => {
            let mut v = vec![kw("route"), plain(" ")];
            v.extend(expr(selector));
            v
        }
        Node::Fallback { bind, .. } => bound_kw("fallback", bind),
        Node::Timeout { ms, bind, .. } => match bind {
            Some(b) => vec![kw("timeout"), plain(format!(" {ms}ms -> ")), sym(&b.0)],
            None => vec![kw("timeout"), plain(format!(" {ms}ms"))],
        },
        Node::Budget { limit, bind, .. } => match bind {
            Some(b) => vec![kw("budget"), plain(format!(" {limit} -> ")), sym(&b.0)],
            None => vec![kw("budget"), plain(format!(" {limit}"))],
        },
        Node::CapScope { tools, bind, .. } => {
            let t = format!("[{}]", tools.join(", "));
            match bind {
                Some(b) => vec![kw("with_tools"), plain(format!(" {t} -> ")), sym(&b.0)],
                None => vec![kw("with_tools"), plain(format!(" {t}"))],
            }
        }
        Node::Scope { bind, .. } => bound_kw("scope", bind),
        Node::Saga { steps } => vec![kw("saga"), plain(format!(" ({} steps)", steps.len()))],
        Node::Once { label, bind, .. } => match bind {
            Some(b) => vec![kw("once"), plain(format!(" {label:?} -> ")), sym(&b.0)],
            None => vec![kw("once"), plain(format!(" {label:?}"))],
        },
        Node::Checkpoint { label } => vec![kw("checkpoint"), plain(format!(" {label:?}"))],
        // Value templates render their contents inline — the plan is the artifact you approve,
        // so `{k: $v}` must show what it assembles, not a `(N fields)` count.
        Node::Obj { .. } | Node::List { .. } => expr(node),
    }
}

/// Render a node inline as a one-line expression (for call args, bind values, conditions, …).
fn expr(node: &Node) -> Vec<Span> {
    match node {
        Node::Call { op, args } => {
            let mut v = vec![(op.clone(), Role::Op), plain("(")];
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    v.push(plain(", "));
                }
                v.extend(expr(a));
            }
            v.push(plain(")"));
            v
        }
        Node::Var { name } => vec![sym(&name.0)],
        Node::Lit { value } => vec![lit(value)],
        Node::Thing { thing } => vec![thing_span(thing)],
        Node::Bind { name, .. } => vec![sym(&name.0)],
        Node::Return { value } => {
            let mut v = vec![kw("return"), plain(" ")];
            v.extend(expr(value));
            v
        }
        // Value templates and the pure leaf nodes render inline in full — as call arguments and
        // bind values they ARE the reviewed plan's arguments, so nothing may hide behind `…`.
        Node::Obj { fields } => {
            let mut v = vec![plain("{")];
            for (i, (k, val)) in fields.iter().enumerate() {
                if i > 0 {
                    v.push(plain(", "));
                }
                v.push(plain(format!("{k}: ")));
                v.extend(expr(val));
            }
            v.push(plain("}"));
            v
        }
        Node::List { items } => {
            let mut v = vec![plain("[")];
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    v.push(plain(", "));
                }
                v.extend(expr(it));
            }
            v.push(plain("]"));
            v
        }
        Node::Jq { path, input, .. } => {
            let mut v = vec![
                ("jq".to_string(), Role::Op),
                plain("("),
                (format!("\"{path}\""), Role::String),
                plain(", "),
            ];
            v.extend(expr(input));
            v.push(plain(")"));
            v
        }
        Node::Fmt { template } => vec![
            ("fmt".to_string(), Role::Op),
            plain("("),
            (format!("\"{template}\""), Role::String),
            plain(")"),
        ],
        Node::Expr { formula, vars } => {
            let mut v = vec![
                ("expr".to_string(), Role::Op),
                plain("("),
                (format!("\"{formula}\""), Role::String),
            ];
            for (k, val) in vars {
                v.push(plain(format!(", {k}: ")));
                v.extend(expr(val));
            }
            v.push(plain(")"));
            v
        }
        Node::When { .. }
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
        | Node::Peek { .. }
        | Node::Parse { .. }
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
        | Node::Checkpoint { .. } => vec![plain("…")],
    }
}

/// Render a literal inline, in **full** — the plan is the artifact you review and approve, so its
/// arguments (paths, patterns, `task` prompts, …) must be visible. `serde_json::to_string` escapes
/// newlines, so a long value stays one (terminal-wrapped) line rather than breaking the tree.
fn lit(value: &serde_json::Value) -> Span {
    let s = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    if value.is_string() {
        (s, Role::String)
    } else {
        (s, Role::Lit)
    }
}

fn eff(effect: &Option<FlowEffect>) -> Option<Span> {
    effect
        .as_ref()
        .map(|e| (format!("   !{}", effect_tag(*e)), Role::Effect))
}

/// The stable lowercase tag for a semantic effect. Delegates to [`FlowEffect::tag`], the single
/// source of truth for the tag vocabulary.
fn effect_tag(e: FlowEffect) -> &'static str {
    e.tag()
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
        ThingKind::Custom(c) => c.as_str(),
    };
    let sel = match &thing.selector {
        Selector::Id(s)
        | Selector::Name(s)
        | Selector::Path(s)
        | Selector::Query(s)
        | Selector::Key(s) => s,
    };
    (format!("@{kind}({sel:?})"), Role::Thing)
}

fn type_str(t: &TypeRef) -> String {
    t.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SymbolName;

    #[test]
    fn render_statement_renders_one_marker_ready_line() {
        // Same one-line summary the tree renderer shows for this node, standalone — what a host
        // prefixes with a ✓/✗/· marker (L-22: `docs/designs/multipass-agent-loop.md` Part 2).
        let node = Node::Bind {
            name: SymbolName("readme".into()),
            value: Box::new(Node::Call {
                op: "read".into(),
                args: vec![Node::Lit {
                    value: serde_json::json!("README.md"),
                }],
            }),
            ty: None,
            effect: None,
        };
        let line = render_statement(&node, &Palette::PLAIN);
        assert_eq!(line, "$readme = read(\"README.md\")");
        assert!(!line.contains('\n'), "a marker prefix needs a single line");
    }

    #[test]
    fn renders_a_flow_tree() {
        let ast = DraftAst {
            name: None,
            params: Vec::new(),
            returns: None,
            body: vec![
                Node::Bind {
                    name: SymbolName("readme".into()),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("README.md"),
                        }],
                    }),
                    ty: None,
                    effect: Some(FlowEffect::Read),
                },
                Node::Return {
                    value: Box::new(Node::Var {
                        name: SymbolName("readme".into()),
                    }),
                },
            ],
        };
        let s = render_pretty(&ast);
        assert!(s.starts_with("flow\n"));
        assert!(s.contains("$readme = read(\"README.md\")"));
        assert!(s.contains("!read"));
        assert!(s.contains("└─ return $readme"));
    }

    #[test]
    fn renders_when_else_branches() {
        let ast = DraftAst {
            body: vec![Node::When {
                cond: Box::new(Node::Var {
                    name: SymbolName("ok".into()),
                }),
                then: vec![Node::Return {
                    value: Box::new(Node::Lit {
                        value: serde_json::json!(true),
                    }),
                }],
                otherwise: vec![Node::Return {
                    value: Box::new(Node::Lit {
                        value: serde_json::json!(false),
                    }),
                }],
            }],
            ..Default::default()
        };
        let s = render_pretty(&ast);
        assert!(s.contains("when $ok"));
        assert!(s.contains("else"));
    }

    #[test]
    fn pretty_shows_long_literals_in_full() {
        // The plan is the artifact you review — long literals (e.g. a task prompt) are shown in full,
        // not truncated, so nothing about what will run is hidden.
        let big = "x".repeat(200);
        let ast = DraftAst {
            body: vec![Node::Return {
                value: Box::new(Node::Lit {
                    value: serde_json::json!(big),
                }),
            }],
            ..Default::default()
        };
        let s = render_pretty(&ast);
        assert!(s.contains(&"x".repeat(200)), "full literal must appear");
        assert!(!s.contains('…'), "no truncation marker");
    }

    #[test]
    fn renders_each_and_parallel_trees() {
        use crate::ast::Branch as AstBranch;
        let ast = DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(Node::Lit {
                        value: serde_json::json!(["a", "b"]),
                    }),
                    item: SymbolName("f".into()),
                    body: vec![Node::Call {
                        op: "read".into(),
                        args: vec![Node::Var {
                            name: SymbolName("f".into()),
                        }],
                    }],
                    collect: Some(SymbolName("all".into())),
                    flat: false,
                },
                Node::Parallel {
                    branches: vec![AstBranch {
                        name: SymbolName("left".into()),
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![Node::Lit {
                                value: serde_json::json!("l"),
                            }],
                        }],
                    }],
                },
            ],
            ..Default::default()
        };
        let s = render_pretty(&ast);
        // `each` shows its iteration variable, source, and collect target; its body is a child.
        assert!(s.contains("each $f in [\"a\",\"b\"] -> $all"), "got: {s}");
        assert!(s.contains("read($f)"));
        // `parallel` shows each branch as a labeled `$name:` group with its body underneath.
        assert!(s.contains("parallel"));
        assert!(s.contains("$left:"), "got: {s}");
    }

    #[test]
    fn obj_and_list_template_contents_are_visible() {
        // F26: the plan-approval tree must show what a value template assembles — a `{…}` / `(N
        // fields)` placeholder hides exactly the arguments the reviewer is approving.
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "ok".to_string(),
            Box::new(Node::Lit {
                value: serde_json::json!(true),
            }),
        );
        fields.insert(
            "n".to_string(),
            Box::new(Node::Var {
                name: SymbolName("count".into()),
            }),
        );
        fields.insert(
            "intent".to_string(),
            Box::new(Node::Jq {
                path: ".intent".into(),
                optional: false,
                input: Box::new(Node::Var {
                    name: SymbolName("extract".into()),
                }),
            }),
        );
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: SymbolName("r".into()),
                    value: Box::new(Node::Obj { fields }),
                    ty: None,
                    effect: None,
                },
                Node::Return {
                    value: Box::new(Node::List {
                        items: vec![
                            Node::Var {
                                name: SymbolName("a".into()),
                            },
                            Node::Lit {
                                value: serde_json::json!(3),
                            },
                        ],
                    }),
                },
            ],
            ..Default::default()
        };
        let s = render_pretty(&ast);
        // Obj fields (BTreeMap order) render inline, including the jq leaf's path and input.
        assert!(
            s.contains("$r = {intent: jq(\".intent\", $extract), n: $count, ok: true}"),
            "got: {s}"
        );
        // List items render inline in order.
        assert!(s.contains("return [$a, 3]"), "got: {s}");
        assert!(!s.contains('…'), "no hidden template placeholders: {s}");
    }

    /// A nesting-heavy AST exercising the header (name/params/returns), a bind with an effect,
    /// `when`/`else`, a `parallel` group label, and connectors at three depths — shared by the
    /// styled-snapshot and span-form tests so both pin the same walk.
    fn representative_ast() -> DraftAst {
        use crate::ast::{Branch as AstBranch, Param, TypeRef};
        DraftAst {
            name: Some("triage".into()),
            params: vec![Param {
                name: SymbolName("repo".into()),
                ty: TypeRef::String,
            }],
            returns: Some(TypeRef::String),
            body: vec![
                Node::Bind {
                    name: SymbolName("readme".into()),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("README.md"),
                        }],
                    }),
                    ty: None,
                    effect: Some(FlowEffect::Read),
                },
                Node::When {
                    cond: Box::new(Node::Var {
                        name: SymbolName("ok".into()),
                    }),
                    then: vec![Node::Parallel {
                        branches: vec![AstBranch {
                            name: SymbolName("left".into()),
                            body: vec![Node::Call {
                                op: "notify".into(),
                                args: vec![Node::Lit {
                                    value: serde_json::json!("hi"),
                                }],
                            }],
                        }],
                    }],
                    otherwise: vec![Node::Return {
                        value: Box::new(Node::Var {
                            name: SymbolName("readme".into()),
                        }),
                    }],
                },
                Node::Return {
                    value: Box::new(Node::Var {
                        name: SymbolName("readme".into()),
                    }),
                },
            ],
        }
    }

    #[test]
    fn styled_ansi_output_is_pinned_byte_exact() {
        // Byte-exact snapshot of the styled render (marker palette) — the refactor of
        // `render_styled` onto `render_styled_spans` (L-75) must not move a single byte, including
        // the connector-wrapped indent runs ("│  " / "   ") in child prefixes.
        let pal = Palette {
            keyword: ("K<", ">"),
            op: ("O<", ">"),
            symbol: ("S<", ">"),
            string: ("T<", ">"),
            lit: ("L<", ">"),
            effect: ("E<", ">"),
            connector: ("C<", ">"),
            thing: ("H<", ">"),
        };
        let s = render_styled(&representative_ast(), &pal);
        let expected = "\
K<flow> triage  (in: S<$repo>: String) -> String
C<├─ >S<$readme> = O<read>(T<\"README.md\">)E<   !read>
C<├─ >K<when> S<$ok>
C<│  >C<├─ >K<parallel>
C<│  >C<│  >C<└─ >S<$left>:
C<│  >C<│  >C<   >C<└─ >O<notify>(T<\"hi\">)
C<│  >C<└─ >K<else>
C<│  >C<   >C<└─ >K<return> S<$readme>
C<└─ >K<return> S<$readme>
";
        assert_eq!(s, expected);
    }

    #[test]
    fn spans_join_to_plain_render_and_connectors_carry_role() {
        // L-75: the span form is the same walk as the string form — concatenating each line's
        // fragments reproduces the plain render line-for-line, and every tree-drawing glyph
        // (`├─` / `└─` / `│`, including the indent-only runs) is tagged `Role::Connector` so a
        // non-ANSI surface (SVG, GUI) can color the tree without re-parsing it.
        let ast = representative_ast();
        let lines = render_styled_spans(&ast);
        let joined: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|(text, _)| text.as_str()).collect())
            .collect();
        let pretty = render_pretty(&ast);
        let plain: Vec<&str> = pretty.lines().collect();
        assert_eq!(joined, plain);
        let mut connectors = 0;
        for (text, role) in lines.iter().flatten() {
            if ['├', '└', '│'].iter().any(|g| text.contains(*g)) {
                assert_eq!(
                    *role,
                    Role::Connector,
                    "tree glyph in {text:?} must carry Role::Connector"
                );
                connectors += 1;
            }
        }
        assert!(connectors >= 8, "deep tree renders many connector spans");
    }

    #[test]
    fn styled_plain_equals_pretty_and_palette_wraps_spans() {
        let ast = DraftAst {
            body: vec![Node::Bind {
                name: SymbolName("x".into()),
                value: Box::new(Node::Call {
                    op: "read".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!("f"),
                    }],
                }),
                ty: None,
                effect: Some(FlowEffect::Read),
            }],
            ..Default::default()
        };
        // The PLAIN palette renders byte-for-byte like `render_pretty`.
        assert_eq!(render_styled(&ast, &Palette::PLAIN), render_pretty(&ast));

        // A colored palette wraps each leaf span with its role's codes.
        let pal = Palette {
            op: ("<op>", "</op>"),
            symbol: ("<s>", "</s>"),
            string: ("<str>", "</str>"),
            ..Palette::PLAIN
        };
        let s = render_styled(&ast, &pal);
        assert!(s.contains("<op>read</op>"), "op wrapped: {s}");
        assert!(s.contains("<s>$x</s>"), "symbol wrapped: {s}");
        assert!(s.contains("<str>\"f\"</str>"), "string wrapped: {s}");
    }
}

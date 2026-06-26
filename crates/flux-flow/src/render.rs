//! Pretty-rendering of a [`DraftAst`] as a human-readable execution-path tree — the `pretty` output of
//! `--plan` / `--compile-only`, and the live plan view the engine surfaces before executing.
//!
//! [`render_styled`] takes a [`Palette`] so a terminal surface can syntax-highlight the tree; the plain
//! [`render_pretty`] is exactly `render_styled(_, &Palette::PLAIN)` (used for `-o pretty`, logs, tests).

use crate::ast::{DraftAst, FlowEffect, Node, Selector, ThingKind, ThingRef, TypeRef};

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

fn paint((open, close): (&str, &str), s: &str) -> String {
    if open.is_empty() && close.is_empty() {
        s.to_string()
    } else {
        format!("{open}{s}{close}")
    }
}

fn sym(p: &Palette, name: &str) -> String {
    paint(p.symbol, &format!("${name}"))
}

/// Render a flow AST as an indented tree (plain, no color).
pub fn render_pretty(ast: &DraftAst) -> String {
    render_styled(ast, &Palette::PLAIN)
}

/// Render a flow AST as an indented tree, wrapping spans with `p`'s role colors.
pub fn render_styled(ast: &DraftAst, p: &Palette) -> String {
    let mut out = String::new();
    out.push_str(&paint(p.keyword, "flow"));
    if let Some(name) = &ast.name {
        out.push(' ');
        out.push_str(name);
    }
    if !ast.params.is_empty() {
        let ps: Vec<String> = ast
            .params
            .iter()
            .map(|pm| format!("{}: {}", sym(p, &pm.name.0), type_str(&pm.ty)))
            .collect();
        out.push_str(&format!("  (in: {})", ps.join(", ")));
    }
    if let Some(r) = &ast.returns {
        out.push_str(&format!(" -> {}", type_str(r)));
    }
    out.push('\n');

    let branches: Vec<Branch> = ast.body.iter().map(Branch::Node).collect();
    render_branches(&branches, "", p, &mut out);
    out
}

/// A child in the render tree: a real node, the `else` arm of a `when` (whose children are the
/// otherwise-nodes), or a labeled group (a `parallel` branch: the `$name:` header over its body).
enum Branch<'a> {
    Node(&'a Node),
    Else(&'a [Node]),
    Group(&'a str, &'a [Node]),
}

fn render_branches(branches: &[Branch], prefix: &str, p: &Palette, out: &mut String) {
    let n = branches.len();
    for (i, b) in branches.iter().enumerate() {
        let last = i + 1 == n;
        let connector = if last { "└─ " } else { "├─ " };
        let (head_str, kids): (String, Vec<Branch>) = match b {
            Branch::Node(node) => (head(node, p), children(node)),
            Branch::Else(nodes) => (
                paint(p.keyword, "else"),
                nodes.iter().map(Branch::Node).collect(),
            ),
            Branch::Group(name, nodes) => (
                format!("{}:", sym(p, name)),
                nodes.iter().map(Branch::Node).collect(),
            ),
        };
        out.push_str(prefix);
        out.push_str(&paint(p.connector, connector));
        out.push_str(&head_str);
        out.push('\n');
        let child_prefix = format!(
            "{prefix}{}",
            paint(p.connector, if last { "   " } else { "│  " })
        );
        render_branches(&kids, &child_prefix, p, out);
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
        Node::Verify { .. } => Vec::new(),
        Node::Peek { .. } => Vec::new(),
        Node::Expr { .. } | Node::Fmt { .. } | Node::Jq { .. } => Vec::new(),
        _ => Vec::new(),
    }
}

fn head(node: &Node, p: &Palette) -> String {
    match node {
        Node::Bind {
            name,
            value,
            effect,
            ..
        } => format!("{} = {}{}", sym(p, &name.0), expr(value, p), eff(effect, p)),
        Node::Call { .. } => expr(node, p),
        Node::When { cond, .. } => format!("{} {}", paint(p.keyword, "when"), expr(cond, p)),
        Node::Repeat { max, until, .. } => match until {
            Some(u) => format!(
                "{} max {max} {} {}",
                paint(p.keyword, "repeat"),
                paint(p.keyword, "until"),
                expr(u, p)
            ),
            None => format!("{} max {max}", paint(p.keyword, "repeat")),
        },
        Node::Each {
            source,
            item,
            collect,
            ..
        } => {
            let base = format!(
                "{} {} {} {}",
                paint(p.keyword, "each"),
                sym(p, &item.0),
                paint(p.keyword, "in"),
                expr(source, p)
            );
            match collect {
                Some(c) => format!("{base} -> {}", sym(p, &c.0)),
                None => base,
            }
        }
        Node::Assert { cond, .. } => format!("{} {}", paint(p.keyword, "assert"), expr(cond, p)),
        Node::Pipe { bind, .. } => match bind {
            Some(b) => format!("{} -> {}", paint(p.keyword, "pipe"), sym(p, &b.0)),
            None => paint(p.keyword, "pipe"),
        },
        Node::Seq { bind, .. } => match bind {
            Some(b) => format!("{} -> {}", paint(p.keyword, "seq"), sym(p, &b.0)),
            None => paint(p.keyword, "seq"),
        },
        Node::Memo {
            name,
            value,
            effect,
            ..
        } => format!(
            "{} = {} {}{}",
            sym(p, &name.0),
            paint(p.keyword, "memo"),
            expr(value, p),
            eff(effect, p)
        ),
        Node::Parallel { .. } => paint(p.keyword, "parallel"),
        Node::Retry { max, backoff, .. } => {
            let b = backoff.as_deref().unwrap_or("none");
            format!("{} max {max} backoff={b}", paint(p.keyword, "retry"))
        }
        Node::Try { catch, .. } => match catch {
            Some(c) => format!("{} catch ${}", paint(p.keyword, "try"), c.0),
            None => paint(p.keyword, "try"),
        },
        Node::Confirm { message, risk, .. } => {
            let r = risk.as_deref().unwrap_or("medium");
            format!("{} [{r}] {}", paint(p.keyword, "confirm"), paint(p.string, message))
        }
        Node::Loop { for_ms, every_ms, until, .. } => {
            let base = format!("{} for {for_ms}ms every {every_ms}ms", paint(p.keyword, "loop"));
            match until {
                Some(u) => format!("{base} until {}", expr(u, p)),
                None => base,
            }
        }
        Node::Await {
            binding, source, ..
        } => match binding {
            Some(b) => format!("{} = {} {source}", sym(p, &b.0), paint(p.keyword, "await")),
            None => format!("{} {source}", paint(p.keyword, "await")),
        },
        Node::Race { timeout_ms, bind, .. } => match bind {
            Some(b) => format!("{} timeout={timeout_ms}ms -> {}", paint(p.keyword, "race"), sym(p, &b.0)),
            None => format!("{} timeout={timeout_ms}ms", paint(p.keyword, "race")),
        },
        Node::Throttle { max, window_ms, .. } => {
            format!("{} max={max} window={window_ms}ms", paint(p.keyword, "throttle"))
        }
        Node::Debounce { wait_ms, .. } => {
            format!("{} wait={wait_ms}ms", paint(p.keyword, "debounce"))
        }
        Node::Unless { cond, .. } => format!("{} {}", paint(p.keyword, "unless"), expr(cond, p)),
        Node::Verify { cmd, expect, message } => {
            let msg = message.as_deref().unwrap_or("");
            format!("{} {} contains {} {}", paint(p.keyword, "verify"), expr(cmd, p), expr(expect, p), paint(p.string, msg))
        }
        Node::Peek { name } => format!("{} {}", paint(p.keyword, "peek"), sym(p, &name.0)),
        Node::Expr { formula, vars } => {
            if vars.is_empty() {
                format!("{} {}", paint(p.keyword, "expr"), paint(p.string, &format!("\"{formula}\"")))
            } else {
                let vs: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={}", expr(v, p))).collect();
                format!("{} {} ({})", paint(p.keyword, "expr"), paint(p.string, &format!("\"{formula}\"")), vs.join(", "))
            }
        }
        Node::Fmt { template } => format!("{} {}", paint(p.keyword, "fmt"), paint(p.string, &format!("\"{template}\""))),
        Node::Jq { path, input } => format!("{} {} {}", paint(p.keyword, "jq"), paint(p.string, &format!("\"{path}\"")), expr(input, p)),
        Node::Return { value } => format!("{} {}", paint(p.keyword, "return"), expr(value, p)),
        Node::Var { name } => sym(p, &name.0),
        Node::Lit { value } => lit(value, p),
        Node::Thing { thing } => thing_str(thing, p),
    }
}

/// Render a node inline as a one-line expression (for call args, bind values, conditions, …).
fn expr(node: &Node, p: &Palette) -> String {
    match node {
        Node::Call { op, args } => {
            let a: Vec<String> = args.iter().map(|x| expr(x, p)).collect();
            format!("{}({})", paint(p.op, op), a.join(", "))
        }
        Node::Var { name } => sym(p, &name.0),
        Node::Lit { value } => lit(value, p),
        Node::Thing { thing } => thing_str(thing, p),
        Node::Bind { name, .. } => sym(p, &name.0),
        Node::Return { value } => format!("{} {}", paint(p.keyword, "return"), expr(value, p)),
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
        | Node::Expr { .. }
        | Node::Fmt { .. }
        | Node::Jq { .. } => "…".to_string(),
    }
}

/// Render a literal inline, in **full** — the plan is the artifact you review and approve, so its
/// arguments (paths, patterns, `task` prompts, …) must be visible. `serde_json::to_string` escapes
/// newlines, so a long value stays one (terminal-wrapped) line rather than breaking the tree.
fn lit(value: &serde_json::Value, p: &Palette) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    if value.is_string() {
        paint(p.string, &s)
    } else {
        paint(p.lit, &s)
    }
}

fn eff(effect: &Option<FlowEffect>, p: &Palette) -> String {
    match effect {
        Some(e) => paint(p.effect, &format!("   !{}", effect_tag(*e))),
        None => String::new(),
    }
}

fn effect_tag(e: FlowEffect) -> &'static str {
    match e {
        FlowEffect::Pure => "pure",
        FlowEffect::Read => "read",
        FlowEffect::Model => "model",
        FlowEffect::Network => "network",
        FlowEffect::WriteFile => "write_file",
        FlowEffect::WriteDb => "write_db",
        FlowEffect::SendExternal => "send_external",
        FlowEffect::Delete => "delete",
        FlowEffect::Money => "money",
        FlowEffect::Calendar => "calendar",
        FlowEffect::HumanVisible => "human_visible",
    }
}

fn thing_str(thing: &ThingRef, p: &Palette) -> String {
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
    paint(p.thing, &format!("@{kind}({sel:?})"))
}

fn type_str(t: &TypeRef) -> String {
    t.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SymbolName;

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

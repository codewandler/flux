//! Pretty-rendering of a [`DraftAst`] as a human-readable execution-path tree — the `pretty` output
//! of `--compile-only`, and the basis for live CLI/TUI graph rendering during execution (M3).

use crate::ast::{DraftAst, FlowEffect, Node, Selector, ThingKind, ThingRef, TypeRef};

/// Render a flow AST as an indented tree showing its execution path.
pub fn render_pretty(ast: &DraftAst) -> String {
    let mut out = String::new();
    out.push_str("flow");
    if let Some(name) = &ast.name {
        out.push(' ');
        out.push_str(name);
    }
    if !ast.params.is_empty() {
        let ps: Vec<String> = ast
            .params
            .iter()
            .map(|p| format!("${}: {}", p.name.0, type_str(&p.ty)))
            .collect();
        out.push_str(&format!("  (in: {})", ps.join(", ")));
    }
    if let Some(r) = &ast.returns {
        out.push_str(&format!(" -> {}", type_str(r)));
    }
    out.push('\n');

    let branches: Vec<Branch> = ast.body.iter().map(Branch::Node).collect();
    render_branches(&branches, "", &mut out);
    out
}

/// A child in the render tree: a real node, or the `else` arm of a `when` (whose children are the
/// otherwise-nodes).
enum Branch<'a> {
    Node(&'a Node),
    Else(&'a [Node]),
}

fn render_branches(branches: &[Branch], prefix: &str, out: &mut String) {
    let n = branches.len();
    for (i, b) in branches.iter().enumerate() {
        let last = i + 1 == n;
        let connector = if last { "└─ " } else { "├─ " };
        let (head_str, kids): (String, Vec<Branch>) = match b {
            Branch::Node(node) => (head(node), children(node)),
            Branch::Else(nodes) => ("else".to_string(), nodes.iter().map(Branch::Node).collect()),
        };
        out.push_str(prefix);
        out.push_str(connector);
        out.push_str(&head_str);
        out.push('\n');
        let child_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
        render_branches(&kids, &child_prefix, out);
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
        _ => Vec::new(),
    }
}

fn head(node: &Node) -> String {
    match node {
        Node::Bind {
            name,
            value,
            effect,
            ..
        } => format!("${} = {}{}", name.0, expr(value), eff(effect)),
        Node::Call { .. } => expr(node),
        Node::When { cond, .. } => format!("when {}", expr(cond)),
        Node::Repeat { max, until, .. } => match until {
            Some(u) => format!("repeat max {max} until {}", expr(u)),
            None => format!("repeat max {max}"),
        },
        Node::Await {
            binding, source, ..
        } => match binding {
            Some(b) => format!("${} = await {source}", b.0),
            None => format!("await {source}"),
        },
        Node::Return { value } => format!("return {}", expr(value)),
        Node::Var { name } => format!("${}", name.0),
        Node::Lit { value } => lit(value),
        Node::Thing { thing } => thing_str(thing),
    }
}

/// Render a node inline as a one-line expression (for call args, bind values, conditions, …).
fn expr(node: &Node) -> String {
    match node {
        Node::Call { op, args } => {
            let a: Vec<String> = args.iter().map(expr).collect();
            format!("{op}({})", a.join(", "))
        }
        Node::Var { name } => format!("${}", name.0),
        Node::Lit { value } => lit(value),
        Node::Thing { thing } => thing_str(thing),
        Node::Bind { name, .. } => format!("${}", name.0),
        Node::Return { value } => format!("return {}", expr(value)),
        Node::When { .. } | Node::Repeat { .. } | Node::Await { .. } => "…".to_string(),
    }
}

/// Render a literal inline, truncating long values so the pretty tree stays readable. (The `json`/
/// `yaml` outputs serialize the full AST; only this human view truncates.)
fn lit(value: &serde_json::Value) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    if s.chars().count() > 60 {
        let head: String = s.chars().take(57).collect();
        format!("{head}…")
    } else {
        s
    }
}

fn eff(effect: &Option<FlowEffect>) -> String {
    match effect {
        Some(e) => format!("   !{}", effect_tag(*e)),
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

fn thing_str(thing: &ThingRef) -> String {
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
    format!("@{kind}({sel:?})")
}

fn type_str(t: &TypeRef) -> String {
    match t {
        TypeRef::Any => "Any".to_string(),
        TypeRef::Bool => "Bool".to_string(),
        TypeRef::Number => "Number".to_string(),
        TypeRef::String => "String".to_string(),
        TypeRef::List(inner) => format!("List<{}>", type_str(inner)),
        TypeRef::Named(n) => n.clone(),
    }
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
    fn pretty_truncates_long_literals() {
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
        assert!(s.contains('…'), "long literal should be truncated");
        assert!(
            !s.contains(&"x".repeat(200)),
            "full literal must not appear"
        );
    }
}

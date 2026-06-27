//! `format` — the **canonical compact text projection** of a [`DraftAst`], and the round-trip partner
//! of [`crate::parse`]. Where [`crate::render`] is a one-way box-drawing *display* tree, this module
//! emits a re-parseable surface: `parse(&format(&ast)) == ast` for every `DraftAst`.
//!
//! # Markers (design §5)
//! - `$x = <expr>` — a **bind** (`=`).
//! - `do <op> <arg>, …` — an **effectful call** discarding its result (`do`).
//! - `$pack += $a, $b` — a **context append** (`+=`).
//!
//! # Supported node kinds (native text form)
//! `bind`, `call`, `var`, `lit`, `return`, `when`/`unless`, `each`, `repeat`, `seq`, `ctx` and
//! `ctx_append`. Blocks are **2-space indentation** delimited (no braces, no `end`).
//!
//! # The `@json` fallback
//! Any node *not* in the supported set (and any supported node appearing in an inline position the
//! surface cannot spell) is emitted as a single-line `@json <compact-json>` escape that [`crate::parse`]
//! reads back via serde. This keeps the round-trip total for **every** `DraftAst` while keeping the
//! hand-written surface small. The `@json` JSON is exactly the wire format, so it never loses data.
//!
//! # Note on `goal`
//! `DraftAst` has no `goal` field, so `format` never emits a `goal` line. The parser *tolerates and
//! ignores* one for forward-compatibility with hand-written headers; it is not part of the round-trip.

use crate::ast::{DraftAst, FlowEffect, Node, SymbolName};

/// Render a [`DraftAst`] as canonical Flux-Lang text. Always 2-space indentation; deterministic.
pub fn format(ast: &DraftAst) -> String {
    let mut out = String::new();
    out.push_str("flow");
    if let Some(name) = &ast.name {
        out.push(' ');
        out.push_str(name);
    }
    if !ast.params.is_empty() {
        out.push('(');
        let ps: Vec<String> = ast
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name.0, p.ty.label()))
            .collect();
        out.push_str(&ps.join(", "));
        out.push(')');
    }
    if let Some(r) = &ast.returns {
        out.push_str(" -> ");
        out.push_str(&r.label());
    }
    out.push('\n');

    fmt_body(&ast.body, 1, &mut out);
    out
}

/// Compact (no-whitespace) JSON for any serializable value. Total: never panics.
fn compact<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// Render a node *inline* (as a bind value, call argument, condition, or return value). Only `var`,
/// `lit` and `call` have a native inline form; everything else falls back to `@json`.
fn fmt_expr(node: &Node) -> String {
    match node {
        Node::Var { name } => format!("${}", name.0),
        Node::Lit { value } => compact(value),
        Node::Call { op, args } => {
            let a: Vec<String> = args.iter().map(fmt_expr).collect();
            format!("{}({})", op, a.join(", "))
        }
        other => format!("@json {}", compact(other)),
    }
}

fn indent_of(level: usize) -> String {
    "  ".repeat(level)
}

fn fmt_body(body: &[Node], level: usize, out: &mut String) {
    for n in body {
        fmt_stmt(n, level, out);
    }
}

fn join_syms(syms: &[SymbolName]) -> String {
    syms.iter()
        .map(|s| format!("${}", s.0))
        .collect::<Vec<_>>()
        .join(", ")
}

fn fmt_stmt(node: &Node, level: usize, out: &mut String) {
    let ind = indent_of(level);
    match node {
        Node::Bind {
            name,
            value,
            ty,
            effect,
        } => {
            if let Some(e) = effect {
                out.push_str(&ind);
                out.push_str("@effect(");
                out.push_str(effect_tag(*e));
                out.push_str(")\n");
            }
            out.push_str(&ind);
            out.push('$');
            out.push_str(&name.0);
            if let Some(t) = ty {
                out.push_str(": ");
                out.push_str(&t.label());
            }
            out.push_str(" = ");
            out.push_str(&fmt_expr(value));
            out.push('\n');
        }
        Node::CtxAppend { ctx, add } => {
            out.push_str(&ind);
            out.push('$');
            out.push_str(&ctx.0);
            out.push_str(" +=");
            if !add.is_empty() {
                out.push(' ');
                out.push_str(&join_syms(add));
            }
            out.push('\n');
        }
        // A bare `call` statement (result discarded) uses the `do` marker.
        Node::Call { op, args } => {
            out.push_str(&ind);
            out.push_str("do ");
            out.push_str(op);
            if !args.is_empty() {
                out.push(' ');
                let a: Vec<String> = args.iter().map(fmt_expr).collect();
                out.push_str(&a.join(", "));
            }
            out.push('\n');
        }
        Node::Return { value } => {
            out.push_str(&ind);
            out.push_str("return ");
            out.push_str(&fmt_expr(value));
            out.push('\n');
        }
        Node::Var { name } => {
            out.push_str(&ind);
            out.push('$');
            out.push_str(&name.0);
            out.push('\n');
        }
        Node::Lit { value } => {
            out.push_str(&ind);
            out.push_str(&compact(value));
            out.push('\n');
        }
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            out.push_str(&ind);
            out.push_str("when ");
            out.push_str(&fmt_expr(cond));
            out.push('\n');
            fmt_body(then, level + 1, out);
            if !otherwise.is_empty() {
                out.push_str(&ind);
                out.push_str("else\n");
                fmt_body(otherwise, level + 1, out);
            }
        }
        Node::Unless { cond, body } => {
            out.push_str(&ind);
            out.push_str("unless ");
            out.push_str(&fmt_expr(cond));
            out.push('\n');
            fmt_body(body, level + 1, out);
        }
        Node::Each {
            source,
            item,
            body,
            collect,
            flat,
        } => {
            out.push_str(&ind);
            out.push_str("each $");
            out.push_str(&item.0);
            out.push_str(" in ");
            out.push_str(&fmt_expr(source));
            if let Some(c) = collect {
                out.push_str(" -> ");
                if *flat {
                    out.push_str("flat ");
                }
                out.push('$');
                out.push_str(&c.0);
            }
            out.push('\n');
            fmt_body(body, level + 1, out);
        }
        Node::Repeat {
            max,
            until,
            body,
            collect,
        } => {
            out.push_str(&ind);
            out.push_str("repeat ");
            out.push_str(&max.to_string());
            if let Some(c) = collect {
                out.push_str(" -> $");
                out.push_str(&c.0);
            }
            out.push('\n');
            if let Some(u) = until {
                out.push_str(&indent_of(level + 1));
                out.push_str("until ");
                out.push_str(&fmt_expr(u));
                out.push('\n');
            }
            fmt_body(body, level + 1, out);
        }
        Node::Seq { body, bind } => {
            out.push_str(&ind);
            out.push_str("seq");
            if let Some(b) = bind {
                out.push_str(" -> $");
                out.push_str(&b.0);
            }
            out.push('\n');
            fmt_body(body, level + 1, out);
        }
        Node::Ctx {
            name,
            purpose,
            include,
            exclude,
            budget,
        } => {
            out.push_str(&ind);
            out.push_str("ctx $");
            out.push_str(&name.0);
            out.push('\n');
            let ind1 = indent_of(level + 1);
            if let Some(p) = purpose {
                out.push_str(&ind1);
                out.push_str("purpose ");
                out.push_str(&compact(&serde_json::Value::String(p.clone())));
                out.push('\n');
            }
            if let Some(b) = budget {
                out.push_str(&ind1);
                out.push_str("budget ");
                out.push_str(&b.to_string());
                out.push('\n');
            }
            if !include.is_empty() {
                out.push_str(&ind1);
                out.push_str("include ");
                out.push_str(&join_syms(include));
                out.push('\n');
            }
            if !exclude.is_empty() {
                out.push_str(&ind1);
                out.push_str("exclude ");
                out.push_str(&join_syms(exclude));
                out.push('\n');
            }
        }
        // Every other node kind round-trips through the single-line `@json` escape.
        other => {
            out.push_str(&ind);
            out.push_str("@json ");
            out.push_str(&compact(other));
            out.push('\n');
        }
    }
}

/// The stable lowercase tag for a semantic effect (matches the serde `snake_case` wire tag).
pub(crate) fn effect_tag(e: FlowEffect) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Param, Selector, ThingKind, ThingRef, TypeRef};

    #[test]
    fn formats_header_and_bind() {
        let ast = DraftAst {
            name: Some("greet".into()),
            params: vec![Param {
                name: "who".into(),
                ty: TypeRef::String,
            }],
            returns: Some(TypeRef::Named("Reply".into())),
            body: vec![Node::Bind {
                name: "msg".into(),
                value: Box::new(Node::Call {
                    op: "greet_op".into(),
                    args: vec![Node::Var { name: "who".into() }],
                }),
                ty: None,
                effect: None,
            }],
        };
        let txt = format(&ast);
        assert!(txt.starts_with("flow greet(who: String) -> Reply\n"));
        assert!(txt.contains("\n  $msg = greet_op($who)\n"), "got: {txt}");
    }

    #[test]
    fn bare_call_uses_do_and_inline_call_uses_parens() {
        let ast = DraftAst {
            body: vec![
                Node::Call {
                    op: "git_stage".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!(["."]),
                    }],
                },
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("f"),
                        }],
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let txt = format(&ast);
        assert!(txt.contains("  do git_stage [\".\"]\n"), "got: {txt}");
        assert!(txt.contains("  $x = read(\"f\")\n"), "got: {txt}");
    }

    #[test]
    fn unsupported_node_uses_json_fallback() {
        let ast = DraftAst {
            body: vec![Node::Thing {
                thing: ThingRef {
                    kind: ThingKind::Person,
                    selector: Selector::Name("john".into()),
                },
            }],
            ..Default::default()
        };
        let txt = format(&ast);
        assert!(txt.contains("  @json {"), "got: {txt}");
        assert!(txt.contains("\"kind\":\"thing\""), "got: {txt}");
    }
}

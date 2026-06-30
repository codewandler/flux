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
//! `bind`, `call`, `var`, `lit`, `return`, `when`/`unless`, `each`, `repeat`, `seq`, `ctx`,
//! `ctx_append`, `match`, `route`, `fallback`, `loop`, `timeout`, `budget`, `fmt` (inline `fmt("…")`),
//! and `jq` field-access sugar (inline `$var.path`, simple dotted paths only). Blocks are **2-space
//! indentation** delimited (no braces, no `end`).
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
use crate::program::CompositeOpDecl;
use flux_spec::{Effect, Idempotency, Risk};

/// Render a [`DraftAst`] as canonical Flux-Lang text. Always 2-space indentation; deterministic.
/// Round-trips: `parse(&format(&ast)) == ast`.
pub fn format(ast: &DraftAst) -> String {
    format_with(ast, "  ")
}

/// Render one top-level composite op declaration as canonical Flux-Lang source.
pub fn format_composite_op(op: &CompositeOpDecl) -> String {
    let mut out = String::new();
    out.push_str("op ");
    out.push_str(&op.name);
    if !op.params.is_empty() {
        out.push('(');
        let ps: Vec<String> = op
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name.0, p.ty.label()))
            .collect();
        out.push_str(&ps.join(", "));
        out.push(')');
    }
    if let Some(r) = &op.returns {
        out.push_str(" -> ");
        out.push_str(&r.label());
    }
    out.push('\n');

    if !op.meta.description.is_empty() {
        out.push_str("  description ");
        out.push_str(&compact(&serde_json::Value::String(
            op.meta.description.clone(),
        )));
        out.push('\n');
    }
    out.push_str("  risk ");
    out.push_str(&compact(&serde_json::Value::String(
        risk_label(op.meta.risk).to_string(),
    )));
    out.push('\n');
    out.push_str("  idempotency ");
    out.push_str(&compact(&serde_json::Value::String(
        idempotency_label(op.meta.idempotency).to_string(),
    )));
    out.push('\n');
    if !op.meta.effects.is_empty() {
        out.push_str("  effects ");
        let effects: Vec<_> = op
            .meta
            .effects
            .iter()
            .map(|e| serde_json::Value::String(effect_label(*e).to_string()))
            .collect();
        out.push_str(&compact(&serde_json::Value::Array(effects)));
        out.push('\n');
    }
    if op.meta.limits.dispatches.is_some()
        || op.meta.limits.timeout_ms.is_some()
        || op.meta.limits.context_chars.is_some()
    {
        out.push_str("  limits ");
        let mut limits = serde_json::Map::new();
        if let Some(n) = op.meta.limits.dispatches {
            limits.insert("dispatches".into(), serde_json::json!(n));
        }
        if let Some(n) = op.meta.limits.timeout_ms {
            limits.insert("timeout_ms".into(), serde_json::json!(n));
        }
        if let Some(n) = op.meta.limits.context_chars {
            limits.insert("context_chars".into(), serde_json::json!(n));
        }
        out.push_str(&compact(&serde_json::Value::Object(limits)));
        out.push('\n');
    }
    out.push_str("  expose ");
    out.push_str(if op.meta.expose { "true" } else { "false" });
    out.push('\n');
    if let Some(view) = &op.meta.view {
        out.push_str("  view ");
        out.push_str(&compact(&serde_json::Value::String(view.clone())));
        out.push('\n');
    }
    if !op.body.body.is_empty() {
        out.push('\n');
        fmt_body(&op.body.body, 1, "  ", &mut out);
    }
    out
}

/// Render a [`DraftAst`] in the **token-efficient** display variant: the same markers as [`format`]
/// but single-space block indentation (≈half the indentation characters on nested plans). For cheap
/// model-facing *display* of a plan. **Display-only:** [`crate::parse`] expects canonical two-space
/// indentation, so this surface does not round-trip — use [`format`] for the writable, parseable form.
pub fn format_compact(ast: &DraftAst) -> String {
    format_with(ast, " ")
}

fn risk_label(risk: Risk) -> &'static str {
    match risk {
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "high",
        Risk::Destructive => "destructive",
    }
}

fn idempotency_label(idempotency: Idempotency) -> &'static str {
    match idempotency {
        Idempotency::Idempotent => "idempotent",
        Idempotency::NonIdempotent => "non_idempotent",
        Idempotency::Conditional => "conditional",
    }
}

fn effect_label(effect: Effect) -> &'static str {
    match effect {
        Effect::Read => "read",
        Effect::Write => "write",
        Effect::Network => "network",
        Effect::Process => "process",
        Effect::Browser => "browser",
        Effect::Filesystem => "filesystem",
        Effect::LocalSystem => "local_system",
    }
}

/// Shared renderer: `indent` is the per-level indentation unit (`"  "` canonical, `" "` compact).
fn format_with(ast: &DraftAst, indent: &str) -> String {
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

    fmt_body(&ast.body, 1, indent, &mut out);
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
        // `fmt("template")` — the string-interpolation node.
        Node::Fmt { template } => {
            format!(
                "fmt({})",
                compact(&serde_json::Value::String(template.clone()))
            )
        }
        // Field-access sugar: a `jq` over a plain `$var` with a simple dotted path renders as
        // `$var.path` (parse re-derives the same `Jq`). Bracket paths or non-`Var` inputs can't be
        // spelled this way, so they fall through to `@json` — keeping the round-trip total.
        Node::Jq { path, input }
            if is_field_path(path) && matches!(input.as_ref(), Node::Var { .. }) =>
        {
            match input.as_ref() {
                Node::Var { name } => format!("${}{}", name.0, path),
                _ => unreachable!("guard checked Var"),
            }
        }
        // Value templates render natively only when they carry a dynamic (non-`Lit`) leaf — `{ ok:
        // true, n: $count }` / `[ $a, 1 ]`. An all-literal or empty template has no native spelling
        // and falls through to `@json` below, so its text never collides with a `Lit`'s `{…}`/`[…]`.
        Node::Obj { fields } if node_is_dynamic(node) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{}: {}", fmt_obj_key(k), fmt_expr(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Node::List { items } if node_is_dynamic(node) => {
            let parts: Vec<String> = items.iter().map(fmt_expr).collect();
            format!("[ {} ]", parts.join(", "))
        }
        other => format!("@json {}", compact(other)),
    }
}

/// Whether a node's subtree carries any dynamic (non-`Lit`) leaf. A value-template (`Obj`/`List`)
/// renders natively only when this holds; an all-literal/empty template falls through to `@json`, so
/// its text can never collide with a `Lit`'s `{…}`/`[…]` JSON rendering (the round-trip disjointness
/// rule — the partner of the parser's try-JSON-then-template split).
fn node_is_dynamic(node: &Node) -> bool {
    match node {
        Node::Lit { .. } => false,
        Node::Obj { fields } => fields.values().any(|v| node_is_dynamic(v)),
        Node::List { items } => items.iter().any(node_is_dynamic),
        _ => true,
    }
}

/// Whether a string is a single bare word (no whitespace) — used to decide whether `retry`'s free-form
/// `backoff` field can be spelled natively (else the whole node falls through to `@json`).
fn is_word_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Whether an object-template key is identifier-safe (emit as a bareword); otherwise it is JSON-quoted.
fn is_ident_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Render an object-template key: a bareword when identifier-safe, else a JSON-quoted string. The
/// parser accepts both forms and recovers the same key string, so the choice is lossless.
fn fmt_obj_key(k: &str) -> String {
    if is_ident_key(k) {
        k.to_string()
    } else {
        compact(&serde_json::Value::String(k.to_string()))
    }
}

/// Whether a `jq` path is a simple dotted field path (`.kind`, `.a.b`) — the only shape the `$var.path`
/// surface can spell. Excludes array indices (`[0]`) and the empty path, which use `@json`.
fn is_field_path(path: &str) -> bool {
    path.starts_with('.')
        && path.len() > 1
        && path[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn indent_of(level: usize, unit: &str) -> String {
    unit.repeat(level)
}

fn fmt_body(body: &[Node], level: usize, indent: &str, out: &mut String) {
    for n in body {
        fmt_stmt(n, level, indent, out);
    }
}

fn join_syms(syms: &[SymbolName]) -> String {
    syms.iter()
        .map(|s| format!("${}", s.0))
        .collect::<Vec<_>>()
        .join(", ")
}

fn fmt_stmt(node: &Node, level: usize, indent: &str, out: &mut String) {
    let ind = indent_of(level, indent);
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
            fmt_body(then, level + 1, indent, out);
            if !otherwise.is_empty() {
                out.push_str(&ind);
                out.push_str("else\n");
                fmt_body(otherwise, level + 1, indent, out);
            }
        }
        Node::Unless { cond, body } => {
            out.push_str(&ind);
            out.push_str("unless ");
            out.push_str(&fmt_expr(cond));
            out.push('\n');
            fmt_body(body, level + 1, indent, out);
        }
        Node::Each {
            source,
            item,
            body,
            collect,
            flat,
        } if !(*flat && collect.is_none()) => {
            // The native `each … -> flat $c` surface can only spell `flat` next to a `collect` target.
            // A `flat: true, collect: None` node (degenerate, but a valid AST shape) has no native form,
            // so this guard lets it fall through to the `@json` escape below — preserving round-trip.
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
            fmt_body(body, level + 1, indent, out);
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
                out.push_str(&indent_of(level + 1, indent));
                out.push_str("until ");
                out.push_str(&fmt_expr(u));
                out.push('\n');
            }
            fmt_body(body, level + 1, indent, out);
        }
        Node::Seq { body, bind } => {
            out.push_str(&ind);
            out.push_str("seq");
            if let Some(b) = bind {
                out.push_str(" -> $");
                out.push_str(&b.0);
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, out);
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
            let ind1 = indent_of(level + 1, indent);
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
        Node::Match {
            subject,
            cases,
            default,
        } => {
            out.push_str(&ind);
            out.push_str("match ");
            out.push_str(&fmt_expr(subject));
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            for c in cases {
                out.push_str(&ind1);
                out.push_str("case ");
                out.push_str(&fmt_expr(&c.value));
                out.push('\n');
                fmt_body(&c.body, level + 2, indent, out);
            }
            if !default.is_empty() {
                out.push_str(&ind1);
                out.push_str("default\n");
                fmt_body(default, level + 2, indent, out);
            }
        }
        Node::Route {
            selector,
            cases,
            default,
        } => {
            out.push_str(&ind);
            out.push_str("route ");
            out.push_str(&fmt_expr(selector));
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            for c in cases {
                out.push_str(&ind1);
                out.push_str("case ");
                out.push_str(&compact(&serde_json::Value::String(c.label.clone())));
                out.push('\n');
                fmt_body(&c.body, level + 2, indent, out);
            }
            if !default.is_empty() {
                out.push_str(&ind1);
                out.push_str("default\n");
                fmt_body(default, level + 2, indent, out);
            }
        }
        Node::Fallback { branches, bind } => {
            out.push_str(&ind);
            out.push_str("fallback");
            if let Some(b) = bind {
                out.push_str(" -> $");
                out.push_str(&b.0);
            }
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            for br in branches {
                out.push_str(&ind1);
                out.push_str("branch\n");
                fmt_body(&br.body, level + 2, indent, out);
            }
        }
        Node::Loop {
            for_ms,
            every_ms,
            until,
            body,
            bind,
        } => {
            out.push_str(&ind);
            out.push_str("loop for ");
            out.push_str(&for_ms.to_string());
            out.push_str(" every ");
            out.push_str(&every_ms.to_string());
            if let Some(b) = bind {
                out.push_str(" -> $");
                out.push_str(&b.0);
            }
            out.push('\n');
            if let Some(u) = until {
                out.push_str(&indent_of(level + 1, indent));
                out.push_str("until ");
                out.push_str(&fmt_expr(u));
                out.push('\n');
            }
            fmt_body(body, level + 1, indent, out);
        }
        Node::Timeout { ms, body, bind } => {
            out.push_str(&ind);
            out.push_str("timeout ");
            out.push_str(&ms.to_string());
            if let Some(b) = bind {
                out.push_str(" -> $");
                out.push_str(&b.0);
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, out);
        }
        Node::Budget { limit, body, bind } => {
            out.push_str(&ind);
            out.push_str("budget ");
            out.push_str(&limit.to_string());
            if let Some(b) = bind {
                out.push_str(" -> $");
                out.push_str(&b.0);
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, out);
        }
        // `parallel` + indented `branch $name` arms. Native only when every branch name is an
        // identifier (so the `$name` reader recovers it exactly); otherwise fall through to `@json`.
        Node::Parallel { branches } if branches.iter().all(|b| is_ident_key(&b.name.0)) => {
            out.push_str(&ind);
            out.push_str("parallel\n");
            for br in branches {
                out.push_str(&indent_of(level + 1, indent));
                out.push_str("branch $");
                out.push_str(&br.name.0);
                out.push('\n');
                fmt_body(&br.body, level + 2, indent, out);
            }
        }
        // `retry <max> [backoff <ident>] [delay <ms>] [-> $bind]` + body. Native only when `backoff`
        // is a single bare word (the field is free-form `String`); otherwise fall through to `@json`.
        Node::Retry {
            max,
            backoff,
            delay_ms,
            body,
            bind,
        } if backoff.as_deref().is_none_or(is_word_token) => {
            out.push_str(&ind);
            out.push_str("retry ");
            out.push_str(&max.to_string());
            if let Some(b) = backoff {
                out.push_str(" backoff ");
                out.push_str(b);
            }
            if let Some(ms) = delay_ms {
                out.push_str(" delay ");
                out.push_str(&ms.to_string());
            }
            if let Some(s) = bind {
                out.push_str(" -> $");
                out.push_str(&s.0);
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, out);
        }
        // `assert <cond> [, "<message>"]` — a one-line guard; the condition delegates to `fmt_expr`
        // (with its own `@json` fallback), so `assert` itself always renders natively.
        Node::Assert { cond, message } => {
            out.push_str(&ind);
            out.push_str("assert ");
            out.push_str(&fmt_expr(cond));
            if let Some(m) = message {
                out.push_str(", ");
                out.push_str(&compact(&serde_json::Value::String(m.clone())));
            }
            out.push('\n');
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
    fn compact_uses_single_space_indent_display_only() {
        // A nested plan: `when $x` with a body, so indentation is exercised.
        let ast = DraftAst {
            body: vec![Node::When {
                cond: Box::new(Node::Var { name: "x".into() }),
                then: vec![Node::Call {
                    op: "echo".into(),
                    args: vec![Node::Lit {
                        value: serde_json::json!("hi"),
                    }],
                }],
                otherwise: vec![],
            }],
            ..Default::default()
        };
        let canonical = format(&ast);
        let compact = format_compact(&ast);
        // Level-1 `when` is indented two spaces canonically, one space compactly — fewer chars.
        assert!(
            canonical.contains("\n  when $x\n"),
            "canonical: {canonical}"
        );
        assert!(compact.contains("\n when $x\n"), "compact: {compact}");
        assert!(
            compact.len() < canonical.len(),
            "compact is shorter ({} vs {})",
            compact.len(),
            canonical.len()
        );
        // The canonical form remains the round-trippable one (compact is display-only).
        assert_eq!(crate::parse::parse(&canonical).unwrap(), ast);
    }

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

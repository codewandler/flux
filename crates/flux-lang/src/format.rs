//! `format` — the **canonical compact text projection** of a [`DraftAst`], and the round-trip partner
//! of [`crate::parse`]. Where [`crate::render`] is a one-way box-drawing *display* tree, this module
//! emits a re-parseable surface: `parse(&format(&ast)) == ast` for every `DraftAst` whose header
//! names are spellable (see "The flow-header exception" below — node bodies are covered totally).
//!
//! # Canonical source
//! - `x = <expr>` — a **bind** (`=`); `$x` remains the reserved-name/legacy escape.
//! - `op(<args>)` — one call form whether the result is bound or discarded. `do` remains accepted
//!   and is emitted only to escape a callable name that collides with statement syntax.
//! - `pack += a, b` — a **context append** (`+=`).
//!
//! # Supported node kinds (native text form)
//! `bind`, `call`, `var`, `lit`, `return`, `when`/`unless`, `each`, `repeat`, `seq`, `ctx`,
//! `ctx_append`, `match`, `route`, `fallback`, `loop`, `timeout`, `budget`, `fmt` (inline `fmt("…")`),
//! and `jq` field/index-access sugar (inline `var.path[0]`). Blocks are **2-space
//! indentation** delimited (no braces, no `end`).
//!
//! # The `@json` fallback
//! Any node *not* in the supported set (and any supported node appearing in an inline position the
//! surface cannot spell) is emitted as a single-line `@json <compact-json>` escape that [`crate::parse`]
//! reads back via serde. This keeps the round-trip total for **every** `DraftAst` while keeping the
//! hand-written surface small. The `@json` JSON is exactly the wire format, so it never loses data.
//!
//! # Name guards (round-trip totality)
//! Every *declared* name position — bind/memo targets, `each` items and collectors, `repeat`
//! collectors, every `-> $bind` arrow target, `ctx` names and include/exclude symbols, `ctx_append`
//! symbols, `parallel` branch names, and `$var` references (statement and expression position) — is
//! spelled natively only when the name is a plain identifier ([`SymbolName::is_identifier`]).
//! Op names are spelled natively only when [`is_valid_op_name`] holds (and, inline, when the op is
//! not literally `fmt`, whose paren form would reparse as the [`Node::Fmt`] node). A `Bind`'s type
//! annotation is spelled only when its label reparses to the same [`TypeRef`] (a `Named("Bool")`
//! would silently come back as the builtin). Any violation makes the **whole node** fall back to
//! `@json`, mirroring the `retry`-backoff and degenerate-`each` guards, so
//! `parse(&format(&ast)) == ast` stays total for node bodies.
//!
//! # The flow-header exception
//! The header (`flow <name>(<params>) -> <type>`) has **no** `@json` escape: an unspellable flow
//! name, parameter name, or header type cannot fall back to anything without changing the AST, and
//! [`crate::parse`] has no whole-flow escape line. `format` emits header names verbatim; an
//! unspellable one produces a **loud** parse error on the way back (never silent corruption). The
//! analyzer rejects such names ([`crate::ast::is_valid_decl_name`]), so they cannot reach a
//! formatted artifact through the normal pipeline; the round-trip property test excludes header
//! names for exactly this documented reason.
//!
//! # Note on `goal`
//! `DraftAst` has no `goal` field, so `format` never emits a `goal` line. The parser *tolerates and
//! ignores* one for forward-compatibility with hand-written headers; it is not part of the round-trip.

use crate::ast::{
    is_bare_symbol_name, is_reserved_word, is_valid_decl_name, is_valid_op_name, DraftAst,
    FlowEffect, Node, SymbolName, TypeRef,
};
use crate::program::CompositeOpDecl;
use flux_spec::{Effect, Idempotency, Risk};

/// Render a [`DraftAst`] as canonical Flux-Lang text. Always 2-space indentation; deterministic.
/// Round-trips: `parse(&format(&ast)) == ast`. Emits the multi-line `"""…"""` spelling (L-39) for
/// any string literal containing a newline.
pub fn format(ast: &DraftAst) -> String {
    format_with(ast, "  ", true)
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
        fmt_body(&op.body.body, 1, "  ", true, &mut out);
    }
    out
}

/// Render a [`DraftAst`] in the **token-efficient** source variant: the same grammar as [`format`]
/// but single-space block indentation (≈half the indentation characters on nested plans). This is
/// writable Flux-Lang and satisfies the same exact parse/format round-trip contract.
///
/// Never emits the multi-line `"""…"""` spelling (L-39): a newline-bearing string stays the
/// standard escaped single-line form, so a compact plan preview stays visually one line per
/// statement (the property this variant exists for).
pub fn format_compact(ast: &DraftAst) -> String {
    format_with(ast, " ", false)
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

/// Shared renderer: `indent` is the per-level indentation unit (`"  "` canonical, `" "` compact);
/// `multiline` enables the `"""…"""` string spelling (L-39) — on for [`format`], off for
/// [`format_compact`].
fn format_with(ast: &DraftAst, indent: &str, multiline: bool) -> String {
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

    fmt_body(&ast.body, 1, indent, multiline, &mut out);
    out
}

/// Compact (no-whitespace) JSON for any serializable value. Total: never panics. Uses the standard
/// escaped-string spelling only — see [`compact_value`]/[`compact_str`] for the multiline-aware
/// variants used everywhere a string literal can appear in the text surface.
fn compact<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// Whether `s` can be safely spelled as `"""<s>"""` (L-39). Three hazards, each of which would
/// break `parse(&format(&ast)) == ast`:
/// - The closing delimiter is discovered by scanning forward for the *next* literal `"""` (see
///   the CST lexer's triple-string token), so content that itself contains `"""` cannot round-trip.
/// - Content **ending** in a `"` would merge with the closer into an accidental run of 3+ quotes
///   and get swallowed one character short.
/// - CST semantic lowering normalizes `\r\n` -> `\n` inside triple-string content, so a `\r`
///   anywhere in the content — most commonly as part of a `\r\n` pair — would silently lose it on
///   the way back through `parse`.
///
/// All three are vanishingly rare in real payloads; the formatter falls back to the always-safe
/// escaped form for them, so the round-trip invariant stays total.
fn is_safe_for_multiline_spelling(s: &str) -> bool {
    !s.contains("\"\"\"") && !s.ends_with('"') && !s.contains('\r')
}

/// Render a string literal as canonical text: the verbatim `"""…"""` spelling when `multiline` is
/// enabled, the string contains a newline, and it is safe to spell that way (see
/// [`is_safe_for_multiline_spelling`]); the standard escaped JSON string otherwise. Deterministic —
/// the string's own content is the only input to the choice, never configuration.
pub(crate) fn compact_str(s: &str, multiline: bool) -> String {
    if multiline && s.contains('\n') && is_safe_for_multiline_spelling(s) {
        format!("\"\"\"{s}\"\"\"")
    } else {
        compact(&serde_json::Value::String(s.to_string()))
    }
}

/// Render any JSON value as canonical text, recursing into arrays/objects so a string **leaf** at
/// any nesting depth (an argument object's field, a list item, …) can use the multi-line spelling —
/// this is what makes a `Lit` node cover "argument objects" and "bare args" per the L-39 Acceptance.
/// Object *keys* always use the standard spelling (never `"""…"""`) — multi-line keys are not a
/// case this story targets, and keeping keys plain keeps `{"key": …` visually recognizable.
fn compact_value(v: &serde_json::Value, multiline: bool) -> String {
    match v {
        serde_json::Value::String(s) => compact_str(s, multiline),
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(|i| compact_value(i, multiline)).collect();
            format!("[{}]", parts.join(","))
        }
        serde_json::Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, val)| {
                    format!(
                        "{}:{}",
                        compact_str(k, false),
                        compact_value(val, multiline)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => compact(other),
    }
}

/// Render a list of plain strings as a compact JSON array (`["a", "b"]`) — the native `with_tools`
/// header's tool-name list, using the same escaping as [`compact`] so a name with a quote or
/// backslash still round-trips through `parse_setting_list`/`as_string_list`.
fn fmt_string_list(items: &[String]) -> String {
    compact(&items)
}

/// Render a node *inline* (as a bind value, call argument, condition, or return value). Only `var`,
/// `lit` and `call` have a native inline form; everything else falls back to `@json`. Unspellable
/// names (a non-identifier `$var`, an invalid op name, an op literally named `fmt` whose paren form
/// would reparse as the `Fmt` node) also fall back to `@json`. `multiline` enables the `"""…"""`
/// string spelling (L-39) inside any `Lit`/`Fmt`/template string leaf.
pub(crate) fn fmt_expr(node: &Node, multiline: bool) -> String {
    match node {
        Node::Var { name } if name.is_identifier() => fmt_symbol(name),
        Node::Lit { value } => compact_value(value, multiline),
        Node::Call { op, args }
            if is_valid_op_name(op) && op != "fmt" && fmt_call_args(args, multiline).is_some() =>
        {
            format!(
                "{}({})",
                op,
                fmt_call_args(args, multiline).unwrap_or_default()
            )
        }
        // `fmt("template")` — the string-interpolation node.
        Node::Fmt { template } => {
            format!("fmt({})", compact_str(template, multiline))
        }
        // Field-access sugar: a `jq` over a plain `$var` with a simple dotted path renders as
        // `$var.path` (parse re-derives the same `Jq`). Bracket paths, non-`Var` inputs, or a
        // non-identifier input symbol can't be spelled this way, so they fall through to `@json` —
        // keeping the round-trip total.
        Node::Jq {
            path,
            input,
            optional,
        } if fmt_field_path(path).is_some()
            && matches!(input.as_ref(), Node::Var { name } if name.is_identifier()) =>
        {
            // L-53: a lenient access renders with a trailing `?` (`$var.path?`); strict is bare.
            let q = if *optional { "?" } else { "" };
            match input.as_ref() {
                Node::Var { name } => format!(
                    "{}{}{}",
                    fmt_symbol(name),
                    fmt_field_path(path).unwrap_or_default(),
                    q
                ),
                _ => unreachable!("guard checked Var"),
            }
        }
        // Value templates render natively only when they carry a dynamic (non-`Lit`) leaf — `{ ok:
        // true, n: $count }` / `[ $a, 1 ]`. An all-literal or empty template has no native spelling
        // and falls through to `@json` below, so its text never collides with a `Lit`'s `{…}`/`[…]`.
        Node::Obj { fields } if node_is_dynamic(node) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| {
                    if is_bare_symbol_name(k)
                        && matches!(v.as_ref(), Node::Var { name } if name.0 == *k)
                    {
                        k.clone()
                    } else {
                        format!("{}: {}", fmt_obj_key(k), fmt_expr(v, multiline))
                    }
                })
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        Node::List { items } if node_is_dynamic(node) => {
            let parts: Vec<String> = items.iter().map(|n| fmt_expr(n, multiline)).collect();
            format!("[ {} ]", parts.join(", "))
        }
        // Native `Expr` rendering when invertible: every var maps to {name: Var(name)}.
        // The formula has variable names without `$`, so we re-insert `$` before each var name.
        Node::Expr { formula, vars } if is_invertible_expr(vars) => {
            // Re-insert $ before each variable name in the formula
            let mut result = formula.clone();
            // Sort variable names by length (longest first) to avoid partial replacements
            let mut var_names: Vec<&String> = vars.keys().collect();
            var_names.sort_by_key(|b| std::cmp::Reverse(b.len()));

            for var_name in var_names {
                // Replace whole-word occurrences of the variable name with $name
                // Use a simple approach: replace identifiers that match exactly
                result = replace_ident(&result, var_name, &format!("${}", var_name));
            }
            result
        }
        // `peek $sym` reads a symbol's current value.
        Node::Peek { name } if name.is_identifier() => format!("peek {}", fmt_symbol(name)),
        // `parse(<value>, as: "T")` — coercion. The value goes through `parse_condition_expr` on the
        // way back, so a native-expr value is fine; only an @json-rendered value forces the escape.
        Node::Parse { value, as_type } if !fmt_expr(value, multiline).starts_with("@json ") => {
            format!(
                "parse({}, as: {})",
                fmt_expr(value, multiline),
                compact_str(as_type, multiline)
            )
        }
        // `thing <kind> <selector> "<value>"` — an external reference.
        Node::Thing { thing } => {
            let (sel_word, val) = selector_word_value(&thing.selector);
            format!(
                "thing {} {} {}",
                thing_kind_word(&thing.kind, multiline),
                sel_word,
                compact_str(val, multiline)
            )
        }
        other => format!("@json {}", compact(other)),
    }
}

/// Whether an `Expr` node's vars map is invertible — every entry is exactly `{name: Var(name)}`.
/// When true, the formatter can render the expr natively by re-inserting `$` before variable names.
fn is_invertible_expr(vars: &std::collections::BTreeMap<String, Box<Node>>) -> bool {
    vars.iter().all(
        |(name, node)| matches!(node.as_ref(), Node::Var { name: var_name } if var_name.0 == *name),
    )
}

/// Replace whole-word occurrences of `ident` with `replacement` in `text`.
/// Only replaces when `ident` appears as a complete identifier (not part of a larger identifier).
fn replace_ident(text: &str, ident: &str, replacement: &str) -> String {
    let mut result = String::new();
    let mut i = 0;
    let text_chars: Vec<char> = text.chars().collect();

    while i < text_chars.len() {
        // Check if we're at the start of the identifier
        if i + ident.len() <= text_chars.len() {
            let slice: String = text_chars[i..i + ident.len()].iter().collect();
            if slice == ident {
                // Check that it's a whole identifier (not part of a larger one)
                let before_ok = i == 0 || !is_ident_continue(text_chars[i - 1]);
                let after_ok = i + ident.len() >= text_chars.len()
                    || !is_ident_continue(text_chars[i + ident.len()]);

                if before_ok && after_ok {
                    result.push_str(replacement);
                    i += ident.len();
                    continue;
                }
            }
        }
        result.push(text_chars[i]);
        i += 1;
    }
    result
}

/// Whether a character can continue an identifier (alphanumeric, underscore, or dot for field access).
fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
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
pub(crate) fn is_word_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Whether an optional declared symbol is spellable (absent counts as spellable) — the guard for
/// every optional `-> $bind` / `collect` slot.
fn opt_ident(sym: &Option<SymbolName>) -> bool {
    sym.as_ref().is_none_or(SymbolName::is_identifier)
}

/// Whether every symbol in a `$a, $b` list is spellable.
fn all_idents(syms: &[SymbolName]) -> bool {
    syms.iter().all(SymbolName::is_identifier)
}

/// Whether a type annotation can be spelled in the text surface and re-read as the *same*
/// [`TypeRef`]. A `Named` label that collides with a builtin (`Any`/`Bool`/`Number`/`String`) or
/// leaves the decl-name charset (spaces, `=`, `<`, `#`, …) would reparse as something else — the
/// node carrying it falls back to `@json` instead.
pub(crate) fn is_spellable_type(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Any | TypeRef::Bool | TypeRef::Number | TypeRef::String => true,
        TypeRef::List(inner) => is_spellable_type(inner),
        TypeRef::Named(n) => {
            is_valid_decl_name(n) && !matches!(n.as_str(), "Any" | "Bool" | "Number" | "String")
        }
    }
}

/// Whether an optional type annotation is spellable (absent counts as spellable).
fn opt_spellable_type(ty: &Option<TypeRef>) -> bool {
    ty.as_ref().is_none_or(is_spellable_type)
}

/// Whether an object-template key is identifier-safe (emit as a bareword); otherwise it is JSON-quoted.
fn is_ident_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Render an object-template key: a bareword when identifier-safe, else a JSON-quoted string. The
/// parser accepts both forms and recovers the same key string, so the choice is lossless.
pub(crate) fn fmt_obj_key(k: &str) -> String {
    if is_ident_key(k) {
        k.to_string()
    } else {
        compact(&serde_json::Value::String(k.to_string()))
    }
}

/// Render a spellable symbol without a sigil. Contextual keywords retain `$` as an explicit escape,
/// so the canonical surface stays unambiguous while ordinary locals shed their repeated marker.
pub(crate) fn fmt_symbol(symbol: &SymbolName) -> String {
    if is_bare_symbol_name(&symbol.0) {
        symbol.0.clone()
    } else {
        format!("${}", symbol.0)
    }
}

/// Render call arguments, projecting the existing single-object named-input convention directly as
/// `name: value` entries. Returns `None` only when an object key cannot be represented as a label.
fn fmt_call_args(args: &[Node], multiline: bool) -> Option<String> {
    if let [Node::Obj { fields }] = args {
        if !fields.keys().all(|key| is_ident_key(key)) {
            return None;
        }
        let all_puns = fields.len() > 1
            && fields.iter().all(|(key, value)| {
                is_bare_symbol_name(key)
                    && matches!(value.as_ref(), Node::Var { name } if name.0 == *key)
            });
        let rendered = fields
            .iter()
            .map(|(key, value)| {
                if all_puns
                    || (fields.len() > 1
                        && is_bare_symbol_name(key)
                        && matches!(value.as_ref(), Node::Var { name } if name.0 == *key))
                {
                    key.clone()
                } else {
                    format!("{key}: {}", fmt_expr(value, multiline))
                }
            })
            .collect::<Vec<_>>();
        return Some(rendered.join(", "));
    }

    let multiple = args.len() > 1;
    Some(
        args.iter()
            .map(|arg| match arg {
                // Two or more bare variable arguments are the named-input pun surface. Preserve an
                // explicitly positional legacy call by retaining `$` on those otherwise-ambiguous
                // entries.
                Node::Var { name } if multiple && name.is_identifier() => {
                    format!("${}", name.0)
                }
                _ => fmt_expr(arg, multiline),
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn fmt_legacy_call_args(args: &[Node], multiline: bool) -> Option<String> {
    let rendered = args
        .iter()
        .map(|arg| match arg {
            Node::Var { name } if name.is_identifier() => format!("${}", name.0),
            _ => fmt_expr(arg, multiline),
        })
        .collect::<Vec<_>>();
    rendered
        .iter()
        .all(|arg| !arg.starts_with("@json "))
        .then(|| rendered.join(", "))
}

/// Convert the AST's jq-compatible dotted path (`.items.0.name`) to readable field/index syntax
/// (`.items[0].name`). Unsupported jq syntax falls back to the JSON node escape.
pub(crate) fn fmt_field_path(path: &str) -> Option<String> {
    crate::jq_path::native_field_path(path)
}

pub(crate) fn fmt_duration(ms: u64) -> String {
    if ms != 0 && ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms != 0 && ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

fn indent_of(level: usize, unit: &str) -> String {
    unit.repeat(level)
}

fn fmt_body(body: &[Node], level: usize, indent: &str, multiline: bool, out: &mut String) {
    for n in body {
        fmt_stmt(n, level, indent, multiline, out);
    }
}

fn join_syms(syms: &[SymbolName]) -> String {
    syms.iter().map(fmt_symbol).collect::<Vec<_>>().join(", ")
}

/// True if `n` renders as an inline expression that `parse_expr` (not the native-expr or @json
/// paths) can re-read — used to decide whether a node whose operand is an expression (`verify`) can
/// be spelled natively rather than escaped. `Expr` needs `parse_condition_expr`, so it is excluded.
fn call_like_operand(n: &Node, multiline: bool) -> bool {
    !matches!(n, Node::Expr { .. }) && !fmt_expr(n, multiline).starts_with("@json ")
}

/// The native word for a [`crate::ast::ThingKind`] (a `Custom` kind renders `custom "<name>"`).
fn thing_kind_word(k: &crate::ast::ThingKind, multiline: bool) -> String {
    use crate::ast::ThingKind::*;
    match k {
        Context => "context".into(),
        File => "file".into(),
        Person => "person".into(),
        Ticket => "ticket".into(),
        Email => "email".into(),
        Repo => "repo".into(),
        Dataset => "dataset".into(),
        CalendarEvent => "calendar_event".into(),
        Url => "url".into(),
        Secret => "secret".into(),
        Custom(s) => format!("custom {}", compact_str(s, multiline)),
    }
}

/// The native word and inner value of a [`crate::ast::Selector`].
fn selector_word_value(s: &crate::ast::Selector) -> (&'static str, &String) {
    use crate::ast::Selector::*;
    match s {
        Id(v) => ("id", v),
        Name(v) => ("name", v),
        Path(v) => ("path", v),
        Query(v) => ("query", v),
        Key(v) => ("key", v),
    }
}

pub(crate) fn fmt_stmt(node: &Node, level: usize, indent: &str, multiline: bool, out: &mut String) {
    let ind = indent_of(level, indent);
    match node {
        Node::Bind {
            name,
            value,
            ty,
            effect,
        } if name.is_identifier() && opt_spellable_type(ty) => {
            if let Some(e) = effect {
                out.push_str(&ind);
                out.push_str("@effect(");
                out.push_str(effect_tag(*e));
                out.push_str(")\n");
            }
            out.push_str(&ind);
            out.push_str(&fmt_symbol(name));
            if let Some(t) = ty {
                out.push_str(": ");
                out.push_str(&t.label());
            }
            out.push_str(" = ");
            out.push_str(&fmt_expr(value, multiline));
            out.push('\n');
        }
        Node::CtxAppend { ctx, add } if ctx.is_identifier() && all_idents(add) => {
            out.push_str(&ind);
            out.push_str(&fmt_symbol(ctx));
            out.push_str(" +=");
            if !add.is_empty() {
                out.push(' ');
                out.push_str(&join_syms(add));
            }
            out.push('\n');
        }
        // Calls use one expression form whether their result is bound or discarded.
        Node::Call { op, args }
            if is_valid_op_name(op)
                && if op == "fmt" || is_reserved_word(op) {
                    fmt_legacy_call_args(args, multiline).is_some()
                } else {
                    fmt_call_args(args, multiline).is_some()
                } =>
        {
            out.push_str(&ind);
            if op == "fmt" || is_reserved_word(op) {
                out.push_str("do ");
                out.push_str(op);
                if !args.is_empty() {
                    out.push(' ');
                    out.push_str(&fmt_legacy_call_args(args, multiline).unwrap_or_default());
                }
            } else {
                out.push_str(op);
                out.push('(');
                out.push_str(&fmt_call_args(args, multiline).unwrap_or_default());
                out.push(')');
            }
            out.push('\n');
        }
        Node::Return { value } => {
            out.push_str(&ind);
            out.push_str("return ");
            out.push_str(&fmt_expr(value, multiline));
            out.push('\n');
        }
        Node::Var { name } if name.is_identifier() => {
            out.push_str(&ind);
            out.push_str(&fmt_symbol(name));
            out.push('\n');
        }
        Node::Lit { value } => {
            out.push_str(&ind);
            out.push_str(&compact_value(value, multiline));
            out.push('\n');
        }
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            out.push_str(&ind);
            out.push_str("when ");
            out.push_str(&fmt_expr(cond, multiline));
            out.push('\n');
            fmt_body(then, level + 1, indent, multiline, out);
            if !otherwise.is_empty() {
                out.push_str(&ind);
                out.push_str("else\n");
                fmt_body(otherwise, level + 1, indent, multiline, out);
            }
        }
        Node::Unless { cond, body } => {
            out.push_str(&ind);
            out.push_str("unless ");
            out.push_str(&fmt_expr(cond, multiline));
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Each {
            source,
            item,
            body,
            collect,
            flat,
        } if !(*flat && collect.is_none()) && item.is_identifier() && opt_ident(collect) => {
            // The native `each … -> flat $c` surface can only spell `flat` next to a `collect` target.
            // A `flat: true, collect: None` node (degenerate, but a valid AST shape) has no native form,
            // so this guard lets it fall through to the `@json` escape below — preserving round-trip.
            // Unspellable item/collect names fall through the same way (the F21 name guards).
            out.push_str(&ind);
            out.push_str("each ");
            out.push_str(&fmt_symbol(item));
            out.push_str(" in ");
            out.push_str(&fmt_expr(source, multiline));
            if let Some(c) = collect {
                out.push_str(" -> ");
                if *flat {
                    out.push_str("flat ");
                }
                out.push_str(&fmt_symbol(c));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Repeat {
            max,
            until,
            body,
            collect,
        } if opt_ident(collect) => {
            out.push_str(&ind);
            out.push_str("repeat ");
            out.push_str(&max.to_string());
            if let Some(u) = until {
                out.push_str(", until: ");
                out.push_str(&fmt_expr(u, multiline));
            }
            if let Some(c) = collect {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(c));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Seq { body, bind } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("seq");
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Ctx {
            name,
            purpose,
            include,
            exclude,
            budget,
        } if name.is_identifier() && all_idents(include) && all_idents(exclude) => {
            out.push_str(&ind);
            out.push_str("ctx ");
            out.push_str(&fmt_symbol(name));
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            if let Some(p) = purpose {
                out.push_str(&ind1);
                out.push_str("purpose ");
                out.push_str(&compact_str(p, multiline));
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
            out.push_str(&fmt_expr(subject, multiline));
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            for c in cases {
                out.push_str(&ind1);
                out.push_str("case ");
                out.push_str(&fmt_expr(&c.value, multiline));
                out.push('\n');
                fmt_body(&c.body, level + 2, indent, multiline, out);
            }
            if !default.is_empty() {
                out.push_str(&ind1);
                out.push_str("default\n");
                fmt_body(default, level + 2, indent, multiline, out);
            }
        }
        Node::Route {
            selector,
            cases,
            default,
        } => {
            out.push_str(&ind);
            out.push_str("route ");
            out.push_str(&fmt_expr(selector, multiline));
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            for c in cases {
                out.push_str(&ind1);
                out.push_str("case ");
                out.push_str(&compact_str(&c.label, multiline));
                out.push('\n');
                fmt_body(&c.body, level + 2, indent, multiline, out);
            }
            if !default.is_empty() {
                out.push_str(&ind1);
                out.push_str("default\n");
                fmt_body(default, level + 2, indent, multiline, out);
            }
        }
        Node::Fallback { branches, bind } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("fallback");
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            for br in branches {
                out.push_str(&ind1);
                out.push_str("branch\n");
                fmt_body(&br.body, level + 2, indent, multiline, out);
            }
        }
        Node::Loop {
            for_ms,
            every_ms,
            until,
            body,
            bind,
        } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("loop for ");
            out.push_str(&fmt_duration(*for_ms));
            out.push_str(", every: ");
            out.push_str(&fmt_duration(*every_ms));
            if let Some(u) = until {
                out.push_str(", until: ");
                out.push_str(&fmt_expr(u, multiline));
            }
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Timeout { ms, body, bind } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("timeout ");
            out.push_str(&fmt_duration(*ms));
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Budget { limit, body, bind } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("budget ");
            out.push_str(&limit.to_string());
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::CapScope { tools, body, bind } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("with_tools ");
            out.push_str(&fmt_string_list(tools));
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        // `parallel` + indented `branch $name` arms. Native only when every branch name is an
        // identifier (so the `$name` reader recovers it exactly); otherwise fall through to `@json`.
        Node::Parallel { branches } if branches.iter().all(|b| b.name.is_identifier()) => {
            out.push_str(&ind);
            out.push_str("parallel\n");
            for br in branches {
                out.push_str(&indent_of(level + 1, indent));
                out.push_str("branch ");
                out.push_str(&fmt_symbol(&br.name));
                out.push('\n');
                fmt_body(&br.body, level + 2, indent, multiline, out);
            }
        }
        // `retry <max> [backoff <ident>] [delay <ms>] [-> $bind]` + body. Native only when `backoff`
        // is a single bare word (the field is free-form `String`) and the bind is spellable;
        // otherwise fall through to `@json`.
        Node::Retry {
            max,
            backoff,
            delay_ms,
            body,
            bind,
        } if backoff.as_deref().is_none_or(is_word_token) && opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("retry ");
            out.push_str(&max.to_string());
            if let Some(b) = backoff {
                out.push_str(", backoff: ");
                out.push_str(b);
            }
            if let Some(ms) = delay_ms {
                out.push_str(", delay: ");
                out.push_str(&fmt_duration(*ms));
            }
            if let Some(s) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(s));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        // `assert <cond> [, "<message>"]` — a one-line guard; the condition delegates to `fmt_expr`
        // (with its own `@json` fallback), so `assert` itself always renders natively.
        Node::Assert { cond, message } => {
            out.push_str(&ind);
            out.push_str("assert ");
            out.push_str(&fmt_expr(cond, multiline));
            if let Some(m) = message {
                out.push_str(", ");
                out.push_str(&compact_str(m, multiline));
            }
            out.push('\n');
        }
        Node::Try {
            body,
            catch,
            handler,
        } if opt_ident(catch) => {
            out.push_str(&ind);
            out.push_str("try\n");
            fmt_body(body, level + 1, indent, multiline, out);
            out.push_str(&ind);
            out.push_str("catch");
            if let Some(c) = catch {
                out.push(' ');
                out.push_str(&fmt_symbol(c));
            }
            out.push('\n');
            fmt_body(handler, level + 1, indent, multiline, out);
        }
        Node::Race {
            timeout_ms,
            branches,
            bind,
        } if branches.iter().all(|b| b.name.is_identifier()) && opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str(&format!("race {}", fmt_duration(*timeout_ms)));
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            let ind1 = indent_of(level + 1, indent);
            for br in branches {
                out.push_str(&ind1);
                out.push_str("branch ");
                out.push_str(&fmt_symbol(&br.name));
                out.push('\n');
                fmt_body(&br.body, level + 2, indent, multiline, out);
            }
        }
        Node::Scope {
            acquire,
            bind,
            body,
            finally,
        } if opt_ident(bind)
            && bind.is_some() == acquire.is_some()
            && acquire
                .as_ref()
                .is_none_or(|a| !fmt_expr(a, multiline).starts_with("@json ")) =>
        {
            out.push_str(&ind);
            out.push_str("scope");
            if let (Some(b), Some(a)) = (bind, acquire) {
                out.push(' ');
                out.push_str(&fmt_symbol(b));
                out.push_str(" = ");
                out.push_str(&fmt_expr(a, multiline));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
            if !finally.is_empty() {
                out.push_str(&ind);
                out.push_str("finally\n");
                fmt_body(finally, level + 1, indent, multiline, out);
            }
        }
        Node::Saga { steps } => {
            out.push_str(&ind);
            out.push_str("saga\n");
            let ind1 = indent_of(level + 1, indent);
            for st in steps {
                out.push_str(&ind1);
                out.push_str("step\n");
                fmt_body(&st.body, level + 2, indent, multiline, out);
                if !st.undo.is_empty() {
                    out.push_str(&ind1);
                    out.push_str("undo\n");
                    fmt_body(&st.undo, level + 2, indent, multiline, out);
                }
            }
        }
        Node::Pipe { steps, bind } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("pipe");
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            fmt_body(steps, level + 1, indent, multiline, out);
        }
        Node::Confirm {
            message,
            risk,
            body,
        } if risk.as_deref().is_none_or(is_word_token) => {
            out.push_str(&ind);
            out.push_str("confirm ");
            out.push_str(&compact_str(message, multiline));
            if let Some(r) = risk {
                out.push_str(", risk: ");
                out.push_str(r);
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Throttle {
            name,
            max,
            window_ms,
            body,
        } => {
            out.push_str(&ind);
            out.push_str("throttle ");
            out.push_str(&compact_str(name, multiline));
            out.push_str(&format!(
                ", max: {max}, per: {}\n",
                fmt_duration(*window_ms)
            ));
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Debounce {
            name,
            wait_ms,
            body,
        } => {
            out.push_str(&ind);
            out.push_str("debounce ");
            out.push_str(&compact_str(name, multiline));
            out.push_str(&format!(", wait: {}\n", fmt_duration(*wait_ms)));
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Verify {
            cmd,
            expect,
            message,
        } if call_like_operand(cmd, multiline) && call_like_operand(expect, multiline) => {
            out.push_str(&ind);
            out.push_str("verify ");
            out.push_str(&fmt_expr(cmd, multiline));
            out.push_str(" contains ");
            out.push_str(&fmt_expr(expect, multiline));
            if let Some(m) = message {
                out.push_str(": ");
                out.push_str(&compact_str(m, multiline));
            }
            out.push('\n');
        }
        Node::Memo {
            name,
            value,
            ty,
            effect,
        } if name.is_identifier() && opt_spellable_type(ty) => {
            if let Some(e) = effect {
                out.push_str(&ind);
                out.push_str("@effect(");
                out.push_str(effect_tag(*e));
                out.push_str(")\n");
            }
            out.push_str(&ind);
            out.push_str("memo ");
            out.push_str(&fmt_symbol(name));
            if let Some(t) = ty {
                out.push_str(": ");
                out.push_str(&t.label());
            }
            out.push_str(" = ");
            out.push_str(&fmt_expr(value, multiline));
            out.push('\n');
        }
        Node::Once { label, body, bind } if opt_ident(bind) => {
            out.push_str(&ind);
            out.push_str("once ");
            out.push_str(&compact_str(label, multiline));
            if let Some(b) = bind {
                out.push_str(" -> ");
                out.push_str(&fmt_symbol(b));
            }
            out.push('\n');
            fmt_body(body, level + 1, indent, multiline, out);
        }
        Node::Checkpoint { label } => {
            out.push_str(&ind);
            out.push_str("checkpoint ");
            out.push_str(&compact_str(label, multiline));
            out.push('\n');
        }
        Node::Await {
            binding,
            source,
            as_type,
            condition,
        } if opt_ident(binding)
            && opt_spellable_type(as_type)
            // The coercion type is spelled in the binding clause (`await $b: T = …`), so an
            // as_type without a binding has no native form — fall back to @json for that shape.
            && (binding.is_some() || as_type.is_none()) =>
        {
            out.push_str(&ind);
            out.push_str("await ");
            if let Some(b) = binding {
                out.push_str(&fmt_symbol(b));
                if let Some(t) = as_type {
                    out.push_str(": ");
                    out.push_str(&t.label());
                }
                out.push_str(" = ");
            }
            out.push_str(&compact_str(source, multiline));
            if let Some(condition) = condition {
                out.push_str(", when: ");
                out.push_str(&fmt_expr(condition, multiline));
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
/// Delegates to [`FlowEffect::tag`], the single source of truth for the tag vocabulary.
pub(crate) fn effect_tag(e: FlowEffect) -> &'static str {
    e.tag()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Param, Selector, ThingKind, ThingRef, TypeRef};

    #[test]
    fn compact_uses_single_space_indent_and_round_trips() {
        // A nested plan: `when x` with a body, so indentation is exercised.
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
        assert!(canonical.contains("\n  when x\n"), "canonical: {canonical}");
        assert!(compact.contains("\n when x\n"), "compact: {compact}");
        assert!(
            compact.len() < canonical.len(),
            "compact is shorter ({} vs {})",
            compact.len(),
            canonical.len()
        );
        assert_eq!(crate::parse::parse(&canonical).unwrap(), ast);
        assert_eq!(crate::parse::parse(&compact).unwrap(), ast);
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
        assert!(txt.contains("\n  msg = greet_op(who)\n"), "got: {txt}");
    }

    #[test]
    fn discarded_and_inline_calls_share_the_paren_form() {
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
        assert!(txt.contains("  git_stage([\".\"])\n"), "got: {txt}");
        assert!(txt.contains("  x = read(\"f\")\n"), "got: {txt}");
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

    // ----- Multi-line strings (L-39) -----

    #[test]
    fn emits_multiline_spelling_for_a_lit_string_containing_a_newline() {
        let ast = DraftAst {
            body: vec![Node::Bind {
                name: "x".into(),
                value: Box::new(Node::Lit {
                    value: serde_json::json!("first\nsecond"),
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let txt = format(&ast);
        assert!(
            txt.contains("x = \"\"\"first\nsecond\"\"\""),
            "expected the verbatim triple-quote spelling, got: {txt}"
        );
        assert!(
            !txt.contains("\\n"),
            "must not fall back to the escaped spelling: {txt}"
        );
        assert_eq!(crate::parse::parse(&txt).unwrap(), ast);
    }

    #[test]
    fn a_single_line_string_never_uses_the_multiline_spelling() {
        let ast = DraftAst {
            body: vec![Node::Lit {
                value: serde_json::json!("no newline here"),
            }],
            ..Default::default()
        };
        let txt = format(&ast);
        assert!(!txt.contains("\"\"\""), "got: {txt}");
    }

    #[test]
    fn falls_back_to_the_escaped_spelling_when_content_would_collide_with_the_delimiter() {
        // Content containing a literal `"""`, or ending in a `"`, cannot be told apart from the
        // closing delimiter by the "scan for the next `\"\"\"`" grammar rule (see parse.rs) — the
        // formatter must recognize both hazards and fall back to the always-safe escaped spelling,
        // so `parse(format(x)) == x` stays total.
        for value in [
            serde_json::json!("has \"\"\" triple quotes\nand a newline"),
            serde_json::json!("first line\nsecond line ends in a quote\""),
        ] {
            let ast = DraftAst {
                body: vec![Node::Lit {
                    value: value.clone(),
                }],
                ..Default::default()
            };
            let txt = format(&ast);
            assert!(
                !txt.contains("\"\"\""),
                "expected the escaped fallback for {value:?}, got: {txt}"
            );
            assert_eq!(
                crate::parse::parse(&txt).unwrap().body,
                vec![Node::Lit { value }]
            );
        }
    }

    #[test]
    fn crlf_content_falls_back_to_the_escaped_spelling() {
        // CST semantic lowering normalizes `\r\n` -> `\n` inside a `"""..."""` token. That means
        // the `\r` would be lost on the way back through `parse` — a real round-trip violation if
        // the formatter chose the multi-line spelling. Treat `\r` as unsafe (like the
        // `"""`/trailing-`"` hazards) and fall back to the escaped spelling instead.
        let value = serde_json::json!("line one\r\nline two");
        let ast = DraftAst {
            body: vec![Node::Lit {
                value: value.clone(),
            }],
            ..Default::default()
        };
        let txt = format(&ast);
        assert!(
            !txt.contains("\"\"\""),
            "a `\\r\\n`-bearing string must use the escaped spelling, got: {txt}"
        );
        assert_eq!(
            crate::parse::parse(&txt).unwrap().body,
            vec![Node::Lit { value }],
            "parse(format(x)) == x must hold for CRLF content"
        );
    }

    #[test]
    fn format_compact_keeps_the_escaped_single_line_spelling() {
        // Compact source never emits the multi-line spelling, so it stays visually single-line per
        // statement while remaining parseable.
        let ast = DraftAst {
            body: vec![Node::Lit {
                value: serde_json::json!("first\nsecond"),
            }],
            ..Default::default()
        };
        let compact = format_compact(&ast);
        assert!(!compact.contains("\"\"\""), "got: {compact}");
        assert!(compact.contains("\\n"), "got: {compact}");
        assert_eq!(crate::parse::parse(&compact).unwrap(), ast);
    }

    #[test]
    fn assert_ctx_route_fmt_native_string_fields_support_the_multiline_spelling() {
        let assert_ast = DraftAst {
            body: vec![Node::Assert {
                cond: Box::new(Node::Lit {
                    value: serde_json::json!(true),
                }),
                message: Some("line one\nline two".into()),
            }],
            ..Default::default()
        };
        let txt = format(&assert_ast);
        assert!(txt.contains("\"\"\"line one\nline two\"\"\""), "got: {txt}");
        assert_eq!(crate::parse::parse(&txt).unwrap(), assert_ast);

        let ctx_ast = DraftAst {
            body: vec![Node::Ctx {
                name: "pack".into(),
                purpose: Some("first\nsecond".into()),
                include: vec![],
                exclude: vec![],
                budget: None,
            }],
            ..Default::default()
        };
        let txt = format(&ctx_ast);
        assert!(txt.contains("\"\"\"first\nsecond\"\"\""), "got: {txt}");
        assert_eq!(crate::parse::parse(&txt).unwrap(), ctx_ast);

        let route_ast = DraftAst {
            body: vec![Node::Route {
                selector: Box::new(Node::Var {
                    name: "kind".into(),
                }),
                cases: vec![crate::ast::RouteCase {
                    label: "a\nb".into(),
                    body: vec![Node::Return {
                        value: Box::new(Node::Lit {
                            value: serde_json::json!(1),
                        }),
                    }],
                }],
                default: vec![],
            }],
            ..Default::default()
        };
        let txt = format(&route_ast);
        assert!(txt.contains("\"\"\"a\nb\"\"\""), "got: {txt}");
        assert_eq!(crate::parse::parse(&txt).unwrap(), route_ast);

        // `Fmt` only has a native spelling in *expression* position (a bare statement falls to
        // `@json`, since there's no `do`/bind marker for it) — exercise it as a `return` value.
        let fmt_ast = DraftAst {
            body: vec![Node::Return {
                value: Box::new(Node::Fmt {
                    template: "hello\n{name}".into(),
                }),
            }],
            ..Default::default()
        };
        let txt = format(&fmt_ast);
        assert!(txt.contains("fmt(\"\"\"hello\n{name}\"\"\")"), "got: {txt}");
        assert_eq!(crate::parse::parse(&txt).unwrap(), fmt_ast);
    }

    /// L-53: the optional-access `?` suffix round-trips through native sugar — `$obj.a?` parses to a
    /// lenient `Jq`, formats back with the `?`, and stays distinct from strict `$obj.a`.
    #[test]
    fn optional_field_access_sugar_round_trips() {
        let opt = crate::parse::parse("flow f\n  $x = $obj.a?\n  return $x\n").unwrap();
        // The bind lowers to a lenient `Jq`.
        let is_opt = matches!(
            &opt.body[0],
            Node::Bind { value, .. }
                if matches!(value.as_ref(), Node::Jq { optional: true, path, .. } if path == ".a")
        );
        assert!(
            is_opt,
            "`$obj.a?` should lower to an optional Jq: {:?}",
            opt.body[0]
        );
        let txt = format(&opt);
        assert!(
            txt.contains("obj.a?"),
            "optional access should format with `?`: {txt}"
        );
        assert_eq!(
            crate::parse::parse(&txt).unwrap(),
            opt,
            "`?` must round-trip"
        );

        // Strict access has no `?` and lowers to a strict Jq.
        let strict = crate::parse::parse("flow f\n  $x = $obj.a\n  return $x\n").unwrap();
        let txt2 = format(&strict);
        assert!(
            txt2.contains("obj.a") && !txt2.contains("obj.a?"),
            "strict access is bare: {txt2}"
        );
        assert_eq!(crate::parse::parse(&txt2).unwrap(), strict);
    }

    /// C-320 (failing first): a JSON-string bracket segment reaches object keys that cannot be
    /// represented by the dotted identifier grammar. The lowered path keeps those segments in
    /// canonical JSON-string brackets, so dots, brackets, quotes and backslashes inside a key can
    /// never be mistaken for path syntax. A numeric-looking *quoted* key stays distinct from an
    /// array index.
    #[test]
    fn quoted_field_access_has_an_unambiguous_round_trip() {
        let source = r#"flow f
  content_type = response.headers["content-type"]
  escaped = response["a.b"]["br[ack]"]["quote\"slash\\"]["0"]?
  return escaped
"#;
        let ast = crate::parse::parse(source).expect("quoted object-key access parses");

        let paths = ast
            .body
            .iter()
            .filter_map(|node| match node {
                Node::Bind { value, .. } => match value.as_ref() {
                    Node::Jq { path, optional, .. } => Some((path.as_str(), *optional)),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                (r#".headers["content-type"]"#, false),
                (r#".["a.b"]["br[ack]"]["quote\"slash\\"]["0"]"#, true),
            ]
        );

        let formatted = format(&ast);
        assert!(
            formatted.contains(r#"response.headers["content-type"]"#),
            "quoted key must stay native: {formatted}"
        );
        assert!(
            formatted.contains(r#"response["a.b"]["br[ack]"]["quote\"slash\\"]["0"]?"#),
            "escaped keys and optional marker must stay native: {formatted}"
        );
        assert_eq!(crate::parse::parse(&formatted).unwrap(), ast);
    }

    /// C-320 review failing-first: formatting is total over the public AST even when a hand-built
    /// jq path has junk or whitespace after an otherwise complete bracket segment.
    #[test]
    fn malformed_bracket_suffixes_format_through_json_without_panicking() {
        for path in [r#".a[0]suffix"#, r#".["key"] suffix"#] {
            let ast = DraftAst {
                body: vec![Node::Bind {
                    name: "bad".into(),
                    value: Box::new(Node::Jq {
                        path: path.into(),
                        input: Box::new(Node::Var {
                            name: "input".into(),
                        }),
                        optional: false,
                    }),
                    ty: None,
                    effect: None,
                }],
                ..Default::default()
            };
            let formatted = format(&ast);
            assert!(formatted.contains("@json"), "{path}: {formatted}");
            assert_eq!(crate::parse::parse(&formatted).unwrap(), ast);
        }
    }

    /// Golden nested case named by the story's Acceptance: a multi-line string inside an object
    /// value-template inside an `each` body.
    #[test]
    fn multiline_string_inside_an_object_template_inside_each_round_trips() {
        let ast = DraftAst {
            body: vec![Node::Each {
                source: Box::new(Node::Var {
                    name: "files".into(),
                }),
                item: "f".into(),
                body: vec![Node::Call {
                    op: "write".into(),
                    args: vec![Node::Obj {
                        fields: {
                            let mut m = std::collections::BTreeMap::new();
                            m.insert("path".to_string(), Box::new(Node::Var { name: "f".into() }));
                            m.insert(
                                "content".to_string(),
                                Box::new(Node::Lit {
                                    value: serde_json::json!("line one\nline two\nline three"),
                                }),
                            );
                            m
                        },
                    }],
                }],
                collect: None,
                flat: false,
            }],
            ..Default::default()
        };
        let txt = format(&ast);
        assert!(
            txt.contains("\"\"\"line one\nline two\nline three\"\"\""),
            "got: {txt}"
        );
        assert_eq!(crate::parse::parse(&txt).unwrap(), ast);
    }

    /// The other dominant corpus shape: a pure-JSON `Lit` object (quoted keys) rather than a
    /// dynamic value template — `do edit {"path":...,"content":"""..."""}`. `compact_value`
    /// recurses into `Lit`'s nested JSON, so a string *leaf* at any depth gets the multi-line
    /// spelling while the surrounding object stays ordinary compact JSON syntax.
    #[test]
    fn multiline_string_inside_a_pure_json_lit_object_round_trips() {
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "edit".into(),
                args: vec![Node::Lit {
                    value: serde_json::json!({"path": "f.txt", "content": "line one\nline two"}),
                }],
            }],
            ..Default::default()
        };
        let txt = format(&ast);
        assert!(
            txt.contains("\"content\":\"\"\"line one\nline two\"\"\""),
            "got: {txt}"
        );
        assert!(
            txt.contains("\"path\":\"f.txt\""),
            "the rest of the object stays plain compact JSON: {txt}"
        );
        assert_eq!(crate::parse::parse(&txt).unwrap(), ast);
    }

    /// Scope pin: the `@json` escape line is documented as "exactly the wire format" — it stays
    /// pure single-line compact JSON even when the node it carries has a newline-bearing string
    /// field, never the multi-line spelling. (Only the natively-spelled positions this story names
    /// — `lit`, value templates, `fmt`, `assert`'s message, `ctx`'s purpose, `route`'s case label —
    /// get the new spelling; a node with no native form is out of scope.)
    #[test]
    fn json_escape_line_never_uses_the_multiline_spelling() {
        // A non-identifier (newline-bearing) name has no native spelling, so it always uses the
        // @json escape — a stable carrier for this invariant regardless of which node kinds gain
        // native syntax.
        let ast = DraftAst {
            body: vec![Node::Var {
                name: "line1\nline2".into(),
            }],
            ..Default::default()
        };
        let txt = format(&ast);
        let json_line = txt
            .lines()
            .find(|l| l.trim_start().starts_with("@json "))
            .unwrap_or_else(|| panic!("expected an @json line, got: {txt}"));
        assert!(
            !json_line.contains("\"\"\"") && json_line.contains("\\n"),
            "the @json line stays single-line compact JSON: {json_line}"
        );
        assert_eq!(crate::parse::parse(&txt).unwrap(), ast);
    }

    /// General edge-shape fuzzing beyond the property test's fixed pool: newline-only content, a
    /// leading newline, consecutive blank lines, and trailing spaces mid-content all round-trip.
    #[test]
    fn edge_content_shapes_round_trip() {
        for s in ["\n", "\nX", "a\n\n\nb", "x  \ny  \n", "a\nb"] {
            let ast = DraftAst {
                body: vec![Node::Lit {
                    value: serde_json::json!(s),
                }],
                ..Default::default()
            };
            let txt = format(&ast);
            assert_eq!(
                crate::parse::parse(&txt).unwrap().body,
                vec![Node::Lit {
                    value: serde_json::json!(s)
                }],
                "round-trip failed for {s:?}: {txt}"
            );
        }
    }
}

//! `parse` — read canonical Flux-Lang **text** back into a [`DraftAst`]. The round-trip partner of
//! [`crate::format`]: `parse(&format(&ast)) == ast` for every `DraftAst` (the supported subset natively,
//! everything else via the `@json` escape). Hand-written, indentation-sensitive recursive descent.
//!
//! It is **total**: malformed input returns [`FlowError::Parse`], never a panic.
//!
//! # Surface (see [`crate::format`] for the full grammar)
//! - Header: `flow [<name>][(<param>, …)][ -> <type>]`, body indented 2 (or any consistent step).
//! - `$x = <expr>`, `$x: T = <expr>` — bind (with optional `@effect(<tag>)` on the line above).
//! - `do <op> <arg>, …` or `<op>(<arg>, …)` — a bare call (both forms accepted; `do` is canonical).
//! - `$pack += $a, $b` — ctx_append; `ctx $p` + indented `purpose`/`budget`/`include`/`exclude`.
//! - `when`/`else`, `unless`, `each $x in <src> [-> [flat] $c]`, `repeat <n> [-> $c]` (`until` first
//!   body line), `seq [-> $c]`, `return <expr>`.
//! - `match <subj>`/`route <sel>` (`case <v>` arms + `default`), `fallback [-> $b]` (`branch` arms),
//!   `loop for <ms> every <ms> [-> $b]` (`until` first body line), `timeout <ms> [-> $b]`,
//!   `budget <n> [-> $b]`.
//! - Inline `fmt("<template>")` (the `Fmt` node) and `$var.path` field-access sugar (lowers to `jq`).
//! - `@json <compact-json>` — the wire-format escape for any unsupported node (inline or statement).
//! - A `goal "…"` header line is tolerated and ignored (`DraftAst` has no goal slot).

use crate::ast::{DraftAst, FlowEffect, Node, Param, SymbolName, TypeRef};
use crate::error::{FlowError, Result};

/// Parse a single Flux-Lang flow from text into a [`DraftAst`].
pub fn parse(src: &str) -> Result<DraftAst> {
    let lines = preprocess(src)?;
    if lines.is_empty() {
        return Err(perr("empty input: expected a `flow` header"));
    }
    if lines[0].indent != 0 {
        return Err(perr("the `flow` header must start at column 0"));
    }
    let (name, params, returns) = parse_header(&lines[0].text)?;

    // The body is every indented line. Top-level (column-0) lines after the header may only be a
    // tolerated-and-ignored `goal "…"` directive (there is no AST slot for it).
    let mut body_lines: Vec<Line> = Vec::new();
    for l in &lines[1..] {
        if l.indent == 0 {
            if is_goal_line(&l.text) {
                continue;
            }
            return Err(perr(&format!("unexpected top-level line: `{}`", l.text)));
        }
        body_lines.push(l.clone());
    }

    let (body, _) = parse_stmts(&body_lines, 0)?;
    Ok(DraftAst {
        name,
        params,
        returns,
        body,
    })
}

fn perr(msg: &str) -> FlowError {
    FlowError::Parse(msg.to_string())
}

fn is_goal_line(t: &str) -> bool {
    t == "goal" || t.starts_with("goal ")
}

// ---------------------------------------------------------------------------
// Lexing: logical lines (comment-stripped, blanks removed, indent measured)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Line {
    indent: usize,
    text: String,
}

fn preprocess(src: &str) -> Result<Vec<Line>> {
    let mut out = Vec::new();
    for raw in src.lines() {
        let code = strip_comment(raw);
        let mut indent = 0usize;
        for c in code.chars() {
            match c {
                ' ' => indent += 1,
                '\t' => return Err(perr("tabs are not allowed for indentation")),
                _ => break,
            }
        }
        let text = code.trim();
        if text.is_empty() {
            continue;
        }
        out.push(Line {
            indent,
            text: text.to_string(),
        });
    }
    Ok(out)
}

/// Remove a `#` line comment, ignoring `#` inside JSON double-quoted strings.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == b'#' {
            return &line[..i];
        }
    }
    line
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_var_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

fn is_op_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'
}

fn take_while(s: &str, pred: impl Fn(char) -> bool) -> (&str, &str) {
    let mut end = s.len();
    for (i, c) in s.char_indices() {
        if !pred(c) {
            end = i;
            break;
        }
    }
    (&s[..end], &s[end..])
}

fn parse_header(t: &str) -> Result<(Option<String>, Vec<Param>, Option<TypeRef>)> {
    let rest = if t == "flow" {
        ""
    } else if let Some(r) = t.strip_prefix("flow") {
        if r.starts_with(char::is_whitespace) || r.starts_with('(') {
            r
        } else {
            return Err(perr("expected a `flow` header"));
        }
    } else {
        return Err(perr("expected a `flow` header"));
    };
    let rest = rest.trim_start();

    // Optional name (absent when the next token opens params or the return arrow).
    let (name, rest) = if rest.is_empty() || rest.starts_with('(') || rest.starts_with("->") {
        (None, rest)
    } else {
        let (nm, r) = take_while(rest, is_name_char);
        if nm.is_empty() {
            (None, rest)
        } else {
            (Some(nm.to_string()), r.trim_start())
        }
    };

    // Optional parameter list.
    let (params, rest) = if rest.starts_with('(') {
        let close = rest
            .find(')')
            .ok_or_else(|| perr("unterminated parameter list"))?;
        let inner = &rest[1..close];
        (parse_params(inner)?, rest[close + 1..].trim_start())
    } else {
        (Vec::new(), rest)
    };

    // Optional return type.
    let returns = if let Some(r) = rest.strip_prefix("->") {
        let ty = r.trim();
        if ty.is_empty() {
            return Err(perr("expected a return type after `->`"));
        }
        Some(parse_type(ty))
    } else if rest.is_empty() {
        None
    } else {
        return Err(perr(&format!("unexpected text in flow header: `{rest}`")));
    };

    Ok((name, params, returns))
}

fn parse_params(inner: &str) -> Result<Vec<Param>> {
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        let colon = part
            .find(':')
            .ok_or_else(|| perr(&format!("parameter missing `:`: `{part}`")))?;
        let name = part[..colon].trim();
        let ty = part[colon + 1..].trim();
        if name.is_empty() {
            return Err(perr("empty parameter name"));
        }
        out.push(Param {
            name: name.into(),
            ty: parse_type(ty),
        });
    }
    Ok(out)
}

fn parse_type(s: &str) -> TypeRef {
    let s = s.trim();
    match s {
        "Any" => TypeRef::Any,
        "Bool" => TypeRef::Bool,
        "Number" => TypeRef::Number,
        "String" => TypeRef::String,
        _ => {
            if let Some(inner) = s.strip_prefix("List<").and_then(|x| x.strip_suffix('>')) {
                TypeRef::List(Box::new(parse_type(inner)))
            } else {
                TypeRef::Named(s.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Statements (indentation-delimited blocks)
// ---------------------------------------------------------------------------

/// Parse all statements at the current indentation level. `lines` starts at the first candidate line;
/// returns the nodes and the number of lines consumed.
fn parse_stmts(lines: &[Line], parent_indent: usize) -> Result<(Vec<Node>, usize)> {
    let mut nodes = Vec::new();
    if lines.is_empty() || lines[0].indent <= parent_indent {
        return Ok((nodes, 0));
    }
    let block_indent = lines[0].indent;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].indent <= parent_indent {
            break;
        }
        if lines[i].indent != block_indent {
            return Err(perr(&format!(
                "unexpected indentation at: `{}`",
                lines[i].text
            )));
        }
        let (node, used) = parse_stmt(&lines[i..], block_indent)?;
        nodes.push(node);
        i += used;
    }
    Ok((nodes, i))
}

/// The maximal run of lines after `lines[0]` indented deeper than `header_indent` (a block body).
fn child_region(lines: &[Line], header_indent: usize) -> &[Line] {
    let mut n = 0;
    for l in &lines[1..] {
        if l.indent > header_indent {
            n += 1;
        } else {
            break;
        }
    }
    &lines[1..1 + n]
}

/// Match a leading keyword token; returns the trimmed remainder when `t` is exactly `kw` or `kw`
/// followed by whitespace.
fn kw<'a>(t: &'a str, k: &str) -> Option<&'a str> {
    if t == k {
        return Some("");
    }
    if let Some(r) = t.strip_prefix(k) {
        if r.starts_with(char::is_whitespace) {
            return Some(r.trim_start());
        }
    }
    None
}

/// Parse one statement. `lines[0]` is the header; block statements consume their indented body (and,
/// for `when`, an `else` clause). Returns the node and the total lines consumed (header included).
fn parse_stmt(lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let t = lines[0].text.as_str();

    // `@effect(tag)` annotates the bind on the next line.
    if let Some(rest) = t.strip_prefix("@effect(") {
        let close = rest
            .find(')')
            .ok_or_else(|| perr("unterminated `@effect(`"))?;
        let tag = &rest[..close];
        if !rest[close + 1..].trim().is_empty() {
            return Err(perr("trailing text after `@effect(...)`"));
        }
        let eff = effect_from_tag(tag).ok_or_else(|| perr(&format!("unknown effect: `{tag}`")))?;
        if lines.len() < 2 || lines[1].indent != indent {
            return Err(perr("`@effect` must directly precede a bind"));
        }
        let (inner, used) = parse_stmt(&lines[1..], indent)?;
        return Ok((set_effect(inner, eff)?, 1 + used));
    }

    // `@json <compact-json>` escape (statement position).
    if let Some(rest) = t.strip_prefix("@json") {
        let (v, tail) = take_json(rest.trim_start())?;
        if !tail.trim().is_empty() {
            return Err(perr("trailing text after `@json` value"));
        }
        let node: Node =
            serde_json::from_value(v).map_err(|e| perr(&format!("invalid `@json` node: {e}")))?;
        return Ok((node, 1));
    }

    if t == "else" {
        return Err(perr("`else` without a matching `when`"));
    }
    if kw(t, "until").is_some() {
        return Err(perr(
            "`until` is only valid as the first line of a `repeat` body",
        ));
    }

    if let Some(rest) = kw(t, "do") {
        return parse_do_call(rest);
    }
    if let Some(rest) = kw(t, "when") {
        return parse_when(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "unless") {
        return parse_unless(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "each") {
        return parse_each(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "repeat") {
        return parse_repeat(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "match") {
        return parse_match(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "route") {
        return parse_route(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "fallback") {
        return parse_fallback(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "loop") {
        return parse_loop(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "timeout") {
        return parse_timeout(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "budget") {
        return parse_budget(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "seq") {
        return parse_seq(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "ctx") {
        return parse_ctx(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "return") {
        return parse_return(rest);
    }

    if t.starts_with('$') {
        return parse_dollar(t);
    }

    // Otherwise the whole line is a single expression statement (e.g. a paren-form bare call).
    let (node, tail) = parse_expr(t)?;
    if !tail.trim().is_empty() {
        return Err(perr(&format!("trailing text after expression: `{tail}`")));
    }
    Ok((node, 1))
}

fn parse_when(cond_str: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let cond = parse_full_expr(cond_str, "when condition")?;
    let then_region = child_region(lines, indent);
    let (then, _) = parse_stmts(then_region, indent)?;
    let mut used = 1 + then_region.len();

    let mut otherwise = Vec::new();
    if let Some(cand) = lines.get(used) {
        if cand.indent == indent && cand.text == "else" {
            let else_lines = &lines[used..];
            let else_region = child_region(else_lines, indent);
            let (ow, _) = parse_stmts(else_region, indent)?;
            otherwise = ow;
            used += 1 + else_region.len();
        }
    }

    Ok((
        Node::When {
            cond: Box::new(cond),
            then,
            otherwise,
        },
        used,
    ))
}

fn parse_unless(cond_str: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let cond = parse_full_expr(cond_str, "unless condition")?;
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((
        Node::Unless {
            cond: Box::new(cond),
            body,
        },
        1 + region.len(),
    ))
}

fn parse_each(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let item_part = rest
        .strip_prefix('$')
        .ok_or_else(|| perr("`each` expects `$item`"))?;
    let (item, r) = take_while(item_part, is_var_char);
    if item.is_empty() {
        return Err(perr("`each` has an empty item symbol"));
    }
    let r = kw(r.trim_start(), "in").ok_or_else(|| perr("`each` expects `in`"))?;
    let (source, after) = parse_expr(r)?;
    let after = after.trim_start();

    let (collect, flat) = if let Some(a) = after.strip_prefix("->") {
        let a = a.trim_start();
        let (a, flat) = match kw(a, "flat") {
            Some(a2) => (a2, true),
            None => (a, false),
        };
        let nm = a
            .trim_start()
            .strip_prefix('$')
            .ok_or_else(|| perr("`each` expects `$collect` after `->`"))?;
        let (nm, tail) = take_while(nm, is_var_char);
        if nm.is_empty() || !tail.trim().is_empty() {
            return Err(perr("malformed `$collect` in `each`"));
        }
        (Some(SymbolName::from(nm)), flat)
    } else if after.is_empty() {
        (None, false)
    } else {
        return Err(perr(&format!(
            "unexpected text in `each` header: `{after}`"
        )));
    };

    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((
        Node::Each {
            source: Box::new(source),
            item: item.into(),
            body,
            collect,
            flat,
        },
        1 + region.len(),
    ))
}

fn parse_repeat(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let (num, r) = take_while(rest, |c| c.is_ascii_digit());
    if num.is_empty() {
        return Err(perr("`repeat` expects a count"));
    }
    let max: u32 = num
        .parse()
        .map_err(|_| perr("`repeat` count out of range"))?;
    let r = r.trim_start();
    let collect = if let Some(a) = r.strip_prefix("->") {
        Some(parse_arrow_sym(a, "repeat")?)
    } else if r.is_empty() {
        None
    } else {
        return Err(perr(&format!("unexpected text in `repeat` header: `{r}`")));
    };

    let region = child_region(lines, indent);
    let (until, body_region) = match region.first() {
        Some(first) => match kw(&first.text, "until") {
            Some(u) => {
                let uexpr = parse_full_expr(u, "until condition")?;
                (Some(Box::new(uexpr)), &region[1..])
            }
            None => (None, region),
        },
        None => (None, region),
    };
    let (body, _) = parse_stmts(body_region, indent)?;
    Ok((
        Node::Repeat {
            max,
            until,
            body,
            collect,
        },
        1 + region.len(),
    ))
}

/// Take a leading unsigned-integer token, returning `(value, rest)`.
fn take_u64(s: &str) -> Result<(u64, &str)> {
    let (digits, rest) = take_while(s.trim_start(), |c| c.is_ascii_digit());
    let n = digits
        .parse::<u64>()
        .map_err(|_| perr(&format!("expected a number, got: `{s}`")))?;
    Ok((n, rest))
}

/// Parse an optional `-> $bind` header tail (returns `None` for an empty remainder).
fn parse_optional_arrow_bind(r: &str, ctx: &str) -> Result<Option<SymbolName>> {
    let r = r.trim_start();
    if r.is_empty() {
        Ok(None)
    } else if let Some(a) = r.strip_prefix("->") {
        Ok(Some(parse_arrow_sym(a, ctx)?))
    } else {
        Err(perr(&format!("unexpected text in `{ctx}` header: `{r}`")))
    }
}

/// If `region`'s first line is `until <cond>`, split it off (the `repeat`/`loop` guard), returning the
/// optional condition and the remaining body region.
fn split_until(region: &[Line]) -> Result<(Option<Box<Node>>, &[Line])> {
    match region.first() {
        Some(first) => match kw(&first.text, "until") {
            Some(u) => {
                let uexpr = parse_full_expr(u, "until condition")?;
                Ok((Some(Box::new(uexpr)), &region[1..]))
            }
            None => Ok((None, region)),
        },
        None => Ok((None, region)),
    }
}

/// Parse the arms of a `match`/`route`/`fallback` block: each arm is a header line at the region's base
/// indent (`<arm_kw> …` or `default`) followed by its indented body. Returns each arm's header-remainder
/// + body, plus the `default` body. `default` for a `fallback` is rejected by its caller.
#[allow(clippy::type_complexity)]
fn parse_arms(region: &[Line], arm_kw: &str) -> Result<(Vec<(String, Vec<Node>)>, Vec<Node>)> {
    let mut arms: Vec<(String, Vec<Node>)> = Vec::new();
    let mut default: Vec<Node> = Vec::new();
    if region.is_empty() {
        return Ok((arms, default));
    }
    let arm_indent = region[0].indent;
    let mut i = 0;
    while i < region.len() {
        if region[i].indent != arm_indent {
            return Err(perr(&format!(
                "unexpected indentation in `{arm_kw}` arms: `{}`",
                region[i].text
            )));
        }
        let t = region[i].text.as_str();
        let body_region = child_region(&region[i..], arm_indent);
        let (body, _) = parse_stmts(body_region, arm_indent)?;
        if t == "default" {
            default = body;
        } else if let Some(hdr) = kw(t, arm_kw) {
            arms.push((hdr.to_string(), body));
        } else {
            return Err(perr(&format!(
                "expected `{arm_kw}` or `default`, got: `{t}`"
            )));
        }
        i += 1 + body_region.len();
    }
    Ok((arms, default))
}

fn parse_match(subject_str: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let subject = parse_full_expr(subject_str, "match subject")?;
    let region = child_region(lines, indent);
    let (arms, default) = parse_arms(region, "case")?;
    let mut cases = Vec::with_capacity(arms.len());
    for (value_str, body) in arms {
        let value = parse_full_expr(&value_str, "case value")?;
        cases.push(crate::ast::MatchCase { value, body });
    }
    Ok((
        Node::Match {
            subject: Box::new(subject),
            cases,
            default,
        },
        1 + region.len(),
    ))
}

fn parse_route(selector_str: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let selector = parse_full_expr(selector_str, "route selector")?;
    let region = child_region(lines, indent);
    let (arms, default) = parse_arms(region, "case")?;
    let mut cases = Vec::with_capacity(arms.len());
    for (label_str, body) in arms {
        let label = match parse_full_expr(&label_str, "route case label")? {
            Node::Lit {
                value: serde_json::Value::String(s),
            } => s,
            _ => return Err(perr("a `route` `case` label must be a string literal")),
        };
        cases.push(crate::ast::RouteCase { label, body });
    }
    Ok((
        Node::Route {
            selector: Box::new(selector),
            cases,
            default,
        },
        1 + region.len(),
    ))
}

fn parse_fallback(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let bind = parse_optional_arrow_bind(rest, "fallback")?;
    let region = child_region(lines, indent);
    let (arms, default) = parse_arms(region, "branch")?;
    if !default.is_empty() {
        return Err(perr("`fallback` has no `default` arm — use `branch` only"));
    }
    let branches = arms
        .into_iter()
        .map(|(_, body)| crate::ast::FallbackBranch { body })
        .collect();
    Ok((Node::Fallback { branches, bind }, 1 + region.len()))
}

fn parse_loop(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let r = kw(rest.trim_start(), "for").ok_or_else(|| perr("`loop` expects `for <ms>`"))?;
    let (for_ms, r) = take_u64(r)?;
    let r = kw(r.trim_start(), "every").ok_or_else(|| perr("`loop` expects `every <ms>`"))?;
    let (every_ms, r) = take_u64(r)?;
    let bind = parse_optional_arrow_bind(r, "loop")?;
    let region = child_region(lines, indent);
    let (until, body_region) = split_until(region)?;
    let (body, _) = parse_stmts(body_region, indent)?;
    Ok((
        Node::Loop {
            for_ms,
            every_ms,
            until,
            body,
            bind,
        },
        1 + region.len(),
    ))
}

fn parse_timeout(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (ms, r) = take_u64(rest.trim_start())?;
    let bind = parse_optional_arrow_bind(r, "timeout")?;
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((Node::Timeout { ms, body, bind }, 1 + region.len()))
}

fn parse_budget(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (digits, r) = take_while(rest.trim_start(), |c| c.is_ascii_digit());
    let limit: u32 = digits
        .parse()
        .map_err(|_| perr("`budget` expects a numeric limit"))?;
    let bind = parse_optional_arrow_bind(r, "budget")?;
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((Node::Budget { limit, body, bind }, 1 + region.len()))
}

fn parse_seq(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let bind = if rest.is_empty() {
        None
    } else if let Some(a) = rest.strip_prefix("->") {
        Some(parse_arrow_sym(a, "seq")?)
    } else {
        return Err(perr(&format!("unexpected text in `seq` header: `{rest}`")));
    };
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((Node::Seq { body, bind }, 1 + region.len()))
}

fn parse_ctx(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let nm = rest
        .strip_prefix('$')
        .ok_or_else(|| perr("`ctx` expects `$name`"))?;
    let (name, tail) = take_while(nm, is_var_char);
    if name.is_empty() {
        return Err(perr("`ctx` has an empty name"));
    }
    if !tail.trim().is_empty() {
        return Err(perr(&format!(
            "unexpected text after `ctx $name`: `{tail}`"
        )));
    }

    let region = child_region(lines, indent);
    let mut purpose = None;
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    let mut budget = None;
    for l in region {
        let lt = l.text.as_str();
        if let Some(r) = kw(lt, "purpose") {
            let (v, tail) = take_json(r.trim_start())?;
            if !tail.trim().is_empty() {
                return Err(perr("trailing text after `purpose`"));
            }
            match v {
                serde_json::Value::String(s) => purpose = Some(s),
                _ => return Err(perr("`purpose` must be a string")),
            }
        } else if let Some(r) = kw(lt, "budget") {
            budget = Some(r.trim().parse().map_err(|_| perr("invalid `budget`"))?);
        } else if let Some(r) = kw(lt, "include") {
            include = parse_sym_list(r)?;
        } else if let Some(r) = kw(lt, "exclude") {
            exclude = parse_sym_list(r)?;
        } else {
            return Err(perr(&format!("unknown `ctx` attribute: `{lt}`")));
        }
    }

    Ok((
        Node::Ctx {
            name: name.into(),
            purpose,
            include,
            exclude,
            budget,
        },
        1 + region.len(),
    ))
}

fn parse_return(rest: &str) -> Result<(Node, usize)> {
    let rest = rest.trim();
    let value = if rest.is_empty() {
        Node::Lit {
            value: serde_json::Value::Null,
        }
    } else {
        parse_full_expr(rest, "return value")?
    };
    Ok((
        Node::Return {
            value: Box::new(value),
        },
        1,
    ))
}

/// A `$name`-led statement: a bare var, a ctx_append (`+=`), or a bind (`=` / `: T =`).
fn parse_dollar(t: &str) -> Result<(Node, usize)> {
    let (name, rest) = take_while(&t[1..], is_var_char);
    if name.is_empty() {
        return Err(perr("empty symbol after `$`"));
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Ok((Node::Var { name: name.into() }, 1));
    }
    if let Some(r) = rest.strip_prefix("+=") {
        let add = parse_sym_list(r)?;
        return Ok((
            Node::CtxAppend {
                ctx: name.into(),
                add,
            },
            1,
        ));
    }
    if let Some(r) = rest.strip_prefix(':') {
        let r = r.trim_start();
        let eq = r
            .find('=')
            .ok_or_else(|| perr("expected `=` in typed bind"))?;
        let ty = parse_type(r[..eq].trim());
        let value = parse_full_expr(&r[eq + 1..], "bind value")?;
        return Ok((
            Node::Bind {
                name: name.into(),
                value: Box::new(value),
                ty: Some(ty),
                effect: None,
            },
            1,
        ));
    }
    if let Some(r) = rest.strip_prefix('=') {
        let value = parse_full_expr(r, "bind value")?;
        return Ok((
            Node::Bind {
                name: name.into(),
                value: Box::new(value),
                ty: None,
                effect: None,
            },
            1,
        ));
    }
    Err(perr(&format!("expected `=`, `+=` or `:` after `${name}`")))
}

/// Parse a `do <op> <arg>, …` bare call.
fn parse_do_call(rest: &str) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let (op, r) = take_while(rest, is_op_char);
    if op.is_empty() {
        return Err(perr("`do` expects an operation name"));
    }
    let r = r.trim_start();
    let args = if r.is_empty() {
        Vec::new()
    } else {
        parse_arg_list(r)?
    };
    Ok((
        Node::Call {
            op: op.to_string(),
            args,
        },
        1,
    ))
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// Parse exactly one expression that must span the whole of `s` (no trailing tokens).
fn parse_full_expr(s: &str, ctx: &str) -> Result<Node> {
    let (node, tail) = parse_expr(s)?;
    if !tail.trim().is_empty() {
        return Err(perr(&format!("trailing text in {ctx}: `{tail}`")));
    }
    Ok(node)
}

/// Parse a single expression from the front of `s`, returning it and the unconsumed remainder.
fn parse_expr(s: &str) -> Result<(Node, &str)> {
    let s = s.trim_start();
    let first = s
        .chars()
        .next()
        .ok_or_else(|| perr("expected an expression"))?;
    match first {
        '$' => {
            let (name, rest) = take_while(&s[1..], is_var_char);
            if name.is_empty() {
                return Err(perr("empty symbol after `$`"));
            }
            // Field-access sugar: `$plan.kind` lowers to `jq(".kind", $plan)`. `is_var_char` admits `.`,
            // so the whole `plan.kind` is taken as one token; split on the first `.` to recover the
            // symbol + the jq path. The formatter only emits this sugar for simple dotted paths, so it
            // round-trips; anything else (array indices, non-Var input) goes through `@json`.
            if let Some(dot) = name.find('.') {
                let (var, path) = name.split_at(dot); // `path` keeps the leading `.`
                if var.is_empty() {
                    return Err(perr("field access needs a symbol before `.`"));
                }
                return Ok((
                    Node::Jq {
                        path: path.to_string(),
                        input: Box::new(Node::Var { name: var.into() }),
                    },
                    rest,
                ));
            }
            Ok((Node::Var { name: name.into() }, rest))
        }
        '@' => {
            let rest = s
                .strip_prefix("@json")
                .ok_or_else(|| perr("expected `@json`"))?;
            let (v, tail) = take_json(rest.trim_start())?;
            let node: Node = serde_json::from_value(v)
                .map_err(|e| perr(&format!("invalid `@json` node: {e}")))?;
            Ok((node, tail))
        }
        '"' | '[' | '{' => {
            let (v, rest) = take_json(s)?;
            Ok((Node::Lit { value: v }, rest))
        }
        c if c == '-' || c.is_ascii_digit() => {
            let (v, rest) = take_json(s)?;
            Ok((Node::Lit { value: v }, rest))
        }
        c if c.is_ascii_alphabetic() || c == '_' => {
            let (ident, rest) = take_while(s, is_op_char);
            let rest_trim = rest.trim_start();
            if let Some(args_str) = rest_trim.strip_prefix('(') {
                let (args, rest2) = parse_call_args(args_str)?;
                // `fmt("template")` is the `Fmt` node, not a call to an op named `fmt` (there is none).
                if ident == "fmt" {
                    return match args.as_slice() {
                        [Node::Lit {
                            value: serde_json::Value::String(t),
                        }] => Ok((
                            Node::Fmt {
                                template: t.clone(),
                            },
                            rest2,
                        )),
                        _ => Err(perr("fmt(...) takes a single string-literal template")),
                    };
                }
                Ok((
                    Node::Call {
                        op: ident.to_string(),
                        args,
                    },
                    rest2,
                ))
            } else {
                match ident {
                    "true" => Ok((lit_bool(true), rest)),
                    "false" => Ok((lit_bool(false), rest)),
                    "null" => Ok((
                        Node::Lit {
                            value: serde_json::Value::Null,
                        },
                        rest,
                    )),
                    _ => Err(perr(&format!("unexpected token: `{ident}`"))),
                }
            }
        }
        _ => Err(perr(&format!("unexpected character: `{first}`"))),
    }
}

fn lit_bool(b: bool) -> Node {
    Node::Lit {
        value: serde_json::Value::Bool(b),
    }
}

/// Parse the argument list of a paren-form call; `s` is the text just after `(`.
fn parse_call_args(s: &str) -> Result<(Vec<Node>, &str)> {
    let mut args = Vec::new();
    let mut s = s.trim_start();
    if let Some(r) = s.strip_prefix(')') {
        return Ok((args, r));
    }
    loop {
        let (node, rest) = parse_expr(s)?;
        args.push(node);
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix(',') {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix(')') {
            return Ok((args, r));
        }
        return Err(perr(&format!(
            "expected `,` or `)` in call arguments, got: `{rest}`"
        )));
    }
}

/// Parse a comma-separated argument list that runs to the end of the line (the `do <op> …` form).
fn parse_arg_list(s: &str) -> Result<Vec<Node>> {
    let mut args = Vec::new();
    let mut s = s.trim_start();
    loop {
        let (node, rest) = parse_expr(s)?;
        args.push(node);
        let rest = rest.trim_start();
        if rest.is_empty() {
            return Ok(args);
        }
        if let Some(r) = rest.strip_prefix(',') {
            s = r.trim_start();
            continue;
        }
        return Err(perr(&format!(
            "expected `,` between arguments, got: `{rest}`"
        )));
    }
}

/// Read a single JSON value from the front of `s` (used for literals, `purpose`, and `@json`).
fn take_json(s: &str) -> Result<(serde_json::Value, &str)> {
    let mut stream = serde_json::Deserializer::from_str(s).into_iter::<serde_json::Value>();
    match stream.next() {
        Some(Ok(v)) => {
            let off = stream.byte_offset();
            Ok((v, &s[off..]))
        }
        Some(Err(e)) => Err(perr(&format!("invalid JSON literal: {e}"))),
        None => Err(perr("expected a JSON value")),
    }
}

// ---------------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------------

/// Parse `-> $name` (the leading `->` already stripped) into a symbol.
fn parse_arrow_sym(after_arrow: &str, ctx: &str) -> Result<SymbolName> {
    let a = after_arrow.trim_start();
    let nm = a
        .strip_prefix('$')
        .ok_or_else(|| perr(&format!("`{ctx}` expects `$name` after `->`")))?;
    let (nm, tail) = take_while(nm, is_var_char);
    if nm.is_empty() || !tail.trim().is_empty() {
        return Err(perr(&format!("malformed `$name` after `->` in `{ctx}`")));
    }
    Ok(SymbolName::from(nm))
}

/// Parse a (possibly empty) comma-separated `$sym, $sym` list.
fn parse_sym_list(s: &str) -> Result<Vec<SymbolName>> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        let name = part
            .strip_prefix('$')
            .ok_or_else(|| perr(&format!("expected `$symbol`, got: `{part}`")))?;
        if name.is_empty() || !name.chars().all(is_var_char) {
            return Err(perr(&format!("invalid symbol: `{part}`")));
        }
        out.push(name.into());
    }
    Ok(out)
}

fn set_effect(node: Node, eff: FlowEffect) -> Result<Node> {
    match node {
        Node::Bind {
            name, value, ty, ..
        } => Ok(Node::Bind {
            name,
            value,
            ty,
            effect: Some(eff),
        }),
        Node::Memo {
            name, value, ty, ..
        } => Ok(Node::Memo {
            name,
            value,
            ty,
            effect: Some(eff),
        }),
        _ => Err(perr("`@effect` can only annotate a bind")),
    }
}

fn effect_from_tag(tag: &str) -> Option<FlowEffect> {
    Some(match tag {
        "pure" => FlowEffect::Pure,
        "read" => FlowEffect::Read,
        "model" => FlowEffect::Model,
        "network" => FlowEffect::Network,
        "write_file" => FlowEffect::WriteFile,
        "write_db" => FlowEffect::WriteDb,
        "send_external" => FlowEffect::SendExternal,
        "delete" => FlowEffect::Delete,
        "money" => FlowEffect::Money,
        "calendar" => FlowEffect::Calendar,
        "human_visible" => FlowEffect::HumanVisible,
        _ => return None,
    })
}

// ===========================================================================
// Tests — the correctness gate: parse(format(ast)) == ast for every ast.
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Branch, FallbackBranch, MatchCase, RouteCase, Selector, ThingKind, ThingRef};
    use crate::format::format;

    /// The headline invariant. Every curated AST round-trips exactly through the text surface.
    fn assert_round_trips(ast: &DraftAst) {
        let text = format(ast);
        match parse(&text) {
            Ok(back) => assert_eq!(&back, ast, "round-trip mismatch.\n--- text ---\n{text}"),
            Err(e) => panic!("parse failed: {e}\n--- text ---\n{text}"),
        }
    }

    fn lit(v: serde_json::Value) -> Node {
        Node::Lit { value: v }
    }
    fn var(name: &str) -> Node {
        Node::Var { name: name.into() }
    }
    fn call(op: &str, args: Vec<Node>) -> Node {
        Node::Call {
            op: op.into(),
            args,
        }
    }
    fn bind(name: &str, value: Node) -> Node {
        Node::Bind {
            name: name.into(),
            value: Box::new(value),
            ty: None,
            effect: None,
        }
    }
    fn jq(path: &str, input: Node) -> Node {
        Node::Jq {
            path: path.into(),
            input: Box::new(input),
        }
    }
    fn s(v: &str) -> serde_json::Value {
        serde_json::Value::String(v.into())
    }

    // ---- P6: new native text forms ----

    #[test]
    fn relaxed_bind_forms_are_native_and_round_trip() {
        // The bind grammar now accepts `$b = $a` (var alias) and `$x = <literal>` directly. The text
        // surface already produced these shapes; this pins that they stay native (no `@json`) and exact.
        let ast = DraftAst {
            body: vec![
                bind("b", var("a")),
                bind("n", lit(serde_json::json!(5))),
                bind("greeting", lit(s("hi"))),
                bind("xs", lit(serde_json::json!([1, 2, 3]))),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("$b = $a"), "var alias: {text}");
        assert!(text.contains("$n = 5"), "number lit: {text}");
        assert!(!text.contains("@json"), "no json fallback: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn obj_and_list_templates_round_trip_via_json_escape() {
        // `obj`/`list` have no native `{k:expr}` spelling yet (that's a Phase-2 item), so they round-trip
        // through the `@json` escape — the invariant `parse(format(ast)) == ast` must still hold.
        let template: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj",
            "fields": {
                "ok": {"kind": "lit", "value": true},
                "items": {"kind": "list", "items": [{"kind": "var", "name": "a"}]}
            }
        }))
        .unwrap();
        let ast = DraftAst {
            body: vec![bind("r", template)],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            text.contains("@json"),
            "templates use the json escape today: {text}"
        );
        assert_round_trips(&ast);
    }

    #[test]
    fn field_access_sugar_round_trips_and_is_native() {
        // `$plan.kind` <-> jq(".kind", $plan); nested `$o.a.b` too.
        let ast = DraftAst {
            body: vec![
                bind("k", jq(".kind", var("plan"))),
                bind("d", jq(".a.b", var("o"))),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("$k = $plan.kind"), "field sugar: {text}");
        assert!(text.contains("$d = $o.a.b"), "nested field sugar: {text}");
        assert!(!text.contains("@json"), "no json fallback: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn fmt_inline_round_trips_and_is_native() {
        let ast = DraftAst {
            body: vec![bind(
                "msg",
                Node::Fmt {
                    template: "hi {name}".into(),
                },
            )],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            text.contains(r#"$msg = fmt("hi {name}")"#),
            "fmt inline: {text}"
        );
        assert!(!text.contains("@json"), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn jq_falls_back_to_json_when_unspellable() {
        // Non-Var input → @json (still round-trips).
        let over_call = DraftAst {
            body: vec![bind("y", jq(".kind", call("get_plan", vec![])))],
            ..Default::default()
        };
        assert!(format(&over_call).contains("@json"), "non-var jq → json");
        assert_round_trips(&over_call);
        // Bracket path → @json (the `$var.path` surface only spells simple dotted paths).
        let bracket = DraftAst {
            body: vec![bind("z", jq(".items[0]", var("o")))],
            ..Default::default()
        };
        assert!(format(&bracket).contains("@json"), "bracket path → json");
        assert_round_trips(&bracket);
    }

    #[test]
    fn match_and_route_round_trip_natively() {
        let m = DraftAst {
            body: vec![Node::Match {
                subject: Box::new(jq(".kind", var("plan"))),
                cases: vec![
                    MatchCase {
                        value: lit(s("chat")),
                        body: vec![bind("a", jq(".text", var("plan")))],
                    },
                    MatchCase {
                        value: lit(s("error")),
                        body: vec![call("echo", vec![lit(s("err"))])],
                    },
                ],
                default: vec![bind("r", call("run_plan", vec![var("plan")]))],
            }],
            ..Default::default()
        };
        let text = format(&m);
        assert!(text.contains("match $plan.kind"), "{text}");
        assert!(text.contains("case \"chat\""), "{text}");
        assert!(text.contains("default"), "{text}");
        assert!(!text.contains("@json"), "match native: {text}");
        assert_round_trips(&m);

        let r = DraftAst {
            body: vec![Node::Route {
                selector: Box::new(call("classify", vec![var("x")])),
                cases: vec![
                    RouteCase {
                        label: "bug".into(),
                        body: vec![call("echo", vec![lit(s("b"))])],
                    },
                    RouteCase {
                        label: "feat".into(),
                        body: vec![call("echo", vec![lit(s("f"))])],
                    },
                ],
                default: vec![call("echo", vec![lit(s("x"))])],
            }],
            ..Default::default()
        };
        let rt = format(&r);
        assert!(rt.contains("route classify($x)"), "{rt}");
        assert!(rt.contains("case \"bug\""), "{rt}");
        assert!(!rt.contains("@json"), "route native: {rt}");
        assert_round_trips(&r);
    }

    #[test]
    fn fallback_loop_timeout_budget_round_trip_natively() {
        let ast = DraftAst {
            body: vec![
                Node::Fallback {
                    branches: vec![
                        FallbackBranch {
                            body: vec![call("a", vec![])],
                        },
                        FallbackBranch {
                            body: vec![call("b", vec![])],
                        },
                    ],
                    bind: Some("win".into()),
                },
                Node::Loop {
                    for_ms: 1000,
                    every_ms: 100,
                    until: Some(Box::new(var("done"))),
                    body: vec![call("poll", vec![])],
                    bind: Some("ticks".into()),
                },
                Node::Timeout {
                    ms: 5000,
                    body: vec![call("slow", vec![])],
                    bind: None,
                },
                Node::Budget {
                    limit: 10,
                    body: vec![call("spend", vec![])],
                    bind: Some("used".into()),
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("fallback -> $win"), "{text}");
        assert!(text.contains("branch"), "{text}");
        assert!(text.contains("loop for 1000 every 100 -> $ticks"), "{text}");
        assert!(text.contains("until $done"), "{text}");
        assert!(text.contains("timeout 5000"), "{text}");
        assert!(text.contains("budget 10 -> $used"), "{text}");
        assert!(!text.contains("@json"), "all native: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn empty_flow_round_trips() {
        assert_round_trips(&DraftAst::default());
        assert_round_trips(&DraftAst {
            name: Some("noop".into()),
            ..Default::default()
        });
    }

    #[test]
    fn header_with_params_returns_round_trips() {
        assert_round_trips(&DraftAst {
            name: Some("route-call".into()),
            params: vec![
                Param {
                    name: "utterance".into(),
                    ty: TypeRef::String,
                },
                Param {
                    name: "count".into(),
                    ty: TypeRef::Number,
                },
                Param {
                    name: "tickets".into(),
                    ty: TypeRef::List(Box::new(TypeRef::Named("Ticket".into()))),
                },
            ],
            returns: Some(TypeRef::Named("RouteResult".into())),
            body: vec![Node::Return {
                value: Box::new(var("utterance")),
            }],
        });
        // Anonymous flow with params only.
        assert_round_trips(&DraftAst {
            name: None,
            params: vec![Param {
                name: "x".into(),
                ty: TypeRef::Bool,
            }],
            returns: Some(TypeRef::Any),
            body: Vec::new(),
        });
    }

    #[test]
    fn binds_calls_vars_lits_returns_round_trip() {
        assert_round_trips(&DraftAst {
            body: vec![
                // plain bind of a call
                Node::Bind {
                    name: "content".into(),
                    value: Box::new(call("read", vec![lit(serde_json::json!("README.md"))])),
                    ty: None,
                    effect: None,
                },
                // typed bind
                Node::Bind {
                    name: "draft".into(),
                    value: Box::new(var("content")),
                    ty: Some(TypeRef::Named("Draft".into())),
                    effect: None,
                },
                // bind with effect + type (annotation line)
                Node::Bind {
                    name: "sent".into(),
                    value: Box::new(call("email.send", vec![var("draft")])),
                    ty: Some(TypeRef::Bool),
                    effect: Some(FlowEffect::SendExternal),
                },
                // bare call statement (do form)
                call("git_stage", vec![lit(serde_json::json!(["."]))]),
                // bare var + bare literal statements
                var("content"),
                lit(serde_json::json!({"k": [1.0, true, null], "s": "v"})),
                // return
                Node::Return {
                    value: Box::new(lit(serde_json::json!("done"))),
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn control_flow_round_trips() {
        assert_round_trips(&DraftAst {
            body: vec![
                Node::When {
                    cond: Box::new(call("ready", vec![var("url")])),
                    then: vec![call("bash", vec![lit(serde_json::json!("echo yes"))])],
                    otherwise: vec![call("bash", vec![lit(serde_json::json!("echo no"))])],
                },
                // when with empty then but a populated else
                Node::When {
                    cond: Box::new(var("flag")),
                    then: Vec::new(),
                    otherwise: vec![Node::Return {
                        value: Box::new(lit(serde_json::json!(false))),
                    }],
                },
                Node::Unless {
                    cond: Box::new(var("already_built")),
                    body: vec![call("bash", vec![lit(serde_json::json!("cargo build"))])],
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn loops_and_seq_round_trip() {
        assert_round_trips(&DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(var("files")),
                    item: "f".into(),
                    body: vec![Node::Bind {
                        name: "text".into(),
                        value: Box::new(call("read", vec![var("f")])),
                        ty: None,
                        effect: None,
                    }],
                    collect: Some("contents".into()),
                    flat: false,
                },
                Node::Each {
                    source: Box::new(var("dirs")),
                    item: "d".into(),
                    body: vec![call("glob", vec![var("d")])],
                    collect: Some("all".into()),
                    flat: true,
                },
                Node::Each {
                    source: Box::new(lit(serde_json::json!(["a", "b"]))),
                    item: "x".into(),
                    body: Vec::new(),
                    collect: None,
                    flat: false,
                },
                Node::Repeat {
                    max: 10,
                    until: Some(Box::new(var("done"))),
                    body: vec![Node::Bind {
                        name: "done".into(),
                        value: Box::new(call("poll", Vec::new())),
                        ty: None,
                        effect: None,
                    }],
                    collect: Some("rounds".into()),
                },
                Node::Repeat {
                    max: 3,
                    until: None,
                    body: vec![call("tick", Vec::new())],
                    collect: None,
                },
                Node::Seq {
                    body: vec![call("a", Vec::new()), call("b", Vec::new())],
                    bind: Some("result".into()),
                },
                Node::Seq {
                    body: Vec::new(),
                    bind: None,
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn ctx_and_ctx_append_round_trip() {
        assert_round_trips(&DraftAst {
            body: vec![
                Node::Ctx {
                    name: "pack".into(),
                    purpose: Some("review the diff".into()),
                    include: vec!["diff".into(), "summary".into()],
                    exclude: vec!["secrets".into()],
                    budget: Some(4000),
                },
                Node::CtxAppend {
                    ctx: "pack".into(),
                    add: vec!["extra".into(), "notes".into()],
                },
                // minimal ctx (no attributes) + empty append
                Node::Ctx {
                    name: "bare".into(),
                    purpose: None,
                    include: Vec::new(),
                    exclude: Vec::new(),
                    budget: None,
                },
                Node::CtxAppend {
                    ctx: "bare".into(),
                    add: Vec::new(),
                },
            ],
            ..Default::default()
        });
    }

    #[test]
    fn nested_blocks_round_trip() {
        assert_round_trips(&DraftAst {
            name: Some("nested".into()),
            body: vec![Node::Each {
                source: Box::new(var("items")),
                item: "it".into(),
                body: vec![Node::When {
                    cond: Box::new(var("it")),
                    then: vec![Node::When {
                        cond: Box::new(var("inner")),
                        then: vec![call("bash", vec![lit(serde_json::json!("both"))])],
                        otherwise: vec![call("bash", vec![lit(serde_json::json!("only outer"))])],
                    }],
                    otherwise: vec![Node::Seq {
                        body: vec![call("cleanup", Vec::new())],
                        bind: None,
                    }],
                }],
                collect: None,
                flat: false,
            }],
            ..Default::default()
        });
    }

    #[test]
    fn json_fallback_round_trips_statement_and_inline() {
        assert_round_trips(&DraftAst {
            body: vec![
                // Unsupported node as a statement -> @json line.
                Node::Assert {
                    cond: Box::new(var("hits")),
                    message: Some("no results".into()),
                },
                Node::Parallel {
                    branches: vec![Branch {
                        name: "left".into(),
                        body: vec![call("read", vec![lit(serde_json::json!("l"))])],
                    }],
                },
                // Unsupported node inline (as a bind value) -> @json escape.
                Node::Bind {
                    name: "price".into(),
                    value: Box::new(Node::Jq {
                        path: ".bitcoin.usd".into(),
                        input: Box::new(var("raw")),
                    }),
                    ty: None,
                    effect: None,
                },
                // A thing reference (unsupported) inline as a call arg.
                Node::Bind {
                    name: "p".into(),
                    value: Box::new(call(
                        "notify",
                        vec![Node::Thing {
                            thing: ThingRef {
                                kind: ThingKind::Person,
                                selector: Selector::Name("john".into()),
                            },
                        }],
                    )),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        });
    }

    // ----- Hand-written text fixtures: pin the surface (not just self-consistency) -----

    #[test]
    fn fixture_basic_flow() {
        let src = "\
flow greet(name: String) -> String
  # bind then return
  $msg = greet_op($name)
  return $msg
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast,
            DraftAst {
                name: Some("greet".into()),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::String,
                }],
                returns: Some(TypeRef::String),
                body: vec![
                    Node::Bind {
                        name: "msg".into(),
                        value: Box::new(call("greet_op", vec![var("name")])),
                        ty: None,
                        effect: None,
                    },
                    Node::Return {
                        value: Box::new(var("msg")),
                    },
                ],
            }
        );
    }

    #[test]
    fn fixture_paren_form_call_and_when_else() {
        // A bare call written in paren form (not `do`) must parse to the same Call node.
        let src = "\
flow check
  read(\"log.txt\")
  when $ok
    do bash \"echo yes\"
  else
    do bash \"echo no\"
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![
                call("read", vec![lit(serde_json::json!("log.txt"))]),
                Node::When {
                    cond: Box::new(var("ok")),
                    then: vec![call("bash", vec![lit(serde_json::json!("echo yes"))])],
                    otherwise: vec![call("bash", vec![lit(serde_json::json!("echo no"))])],
                },
            ]
        );
    }

    #[test]
    fn fixture_goal_line_is_tolerated_and_ignored() {
        let src = "\
flow withgoal
goal \"do the thing\"
  return true
";
        let ast = parse(src).unwrap();
        assert_eq!(ast.name.as_deref(), Some("withgoal"));
        assert_eq!(
            ast.body,
            vec![Node::Return {
                value: Box::new(lit(serde_json::json!(true))),
            }]
        );
    }

    #[test]
    fn fixture_ctx_block() {
        let src = "\
flow pack-it
  ctx $pack
    purpose \"review\"
    budget 1500
    include $a, $b
    exclude $c
  $pack += $d
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![
                Node::Ctx {
                    name: "pack".into(),
                    purpose: Some("review".into()),
                    include: vec!["a".into(), "b".into()],
                    exclude: vec!["c".into()],
                    budget: Some(1500),
                },
                Node::CtxAppend {
                    ctx: "pack".into(),
                    add: vec!["d".into()],
                },
            ]
        );
    }

    // ----- Totality: malformed input errors, never panics -----

    #[test]
    fn malformed_input_returns_parse_error() {
        for bad in [
            "",
            "not a flow",
            "flow x\n\telse",            // tab indentation
            "flow x\n  $ = 1",           // empty symbol
            "flow x\n  $a = ",           // missing expression
            "flow x\n  do",              // do without op
            "flow x\n  each $f $xs",     // each without `in`
            "flow x\n  repeat\n",        // repeat without count
            "flow x\n  when",            // when without condition
            "flow x\n  $a = read(\"x\"", // unbalanced parens
            "flow x\n  else",            // dangling else
            "flow x\n  @json {oops}",    // invalid json
        ] {
            assert!(parse(bad).is_err(), "expected Err for: {bad:?}");
        }
    }

    /// `take_json` consumes exactly one value and reports the remainder (the inline-args case).
    #[test]
    fn take_json_leaves_remainder() {
        let (v, rest) = take_json("\"hi\", $x").unwrap();
        assert_eq!(v, serde_json::json!("hi"));
        assert_eq!(rest, ", $x");
        let (v, rest) = take_json("[1, 2])").unwrap();
        assert_eq!(v, serde_json::json!([1, 2]));
        assert_eq!(rest, ")");
    }
}

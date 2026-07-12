//! `parse` — read canonical Flux-Lang **text** back into a [`DraftAst`]. The round-trip partner of
//! [`crate::format`]: `parse(&format(&ast)) == ast` for every `DraftAst` (the supported subset natively,
//! everything else via the `@json` escape; the flow *header* is the one documented exception — see
//! [`crate::format`]'s "flow-header exception"). Hand-written, indentation-sensitive recursive descent.
//!
//! It is **total**: malformed input returns [`FlowError::Parse`], never a panic. Errors carry a
//! `line N:` prefix (1-based source line, counting comment/blank lines) wherever a line is in scope,
//! so the model repair loop can point at the offending statement.
//!
//! # Declared vs. referenced names
//! A *declared* symbol (a bind target, `each` item, `-> $bind` arrow target, `ctx` name, sym-list
//! entry, `parallel` branch name) is a plain identifier: ASCII alphanumerics and `_` only. `.` is
//! deliberately **not** a declared-name character — in expression position `$a.b` is field-access
//! sugar for `jq(".b", $a)`, so a dotted *declared* name would silently change meaning through a
//! format→parse cycle (`$a.b = 1` is a parse error, not a bind named `a.b`).
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
use crate::program::{
    AgentDecl, ChannelDecl, CompositeLimits, CompositeOpDecl, CompositeOpMeta, DatasourceDecl,
    JourneyDecl, Module, PermissionDecl, Program, TriggerDecl,
};
use flux_spec::{Effect, Idempotency, Risk};
use std::collections::BTreeMap;

/// Parse a single Flux-Lang flow from text into a [`DraftAst`].
///
/// This is the AST-only entry: one pass of the proven line machinery, no CST. The CST front-end
/// (L-59) lives in [`crate::lower_cst`] — [`crate::lower_cst::parse_with_ranges`] runs BOTH
/// parsers and adds the analyzer range side-map; use it when ranges are needed (the LSP does).
/// CST/legacy acceptance agreement is enforced by the dedicated guards (the `cst_agreement`
/// corpus sweep and the round-trip property test), NOT by a per-parse assertion — an assertion
/// here would double every parse and turn grammar drift into a process abort on untrusted
/// (model-emitted) input in debug builds (review finding, 2026-07-09).
pub fn parse(src: &str) -> Result<DraftAst> {
    parse_flow_text(src)
}

/// The legacy line-machinery flow parser — the semantic authority the CST front-end lowers with.
pub(crate) fn parse_flow_text(src: &str) -> Result<DraftAst> {
    let lines = preprocess(src)?;
    if lines.is_empty() {
        return Err(perr("empty input: expected a `flow` header"));
    }
    if lines[0].indent != 0 {
        return Err(perr_at(
            lines[0].number,
            "the `flow` header must start at column 0",
        ));
    }
    let (name, params, returns) = parse_header(&lines[0].text).map_err(|e| err_at(&lines[0], e))?;

    // The body is every indented line. Top-level (column-0) lines after the header may only be a
    // tolerated-and-ignored `goal "…"` directive (there is no AST slot for it).
    let mut body_lines: Vec<Line> = Vec::new();
    for l in &lines[1..] {
        if l.indent == 0 {
            if is_goal_line(&l.text) {
                continue;
            }
            return Err(perr_at(
                l.number,
                &format!("unexpected top-level line: `{}`", l.text),
            ));
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

/// A parse error located at a 1-based source line.
fn perr_at(line: usize, msg: &str) -> FlowError {
    FlowError::Parse(format!("line {line}: {msg}"))
}

/// Attach `line`'s source position to a parse error — once. An error already carrying a `line N:`
/// prefix from a deeper (more precise) frame is left untouched, so the innermost statement wins.
fn err_at(line: &Line, e: FlowError) -> FlowError {
    match e {
        FlowError::Parse(msg) if !msg.starts_with("line ") => {
            FlowError::Parse(format!("line {}: {msg}", line.number))
        }
        other => other,
    }
}

fn is_goal_line(t: &str) -> bool {
    t == "goal" || t.starts_with("goal ")
}

// ---------------------------------------------------------------------------
// Program / module layer: native-text typed declarations
// ---------------------------------------------------------------------------

/// Whether `t` opens a `flow` header (`flow`, `flow <name>`, or `flow(`).
fn is_flow_header(t: &str) -> bool {
    t == "flow"
        || t.strip_prefix("flow")
            .is_some_and(|r| r.starts_with(char::is_whitespace) || r.starts_with('('))
}

/// Parse a `.flux` **module** from native flux-lang text: a multi-agent [`Program`] — any of the
/// `permissions`/`agent`/`channel`/`datasource`/`trigger`/`journey` declarations plus top-level
/// `flow`s — or, when the file is a lone `flow`, a bare [`Module::Flow`]. The backend of
/// [`crate::program::Module::parse_str`]; module declarations are pure data (the L6 hosts give them
/// runtime meaning), so this adds **no** new node kinds.
pub fn parse_program(src: &str) -> Result<Module> {
    // AST-only, single-pass — CST/legacy acceptance agreement is enforced by the dedicated test
    // guards, not per-parse (see `parse`). Module-level range maps are deferred (the analyzer
    // runs per flow).
    parse_program_text(src)
}

/// The legacy line-machinery module parser — see [`parse_flow_text`].
pub(crate) fn parse_program_text(src: &str) -> Result<Module> {
    let lines = preprocess(src)?;
    if lines.is_empty() {
        return Err(perr(
            "empty input: expected a `flow` header or module declarations",
        ));
    }
    let mut program = Program::default();
    let mut saw_module_decl = false;
    let mut saw_permissions = false;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if line.indent != 0 {
            return Err(perr_at(
                line.number,
                &format!("a declaration must start at column 0: `{}`", line.text),
            ));
        }
        let header = line.text.as_str();
        let region = child_region(&lines[i..], 0);
        let consumed = 1 + region.len();

        if let Some(rest) = kw(header, "permissions") {
            if !rest.trim().is_empty() {
                return Err(perr_at(
                    line.number,
                    "`permissions` is a singleton declaration and takes no name",
                ));
            }
            if saw_permissions {
                return Err(perr_at(
                    line.number,
                    "a program may declare `permissions` only once",
                ));
            }
            program.permissions = Some(parse_permission_decl(region).map_err(|e| err_at(line, e))?);
            saw_permissions = true;
            saw_module_decl = true;
        } else if let Some(rest) = kw(header, "agent") {
            program
                .agents
                .push(parse_agent_decl(rest, region).map_err(|e| err_at(line, e))?);
            saw_module_decl = true;
        } else if let Some(rest) = kw(header, "channel") {
            program
                .channels
                .push(parse_channel_decl(rest, region).map_err(|e| err_at(line, e))?);
            saw_module_decl = true;
        } else if let Some(rest) = kw(header, "datasource") {
            program
                .datasources
                .push(parse_datasource_decl(rest, region).map_err(|e| err_at(line, e))?);
            saw_module_decl = true;
        } else if let Some(rest) = kw(header, "trigger") {
            program
                .triggers
                .push(parse_trigger_decl(rest, region).map_err(|e| err_at(line, e))?);
            saw_module_decl = true;
        } else if let Some(rest) = kw(header, "journey") {
            program
                .journeys
                .push(parse_journey_decl(rest, region).map_err(|e| err_at(line, e))?);
            saw_module_decl = true;
        } else if let Some(rest) = kw(header, "op") {
            program
                .ops
                .push(parse_composite_op_decl(rest, region).map_err(|e| err_at(line, e))?);
            saw_module_decl = true;
        } else if is_flow_header(header) {
            program
                .flows
                .push(parse_flow_decl(header, region).map_err(|e| err_at(line, e))?);
        } else if is_goal_line(header) {
            // tolerated and ignored, mirroring `parse`
        } else {
            return Err(perr_at(
                line.number,
                &format!(
                    "unknown top-level declaration: `{header}` (expected permissions / agent / \
                     channel / datasource / trigger / journey / op / flow)"
                ),
            ));
        }
        i += consumed;
    }

    // A file with no module declarations and exactly one top-level flow is a bare flow.
    if !saw_module_decl && program.flows.len() == 1 {
        return Ok(Module::Flow(program.flows.pop().unwrap()));
    }
    Ok(Module::Program(program))
}

/// A flow declaration (header + indented body region) → a [`DraftAst`]. Shared by top-level `flow`
/// decls and a `journey`'s inline `flow` block; reuses the flow header + statement parsers verbatim.
fn parse_flow_decl(header: &str, region: &[Line]) -> Result<DraftAst> {
    let (name, params, returns) = parse_header(header)?;
    let (body, _) = parse_stmts(region, 0)?;
    Ok(DraftAst {
        name,
        params,
        returns,
        body,
    })
}

fn parse_op_header(t: &str) -> Result<(String, Vec<Param>, Option<TypeRef>)> {
    let rest = t.trim();
    let (name, rest) = take_while(rest, is_name_char);
    if name.is_empty() {
        return Err(perr("`op` needs a name"));
    }
    let rest = rest.trim_start();
    let (params, rest) = if rest.starts_with('(') {
        let close = rest
            .find(')')
            .ok_or_else(|| perr("unterminated op parameter list"))?;
        let inner = &rest[1..close];
        (parse_params(inner)?, rest[close + 1..].trim_start())
    } else {
        (Vec::new(), rest)
    };
    let returns = if let Some(r) = rest.strip_prefix("->") {
        let ty = r.trim();
        if ty.is_empty() {
            return Err(perr("expected an op return type after `->`"));
        }
        Some(parse_type(ty))
    } else if rest.is_empty() {
        None
    } else {
        return Err(perr(&format!("unexpected text in op header: `{rest}`")));
    };
    Ok((name.to_string(), params, returns))
}

fn parse_composite_op_decl(name_str: &str, region: &[Line]) -> Result<CompositeOpDecl> {
    let (name, params, returns) = parse_op_header(name_str)?;
    let mut meta = CompositeOpMeta::default();
    if region.is_empty() {
        return Ok(CompositeOpDecl {
            name,
            params,
            returns,
            meta,
            body: DraftAst::default(),
        });
    }

    let block_indent = region[0].indent;
    let mut body_start = 0;
    while body_start < region.len() {
        let line = &region[body_start];
        if line.indent != block_indent {
            return Err(perr_at(
                line.number,
                &format!("unexpected indentation in op `{name}`: `{}`", line.text),
            ));
        }
        let (key, rest) = take_while(&line.text, is_name_char);
        if !is_composite_meta_key(key) {
            break;
        }
        parse_composite_meta_line(&mut meta, key, rest.trim_start())
            .map_err(|e| err_at(line, e))?;
        body_start += 1;
    }

    let (body, _) = parse_stmts(&region[body_start..], 0)?;
    Ok(CompositeOpDecl {
        name: name.clone(),
        params: params.clone(),
        returns: returns.clone(),
        meta,
        body: DraftAst {
            name: Some(name),
            params,
            returns,
            body,
        },
    })
}

fn is_composite_meta_key(key: &str) -> bool {
    matches!(
        key,
        "description" | "risk" | "idempotency" | "effects" | "limits" | "expose" | "view"
    )
}

fn parse_composite_meta_line(meta: &mut CompositeOpMeta, key: &str, value: &str) -> Result<()> {
    match key {
        "description" => meta.description = string_value(value, "description")?,
        "risk" => meta.risk = parse_risk(&string_value(value, "risk")?)?,
        "idempotency" => {
            meta.idempotency = parse_idempotency(&string_value(value, "idempotency")?)?
        }
        "effects" => meta.effects = parse_effects(&parse_setting(value)?)?,
        "limits" => meta.limits = parse_limits(&parse_setting(value)?)?,
        "expose" => {
            meta.expose = parse_setting(value)?
                .as_bool()
                .ok_or_else(|| perr("`expose` must be a boolean"))?
        }
        "view" => meta.view = Some(string_value(value, "view")?),
        _ => {}
    }
    Ok(())
}

/// The single-identifier name after a decl keyword (e.g. `assistant` in `agent assistant`).
fn decl_name(s: &str, kind: &str) -> Result<String> {
    let name = s.trim();
    let (tok, rest) = take_while(name, is_name_char);
    if tok.is_empty() || !rest.trim().is_empty() {
        return Err(perr(&format!(
            "`{kind}` name must be a single identifier, got: `{name}`"
        )));
    }
    Ok(tok.to_string())
}

/// The `key value` attribute lines of a flat decl body — all at one indentation level (no nested
/// blocks). Returns each key paired with the rest of its line (the value text).
fn attr_lines(region: &[Line]) -> Result<Vec<(String, &str)>> {
    let mut out = Vec::new();
    if region.is_empty() {
        return Ok(out);
    }
    let indent = region[0].indent;
    for l in region {
        if l.indent != indent {
            return Err(perr_at(
                l.number,
                &format!("unexpected indentation in declaration body: `{}`", l.text),
            ));
        }
        let (key, rest) = take_while(&l.text, is_name_char);
        if key.is_empty() {
            return Err(perr_at(
                l.number,
                &format!("expected a `key value` attribute, got: `{}`", l.text),
            ));
        }
        out.push((key.to_string(), rest.trim_start()));
    }
    Ok(out)
}

fn parse_agent_decl(name_str: &str, region: &[Line]) -> Result<AgentDecl> {
    let name = decl_name(name_str, "agent")?;
    let mut decl = AgentDecl {
        name,
        ..Default::default()
    };
    let mut settings = serde_json::Map::new();
    let mut allow: Option<Vec<String>> = None;
    let mut saw_allow = false;
    let mut deny: Option<Vec<String>> = None;
    for (key, val) in attr_lines(region)? {
        match key.as_str() {
            "model" => decl.model = Some(string_value(val, "model")?),
            "tools" => decl.tools = as_string_list(&parse_setting(val)?, "tools")?,
            "datasources" => {
                decl.datasources = as_string_list(&parse_setting(val)?, "datasources")?
            }
            "description" => decl.description = Some(string_value(val, "description")?),
            "allow" => {
                if saw_allow {
                    return Err(perr("duplicate agent attribute `allow`"));
                }
                allow = Some(as_string_list(&parse_setting(val)?, "allow")?);
                saw_allow = true;
            }
            "deny" => {
                if deny.is_some() {
                    return Err(perr("duplicate agent attribute `deny`"));
                }
                deny = Some(as_string_list(&parse_setting(val)?, "deny")?);
            }
            _ => {
                settings.insert(key, parse_setting(val)?);
            }
        }
    }
    if saw_allow || deny.is_some() {
        decl.permissions = Some(PermissionDecl {
            allow,
            deny: deny.unwrap_or_default(),
        });
    }
    if !settings.is_empty() {
        decl.settings = serde_json::Value::Object(settings);
    }
    Ok(decl)
}

fn parse_permission_decl(region: &[Line]) -> Result<PermissionDecl> {
    let mut allow: Option<Vec<String>> = None;
    let mut saw_allow = false;
    let mut deny: Option<Vec<String>> = None;
    for (key, val) in attr_lines(region)? {
        match key.as_str() {
            "allow" => {
                if saw_allow {
                    return Err(perr("duplicate permissions attribute `allow`"));
                }
                allow = Some(as_string_list(&parse_setting(val)?, "allow")?);
                saw_allow = true;
            }
            "deny" => {
                if deny.is_some() {
                    return Err(perr("duplicate permissions attribute `deny`"));
                }
                deny = Some(as_string_list(&parse_setting(val)?, "deny")?);
            }
            other => return Err(perr(&format!("unknown permissions attribute `{other}`"))),
        }
    }
    Ok(PermissionDecl {
        allow,
        deny: deny.unwrap_or_default(),
    })
}

fn parse_channel_decl(name_str: &str, region: &[Line]) -> Result<ChannelDecl> {
    let name = decl_name(name_str, "channel")?;
    let mut kind: Option<String> = None;
    let mut settings = serde_json::Map::new();
    for (key, val) in attr_lines(region)? {
        match key.as_str() {
            "kind" => kind = Some(string_value(val, "kind")?),
            _ => {
                settings.insert(key, parse_setting(val)?);
            }
        }
    }
    Ok(ChannelDecl {
        kind: kind.unwrap_or_else(|| name.clone()), // `kind` defaults to the decl name
        name,
        settings: settings_value(settings),
    })
}

fn parse_datasource_decl(name_str: &str, region: &[Line]) -> Result<DatasourceDecl> {
    let name = decl_name(name_str, "datasource")?;
    let mut kind: Option<String> = None;
    let mut path: Option<String> = None;
    let mut settings = serde_json::Map::new();
    for (key, val) in attr_lines(region)? {
        match key.as_str() {
            "kind" => kind = Some(string_value(val, "kind")?),
            "path" => path = Some(string_value(val, "path")?),
            _ => {
                settings.insert(key, parse_setting(val)?);
            }
        }
    }
    Ok(DatasourceDecl {
        kind: kind.unwrap_or_else(|| name.clone()),
        name,
        path,
        settings: settings_value(settings),
    })
}

fn parse_trigger_decl(name_str: &str, region: &[Line]) -> Result<TriggerDecl> {
    let name = decl_name(name_str, "trigger")?;
    let mut on: Option<String> = None;
    let mut run: Option<String> = None;
    let mut agent: Option<String> = None;
    for (key, val) in attr_lines(region)? {
        match key.as_str() {
            "on" => on = Some(string_value(val, "on")?),
            "run" => run = Some(string_value(val, "run")?),
            "agent" => agent = Some(string_value(val, "agent")?),
            other => return Err(perr(&format!("unknown trigger attribute `{other}`"))),
        }
    }
    // A trigger routes to an `agent` turn or a `run` journey — require exactly the field the runtime
    // will use. An agent-bound trigger doesn't need a journey name (the model drives the turn); a
    // trigger with neither has nothing to run.
    let run = run.unwrap_or_default();
    if agent.is_none() && run.is_empty() {
        return Err(perr(
            "a `trigger` needs a `run` journey/flow name or an `agent` to run",
        ));
    }
    Ok(TriggerDecl {
        name,
        on: on.ok_or_else(|| perr("a `trigger` needs an `on` event label"))?,
        run,
        agent,
    })
}

fn parse_journey_decl(name_str: &str, region: &[Line]) -> Result<JourneyDecl> {
    let name = decl_name(name_str, "journey")?;
    if region.is_empty() {
        return Err(perr(&format!("journey `{name}` needs a `flow` body")));
    }
    let block_indent = region[0].indent;
    let mut agent: Option<String> = None;
    let mut flow: Option<DraftAst> = None;
    let mut j = 0;
    while j < region.len() {
        let line = &region[j];
        if line.indent != block_indent {
            return Err(perr_at(
                line.number,
                &format!(
                    "unexpected indentation in journey `{name}`: `{}`",
                    line.text
                ),
            ));
        }
        let t = line.text.as_str();
        if let Some(rest) = kw(t, "agent") {
            agent = Some(string_value(rest, "agent").map_err(|e| err_at(line, e))?);
            j += 1;
        } else if is_flow_header(t) {
            let body = child_region(&region[j..], block_indent);
            let mut ast = parse_flow_decl(t, body).map_err(|e| err_at(line, e))?;
            if ast.name.is_none() {
                ast.name = Some(name.clone());
            }
            flow = Some(ast);
            j += 1 + body.len();
        } else {
            return Err(perr_at(
                line.number,
                &format!("unexpected line in journey `{name}`: `{t}`"),
            ));
        }
    }
    let flow = flow.ok_or_else(|| perr(&format!("journey `{name}` needs a `flow` body")))?;
    Ok(JourneyDecl { name, agent, flow })
}

/// An empty settings map renders as `Value::Null` (so it serializes away); a non-empty one as an object.
fn settings_value(map: serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    if map.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(map)
    }
}

/// Parse a full setting value, rejecting trailing text.
fn parse_setting(s: &str) -> Result<serde_json::Value> {
    let (v, rest) = parse_setting_value(s)?;
    if !rest.trim().is_empty() {
        return Err(perr(&format!(
            "trailing text after setting value: `{rest}`"
        )));
    }
    Ok(v)
}

/// A setting value coerced to a string (a quoted string or a bare identifier both work).
fn string_value(s: &str, what: &str) -> Result<String> {
    match parse_setting(s)? {
        serde_json::Value::String(s) => Ok(s),
        _ => Err(perr(&format!("`{what}` must be a string"))),
    }
}

fn as_string_list(v: &serde_json::Value, what: &str) -> Result<Vec<String>> {
    match v {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|i| match i {
                serde_json::Value::String(s) => Ok(s.clone()),
                _ => Err(perr(&format!("`{what}` must be a list of strings"))),
            })
            .collect(),
        _ => Err(perr(&format!("`{what}` must be a list of strings"))),
    }
}

fn parse_risk(s: &str) -> Result<Risk> {
    match normalize_token(s).as_str() {
        "low" => Ok(Risk::Low),
        "medium" => Ok(Risk::Medium),
        "high" => Ok(Risk::High),
        "destructive" => Ok(Risk::Destructive),
        other => Err(perr(&format!("unknown risk `{other}`"))),
    }
}

fn parse_idempotency(s: &str) -> Result<Idempotency> {
    match normalize_token(s).as_str() {
        "idempotent" => Ok(Idempotency::Idempotent),
        "non_idempotent" => Ok(Idempotency::NonIdempotent),
        "conditional" => Ok(Idempotency::Conditional),
        other => Err(perr(&format!("unknown idempotency `{other}`"))),
    }
}

fn parse_effects(v: &serde_json::Value) -> Result<Vec<Effect>> {
    let items = as_string_list(v, "effects")?;
    let mut out = Vec::new();
    for item in items {
        let effect = match normalize_token(&item).as_str() {
            "read" => Effect::Read,
            "write" => Effect::Write,
            "network" | "model" => Effect::Network,
            "process" => Effect::Process,
            "browser" => Effect::Browser,
            "filesystem" | "file_system" => Effect::Filesystem,
            "local_system" => Effect::LocalSystem,
            other => return Err(perr(&format!("unknown effect `{other}`"))),
        };
        if !out.contains(&effect) {
            out.push(effect);
        }
    }
    Ok(out)
}

fn parse_limits(v: &serde_json::Value) -> Result<CompositeLimits> {
    let serde_json::Value::Object(map) = v else {
        return Err(perr("`limits` must be an object"));
    };
    let mut limits = CompositeLimits::default();
    for (key, value) in map {
        let n = value
            .as_u64()
            .ok_or_else(|| perr(&format!("limit `{key}` must be a positive integer")))?;
        match key.as_str() {
            "dispatches" => limits.dispatches = Some(n),
            "timeout_ms" => limits.timeout_ms = Some(n),
            "context_chars" => limits.context_chars = Some(n),
            other => return Err(perr(&format!("unknown limit `{other}`"))),
        }
    }
    Ok(limits)
}

fn normalize_token(s: &str) -> String {
    s.trim().to_ascii_lowercase().replace('-', "_")
}

/// Evaluate a native-text **setting value** to a [`serde_json::Value`] (no IO): string / number /
/// `true|false|null`; `[ … ]` lists and `{ k: v }` records (bare identifiers coerce to strings, so
/// `tools [search, send]` works); and `secret "NAME"` → the reserved marker `{"$secret":"NAME"}`,
/// resolved later by the host (never inline plaintext). Returns the value and the unconsumed remainder.
fn parse_setting_value(s: &str) -> Result<(serde_json::Value, &str)> {
    let s = s.trim_start();
    let first = s
        .chars()
        .next()
        .ok_or_else(|| perr("expected a setting value"))?;
    match first {
        '"' => take_json(s),
        '-' | '0'..='9' => take_json(s),
        '[' => parse_setting_list(s),
        '{' => parse_setting_record(s),
        c if c.is_ascii_alphabetic() || c == '_' => {
            let (ident, rest) = take_while(s, is_name_char);
            match ident {
                "true" => Ok((serde_json::Value::Bool(true), rest)),
                "false" => Ok((serde_json::Value::Bool(false), rest)),
                "null" => Ok((serde_json::Value::Null, rest)),
                "secret" => parse_secret_ref(rest),
                _ => Ok((serde_json::Value::String(ident.to_string()), rest)),
            }
        }
        _ => Err(perr(&format!(
            "unexpected character in setting value: `{first}`"
        ))),
    }
}

/// `secret "NAME"` → `{"$secret":"NAME"}` (the env-var name to resolve at load; plaintext never inline).
fn parse_secret_ref(after_kw: &str) -> Result<(serde_json::Value, &str)> {
    let s = after_kw.trim_start();
    let (name_v, rest) = take_json(s).map_err(|_| {
        perr("`secret` expects a quoted env-var name, e.g. `secret \"SLACK_BOT_TOKEN\"`")
    })?;
    let name = match name_v {
        serde_json::Value::String(n) => n,
        _ => return Err(perr("`secret` expects a quoted env-var name")),
    };
    Ok((crate::program::secret_marker(&name), rest))
}

fn parse_setting_list(s: &str) -> Result<(serde_json::Value, &str)> {
    let mut s = s
        .strip_prefix('[')
        .ok_or_else(|| perr("expected `[`"))?
        .trim_start();
    let mut items = Vec::new();
    if let Some(r) = s.strip_prefix(']') {
        return Ok((serde_json::Value::Array(items), r));
    }
    loop {
        let (item, rest) = parse_setting_value(s)?;
        items.push(item);
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix(',') {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix(']') {
            return Ok((serde_json::Value::Array(items), r));
        }
        return Err(perr(&format!("expected `,` or `]` in list, got: `{rest}`")));
    }
}

fn parse_setting_record(s: &str) -> Result<(serde_json::Value, &str)> {
    let mut s = s
        .strip_prefix('{')
        .ok_or_else(|| perr("expected `{`"))?
        .trim_start();
    let mut map = serde_json::Map::new();
    if let Some(r) = s.strip_prefix('}') {
        return Ok((serde_json::Value::Object(map), r));
    }
    loop {
        let (key, rest) = parse_obj_key(s)?;
        let rest = rest
            .trim_start()
            .strip_prefix(':')
            .ok_or_else(|| perr(&format!("expected `:` after record key `{key}`")))?;
        let (val, rest) = parse_setting_value(rest)?;
        map.insert(key, val);
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix(',') {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix('}') {
            return Ok((serde_json::Value::Object(map), r));
        }
        return Err(perr(&format!(
            "expected `,` or `}}` in record, got: `{rest}`"
        )));
    }
}

// ---------------------------------------------------------------------------
// Lexing: logical lines (comment-stripped, blanks removed, indent measured)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Line {
    indent: usize,
    text: String,
    /// The 1-based source line this logical line came from (comment/blank lines still count), so
    /// parse errors can point back into the exact text the model emitted.
    number: usize,
}

/// Split source text into logical [`Line`]s: normally one physical line each, comment-stripped
/// (`#` outside a string) and indentation-measured — **except** a `"""…"""` multi-line string
/// literal (L-39), which is read verbatim (no comment-stripping, no escape processing) up to the
/// next literal `"""`, however many physical lines that spans, and spliced back in as a standard
/// escaped JSON string. This is a pure lexer-level desugaring: every later stage (`take_json`,
/// `parse_expr`, …) only ever sees ordinary escaped `"…"` strings, so a `"""` block works in
/// **every** position a JSON string can appear (bind value, call arg, object/array template leaf,
/// `@json` escape, …) with zero changes below this function. See `docs/syntax.md`'s "Multi-line
/// strings" section for the full grammar and its documented edge cases.
fn preprocess(src: &str) -> Result<Vec<Line>> {
    // Normalize CRLF so the char scanner below (which only special-cases `\n`) matches the old
    // `str::lines()`-based behavior on Windows-authored sources.
    let src = src.replace("\r\n", "\n");
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line_no = 1usize;
    while i < n {
        let this_line_no = line_no;
        let mut indent = 0usize;
        while i < n && chars[i] == ' ' {
            indent += 1;
            i += 1;
        }
        if i < n && chars[i] == '\t' {
            return Err(perr_at(
                this_line_no,
                "tabs are not allowed for indentation",
            ));
        }

        let mut text = String::new();
        let mut in_str = false;
        let mut esc = false;
        loop {
            if i >= n {
                break;
            }
            let c = chars[i];
            if c == '\n' {
                i += 1;
                line_no += 1;
                break;
            }
            if !in_str && c == '#' {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
                if i < n {
                    i += 1;
                    line_no += 1;
                }
                break;
            }
            if !in_str && c == '"' && i + 2 < n && chars[i + 1] == '"' && chars[i + 2] == '"' {
                // Opening `"""` delimiter: read verbatim to the next literal `"""`, across as many
                // physical lines as needed, then re-encode as a standard escaped JSON string so
                // every downstream parser stays unchanged.
                i += 3;
                let mut content = String::new();
                loop {
                    if i + 2 < n && chars[i] == '"' && chars[i + 1] == '"' && chars[i + 2] == '"' {
                        i += 3;
                        break;
                    }
                    if i >= n {
                        return Err(perr_at(
                            this_line_no,
                            "unterminated multi-line string: missing closing `\"\"\"`",
                        ));
                    }
                    if chars[i] == '\n' {
                        line_no += 1;
                    }
                    content.push(chars[i]);
                    i += 1;
                }
                let escaped =
                    serde_json::to_string(&content).unwrap_or_else(|_| "\"\"".to_string());
                text.push_str(&escaped);
                continue;
            }
            if !in_str && c == '"' {
                in_str = true;
                text.push(c);
                i += 1;
                continue;
            }
            if in_str {
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    in_str = false;
                }
            }
            text.push(c);
            i += 1;
        }

        let trimmed = text.trim();
        if !trimmed.is_empty() {
            out.push(Line {
                indent,
                text: trimmed.to_string(),
                number: this_line_no,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// A *declared*-symbol character (bind targets, `each` items, `-> $bind`, sym lists, `ctx` names,
/// `parallel` branch names): ASCII alphanumeric or `_` — the parser-side mirror of
/// [`SymbolName::is_identifier`]. Deliberately excludes `.` (see the module docs: dotted names are
/// field-access sugar in expression position, never declarable).
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A *referenced*-symbol character in expression position: identifier chars plus `.`, so `$a.b` is
/// read as one token and then split into the symbol + jq field path by [`parse_expr`].
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
            return Err(perr_at(
                lines[i].number,
                &format!("unexpected indentation at: `{}`", lines[i].text),
            ));
        }
        let (node, used) =
            parse_stmt(&lines[i..], block_indent).map_err(|e| err_at(&lines[i], e))?;
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
        let (inner, used) = parse_stmt(&lines[1..], indent).map_err(|e| err_at(&lines[1], e))?;
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
            "`until` is only valid as the first line of a `repeat`/`loop` body",
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
    if let Some(rest) = kw(t, "parallel") {
        return parse_parallel(rest, lines, indent);
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
    if let Some(rest) = kw(t, "with_tools") {
        return parse_with_tools(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "retry") {
        return parse_retry(rest, lines, indent);
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
    if let Some(rest) = kw(t, "assert") {
        return parse_assert(rest);
    }
    if let Some(rest) = kw(t, "memo") {
        return parse_memo(rest);
    }
    if let Some(rest) = kw(t, "once") {
        return parse_once(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "checkpoint") {
        return parse_checkpoint(rest);
    }
    if let Some(rest) = kw(t, "await") {
        return parse_await(rest);
    }
    if let Some(rest) = kw(t, "confirm") {
        return parse_confirm(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "throttle") {
        return parse_throttle(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "debounce") {
        return parse_debounce(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "verify") {
        return parse_verify(rest);
    }
    if let Some(rest) = kw(t, "try") {
        return parse_try(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "race") {
        return parse_race(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "scope") {
        return parse_scope(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "saga") {
        return parse_saga(rest, lines, indent);
    }
    if let Some(rest) = kw(t, "pipe") {
        return parse_pipe(rest, lines, indent);
    }

    if t.starts_with('$') {
        return parse_dollar(t);
    }

    // Otherwise the whole line is a single expression statement (e.g. a paren-form bare call, or a
    // bare `expr` like `$a + 1`). Parse it as a full expression so an operator formula round-trips.
    let node = parse_condition_expr(t, "expression statement")?;
    Ok((node, 1))
}

fn parse_when(cond_str: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let cond = parse_condition_expr(cond_str, "when condition")?;
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
    let cond = parse_condition_expr(cond_str, "unless condition")?;
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
    let (item, r) = take_while(item_part, is_ident_char);
    if item.is_empty() {
        return Err(perr("`each` has an empty item symbol"));
    }
    let r = kw(r.trim_start(), "in").ok_or_else(|| perr("`each` expects `in`"))?;
    // The source is a full expression up to an optional `-> $collect`; splitting at the top-level
    // arrow first lets an operator source (`each $x in $a + 1`) round-trip, not just a leaf.
    let (source_str, after) = match find_top_level_arrow(r) {
        Some(pos) => (&r[..pos], &r[pos..]),
        None => (r, ""),
    };
    let source = parse_condition_expr(source_str.trim(), "each source")?;
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
        let (nm, tail) = take_while(nm, is_ident_char);
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
    let (until, body_region) = split_until(region)?;
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
                let uexpr =
                    parse_condition_expr(u, "until condition").map_err(|e| err_at(first, e))?;
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
            return Err(perr_at(
                region[i].number,
                &format!(
                    "unexpected indentation in `{arm_kw}` arms: `{}`",
                    region[i].text
                ),
            ));
        }
        let t = region[i].text.as_str();
        let body_region = child_region(&region[i..], arm_indent);
        let (body, _) = parse_stmts(body_region, arm_indent)?;
        if t == "default" {
            default = body;
        } else if let Some(hdr) = kw(t, arm_kw) {
            arms.push((hdr.to_string(), body));
        } else {
            return Err(perr_at(
                region[i].number,
                &format!("expected `{arm_kw}` or `default`, got: `{t}`"),
            ));
        }
        i += 1 + body_region.len();
    }
    Ok((arms, default))
}

fn parse_match(subject_str: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    // Subject and case values are full expressions (like bind values) so a formatted operator
    // expr round-trips in either position.
    let subject = parse_condition_expr(subject_str, "match subject")?;
    let region = child_region(lines, indent);
    let (arms, default) = parse_arms(region, "case")?;
    let mut cases = Vec::with_capacity(arms.len());
    for (value_str, body) in arms {
        let value = parse_condition_expr(&value_str, "case value")?;
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
    // A full expression (like a bind value) so a formatted operator selector round-trips.
    let selector = parse_condition_expr(selector_str, "route selector")?;
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

/// `parallel` + indented `branch $name` arms (mirrors `fallback`'s `branch` arms, but each names the
/// symbol its branch result binds to). No `default` arm.
fn parse_parallel(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    if !rest.trim().is_empty() {
        return Err(perr(
            "`parallel` takes no header — put each concurrent path on its own `branch $name` arm",
        ));
    }
    let region = child_region(lines, indent);
    let (arms, default) = parse_arms(region, "branch")?;
    if !default.is_empty() {
        return Err(perr(
            "`parallel` has no `default` arm — use `branch $name` only",
        ));
    }
    let mut branches = Vec::with_capacity(arms.len());
    for (hdr, body) in arms {
        let name = hdr
            .trim()
            .strip_prefix('$')
            .ok_or_else(|| perr(&format!("`parallel` branch needs a `$name`, got: `{hdr}`")))?;
        let (nm, tail) = take_while(name, is_ident_char);
        if nm.is_empty() || !tail.trim().is_empty() {
            return Err(perr(&format!("invalid `parallel` branch name: `{hdr}`")));
        }
        branches.push(crate::ast::Branch {
            name: nm.into(),
            body,
        });
    }
    Ok((Node::Parallel { branches }, 1 + region.len()))
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

/// `retry <max> [backoff <ident>] [delay <ms>] [-> $bind]` + indented body. Space-keyword tokens in a
/// fixed order (mirrors `loop for <ms> every <ms>`); `backoff` is `none`/`linear`/`exponential`.
fn parse_retry(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (digits, mut r) = take_while(rest.trim_start(), |c| c.is_ascii_digit());
    let max: u32 = digits
        .parse()
        .map_err(|_| perr("`retry` expects a numeric max"))?;
    let mut backoff = None;
    if let Some(after) = kw(r.trim_start(), "backoff") {
        let (ident, rr) = take_while(after, is_name_char);
        if ident.is_empty() {
            return Err(perr(
                "`retry backoff` expects a name (none/linear/exponential)",
            ));
        }
        backoff = Some(ident.to_string());
        r = rr;
    }
    let mut delay_ms = None;
    if let Some(after) = kw(r.trim_start(), "delay") {
        let (ms, rr) = take_u64(after)?;
        delay_ms = Some(ms);
        r = rr;
    }
    let bind = parse_optional_arrow_bind(r, "retry")?;
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((
        Node::Retry {
            max,
            backoff,
            delay_ms,
            body,
            bind,
        },
        1 + region.len(),
    ))
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

/// `with_tools ["a", "b"] [-> $bind]` + indented body — the capability-scope block. The tool-name
/// list uses the same bracket-list literal grammar as a setting value (`parse_setting_list`).
fn parse_with_tools(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let (list, r) = parse_setting_list(rest)
        .map_err(|_| perr("`with_tools` expects a list of tool-name strings, e.g. `[\"read\"]`"))?;
    let tools = as_string_list(&list, "with_tools")?;
    let bind = parse_optional_arrow_bind(r, "with_tools")?;
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((Node::CapScope { tools, body, bind }, 1 + region.len()))
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
    let (name, tail) = take_while(nm, is_ident_char);
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
        let attr: Result<()> = (|| {
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
            Ok(())
        })();
        attr.map_err(|e| err_at(l, e))?;
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
        // A return value is a full expression (like a bind value), so `return $a + 1` /
        // `return len($xs) > 0` work — not just a single leaf. `parse_condition_expr` falls through
        // to native-`expr` parsing when a leaf parse leaves operator tokens.
        parse_condition_expr(rest, "return value")?
    };
    Ok((
        Node::Return {
            value: Box::new(value),
        },
        1,
    ))
}

/// `assert <cond> [, "<message>"]` — a one-line boolean guard. The condition is a full expression
/// (the first top-level `,` after it begins the optional message, so commas inside `op(a,b)`/`{…}`/
/// `[…]`/strings are already consumed by `parse_expr`).
fn parse_assert(rest: &str) -> Result<(Node, usize)> {
    // First, try to split by comma to separate condition from message
    let rest = rest.trim();
    let (cond_str, message) = if let Some(comma_pos) = find_top_level_comma(rest) {
        let cond_part = &rest[..comma_pos];
        let msg_part = rest[comma_pos + 1..].trim();
        let (v, rest2) = take_json(msg_part)?;
        if !rest2.trim().is_empty() {
            return Err(perr("trailing text after `assert` message"));
        }
        match v {
            serde_json::Value::String(m) => (cond_part, Some(m)),
            _ => return Err(perr("`assert` message must be a quoted string")),
        }
    } else {
        (rest, None)
    };

    // Now parse the condition with native-expr fallback
    let cond = parse_condition_expr(cond_str, "assert condition")?;

    Ok((
        Node::Assert {
            cond: Box::new(cond),
            message,
        },
        1,
    ))
}

/// Find the position of a top-level comma (not inside parens, brackets, braces, or strings).
fn find_top_level_comma(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut string_char = ' ';
    let mut chars = s.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if in_string {
            if c == '\\' {
                chars.next(); // Skip escaped character
            } else if c == string_char {
                in_string = false;
            }
        } else {
            match c {
                '"' | '\'' => {
                    in_string = true;
                    string_char = c;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => return Some(i),
                _ => {}
            }
        }
    }
    None
}

/// Byte offset of the first TOP-LEVEL `,` or closing `)`/`]`/`}` in `s` (or `s.len()` if none) — the
/// boundary of one argument / template-element expression. Commas/closers inside nested `(`/`[`/`{`
/// or strings don't count.
fn split_arg_end(s: &str) -> usize {
    let mut depth = 0;
    let mut in_string = false;
    let mut string_char = ' ';
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if in_string {
            if c == '\\' {
                chars.next();
            } else if c == string_char {
                in_string = false;
            }
        } else {
            match c {
                '"' | '\'' => {
                    in_string = true;
                    string_char = c;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => return i,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => return i,
                _ => {}
            }
        }
    }
    s.len()
}

/// Parse one full argument / template-element expression from the front of `s`, stopping at the
/// first top-level `,` or closing bracket (left in the returned rest). Unlike [`parse_expr`] (a
/// single leaf) this consumes operators, so a call arg or template value can be a full `expr`
/// (`op($a + 1, $b)`, `{ n: $a * 2 }`) and round-trip a formatted operator expression.
fn parse_delimited_expr(s: &str) -> Result<(Node, &str)> {
    let end = split_arg_end(s);
    let node = parse_condition_expr(s[..end].trim(), "argument")?;
    Ok((node, &s[end..]))
}

/// The byte offset of a top-level `->` (the `each … -> $collect` separator), outside strings and
/// brackets. `->` is not an `expr` operator, so it never occurs inside a valid source expression —
/// this lets an `each` source be parsed as a full expression up to the arrow.
fn find_top_level_arrow(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut string_char = ' ';
    let mut chars = s.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if in_string {
            if c == '\\' {
                chars.next();
            } else if c == string_char {
                in_string = false;
            }
        } else {
            match c {
                '"' | '\'' => {
                    in_string = true;
                    string_char = c;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                '-' if depth == 0 && matches!(chars.peek(), Some((_, '>'))) => return Some(i),
                _ => {}
            }
        }
    }
    None
}

/// A `$name`-led statement: a bare var, a ctx_append (`+=`), or a bind (`=` / `: T =`).
/// The name is a *declared*-symbol identifier — `$a.b = 1` is a parse error (in expression
/// position `$a.b` is field access on `$a`; a declared name can never contain `.`).
fn parse_dollar(t: &str) -> Result<(Node, usize)> {
    let (name, rest) = take_while(&t[1..], is_ident_char);
    if name.is_empty() {
        return Err(perr("empty symbol after `$`"));
    }
    if rest.starts_with('.') {
        return Err(perr(&format!(
            "`${name}{rest}`: a declared name cannot contain `.` — `$x.y` is field access on \
             `$x`, valid only in expression position"
        )));
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
        let value = parse_condition_expr(&r[eq + 1..], "bind value")?;
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
        let value = parse_condition_expr(r, "bind value")?;
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

/// `memo $x[: T] = <expr>` — a bind pinned across turns (an `@effect(tag)` on the line above is
/// re-attached by [`set_effect`], exactly like a plain bind).
fn parse_memo(rest: &str) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let nm = rest
        .strip_prefix('$')
        .ok_or_else(|| perr("`memo` expects `$name`"))?;
    let (name, after) = take_while(nm, is_ident_char);
    if name.is_empty() {
        return Err(perr("`memo` has an empty name"));
    }
    let after = after.trim_start();
    let (ty, rhs) = if let Some(a) = after.strip_prefix(':') {
        let a = a.trim_start();
        let eq = a
            .find('=')
            .ok_or_else(|| perr("expected `=` in typed `memo`"))?;
        (Some(parse_type(a[..eq].trim())), &a[eq + 1..])
    } else if let Some(a) = after.strip_prefix('=') {
        (None, a)
    } else {
        return Err(perr("expected `=` or `:` after `memo $name`"));
    };
    let value = parse_condition_expr(rhs, "memo value")?;
    Ok((
        Node::Memo {
            name: name.into(),
            value: Box::new(value),
            ty,
            effect: None,
        },
        1,
    ))
}

/// `once "label" [-> $bind]` + indented body — an at-most-once side effect.
fn parse_once(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (label, r) = take_string(rest.trim_start(), "once")?;
    let bind = parse_optional_arrow_bind(r, "once")?;
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((Node::Once { label, body, bind }, 1 + region.len()))
}

/// `checkpoint "label"` — a durable resume marker (top-level, no body).
fn parse_checkpoint(rest: &str) -> Result<(Node, usize)> {
    let (label, tail) = take_string(rest.trim_start(), "checkpoint")?;
    if !tail.trim().is_empty() {
        return Err(perr("trailing text after `checkpoint \"label\"`"));
    }
    Ok((Node::Checkpoint { label }, 1))
}

/// `await [$b[: T] =] "source"` — pause for an external event, optionally binding its payload.
fn parse_await(rest: &str) -> Result<(Node, usize)> {
    let rest = rest.trim_start();
    let (binding, as_type, src) = if let Some(r) = rest.strip_prefix('$') {
        let (nm, after) = take_while(r, is_ident_char);
        if nm.is_empty() {
            return Err(perr("`await` has an empty binding name"));
        }
        let after = after.trim_start();
        let (ty, rhs) = if let Some(a) = after.strip_prefix(':') {
            let a = a.trim_start();
            let eq = a
                .find('=')
                .ok_or_else(|| perr("expected `=` in typed `await`"))?;
            (Some(parse_type(a[..eq].trim())), &a[eq + 1..])
        } else if let Some(a) = after.strip_prefix('=') {
            (None, a)
        } else {
            return Err(perr("expected `=` after `await $name`"));
        };
        (Some(SymbolName::from(nm)), ty, rhs)
    } else {
        (None, None, rest)
    };
    let (source, tail) = take_string(src.trim_start(), "await")?;
    if !tail.trim().is_empty() {
        return Err(perr("trailing text after `await` source"));
    }
    Ok((
        Node::Await {
            binding,
            source,
            as_type,
        },
        1,
    ))
}

/// Take a leading JSON string literal, returning its unescaped value and the remainder.
fn take_string<'a>(s: &'a str, ctx: &str) -> Result<(String, &'a str)> {
    let (v, tail) = take_json(s)?;
    match v {
        serde_json::Value::String(x) => Ok((x, tail)),
        _ => Err(perr(&format!("`{ctx}` expects a quoted string"))),
    }
}

/// `confirm "message" [risk <level>]` + indented body — the human-in-the-loop gate.
fn parse_confirm(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (message, r) = take_string(rest.trim_start(), "confirm")?;
    let r = r.trim_start();
    let risk = if let Some(after) = kw(r, "risk") {
        let (level, tail) = take_while(after.trim_start(), is_name_char);
        if level.is_empty() {
            return Err(perr(
                "`confirm risk` expects a level (e.g. low/medium/high/critical)",
            ));
        }
        if !tail.trim().is_empty() {
            return Err(perr("trailing text after `confirm … risk <level>`"));
        }
        Some(level.to_string())
    } else if r.is_empty() {
        None
    } else {
        return Err(perr(&format!("unexpected text in `confirm` header: `{r}`")));
    };
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((
        Node::Confirm {
            message,
            risk,
            body,
        },
        1 + region.len(),
    ))
}

/// `throttle "name" <max> per <window_ms>` + indented body — the rate-limit guard-rail.
fn parse_throttle(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (name, r) = take_string(rest.trim_start(), "throttle")?;
    let (digits, r) = take_while(r.trim_start(), |c| c.is_ascii_digit());
    let max: u32 = digits
        .parse()
        .map_err(|_| perr("`throttle` expects a numeric max"))?;
    let after_per =
        kw(r.trim_start(), "per").ok_or_else(|| perr("`throttle` expects `per <window_ms>`"))?;
    let (window_ms, tail) = take_u64(after_per.trim_start())?;
    if !tail.trim().is_empty() {
        return Err(perr("trailing text after `throttle` header"));
    }
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((
        Node::Throttle {
            name,
            max,
            window_ms,
            body,
        },
        1 + region.len(),
    ))
}

/// `debounce "name" <wait_ms>` + indented body — coalesce rapid re-invocations.
fn parse_debounce(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (name, r) = take_string(rest.trim_start(), "debounce")?;
    let (wait_ms, tail) = take_u64(r.trim_start())?;
    if !tail.trim().is_empty() {
        return Err(perr("trailing text after `debounce` header"));
    }
    let region = child_region(lines, indent);
    let (body, _) = parse_stmts(region, indent)?;
    Ok((
        Node::Debounce {
            name,
            wait_ms,
            body,
        },
        1 + region.len(),
    ))
}

/// `verify <cmd> contains <expect> [: "message"]` — run a command, assert its output contains a
/// substring. `cmd`/`expect` are expressions (typically a `bash(…)` call and a string).
fn parse_verify(rest: &str) -> Result<(Node, usize)> {
    let (cmd, r) = parse_expr(rest.trim_start())?;
    let after = kw(r.trim_start(), "contains")
        .ok_or_else(|| perr("`verify` expects `<cmd> contains <expected>`"))?;
    let (expect, r2) = parse_expr(after.trim_start())?;
    let r2 = r2.trim_start();
    let message = if let Some(m) = r2.strip_prefix(':') {
        let (msg, tail) = take_string(m.trim_start(), "verify message")?;
        if !tail.trim().is_empty() {
            return Err(perr("trailing text after `verify` message"));
        }
        Some(msg)
    } else if r2.is_empty() {
        None
    } else {
        return Err(perr(&format!("unexpected text in `verify`: `{r2}`")));
    };
    Ok((
        Node::Verify {
            cmd: Box::new(cmd),
            expect: Box::new(expect),
            message,
        },
        1,
    ))
}

/// `parse(<value>, as: "<type>")` — the coercion node. `args_str` is the text just after `(`.
fn parse_parse_node(args_str: &str) -> Result<(Node, &str)> {
    let (value, r) = parse_delimited_expr(args_str.trim_start())?;
    let r = r
        .trim_start()
        .strip_prefix(',')
        .ok_or_else(|| perr("`parse(…)` expects `, as: \"type\"`"))?;
    let r = r
        .trim_start()
        .strip_prefix("as")
        .ok_or_else(|| perr("`parse(…)` expects an `as:` argument"))?;
    let r = r
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| perr("`parse(…)` expects `as: \"type\"`"))?;
    let (as_type, r) = take_string(r.trim_start(), "parse as")?;
    let r = r
        .trim_start()
        .strip_prefix(')')
        .ok_or_else(|| perr("expected `)` to close `parse(…)`"))?;
    Ok((
        Node::Parse {
            value: Box::new(value),
            as_type,
        },
        r,
    ))
}

/// `try` + body, optionally followed by a sibling `catch [$err]` + handler (the clause shape mirrors
/// `when`/`else`).
fn parse_try(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    if !rest.trim().is_empty() {
        return Err(perr(
            "`try` takes no header — put the guarded work in its body",
        ));
    }
    let body_region = child_region(lines, indent);
    let (body, _) = parse_stmts(body_region, indent)?;
    let mut used = 1 + body_region.len();
    let mut catch = None;
    let mut handler = Vec::new();
    if let Some(cand) = lines.get(used) {
        if cand.indent == indent {
            if let Some(c) = kw(&cand.text, "catch") {
                catch = if c.trim().is_empty() {
                    None
                } else {
                    let nm = c
                        .trim()
                        .strip_prefix('$')
                        .ok_or_else(|| perr("`catch` expects `$name` or nothing"))?;
                    let (name, tail) = take_while(nm, is_ident_char);
                    if name.is_empty() || !tail.trim().is_empty() {
                        return Err(perr(&format!("invalid `catch` binding: `{c}`")));
                    }
                    Some(SymbolName::from(name))
                };
                let handler_region = child_region(&lines[used..], indent);
                let (h, _) = parse_stmts(handler_region, indent)?;
                handler = h;
                used += 1 + handler_region.len();
            }
        }
    }
    Ok((
        Node::Try {
            body,
            catch,
            handler,
        },
        used,
    ))
}

/// `race <timeout_ms> [-> $bind]` + `branch $name` arms — first-wins concurrency (twin of `parallel`).
fn parse_race(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let (timeout_ms, r) = take_u64(rest.trim_start())?;
    let bind = parse_optional_arrow_bind(r, "race")?;
    let region = child_region(lines, indent);
    let (arms, default) = parse_arms(region, "branch")?;
    if !default.is_empty() {
        return Err(perr(
            "`race` has no `default` arm — use `branch $name` only",
        ));
    }
    let mut branches = Vec::with_capacity(arms.len());
    for (hdr, body) in arms {
        let name = hdr
            .trim()
            .strip_prefix('$')
            .ok_or_else(|| perr(&format!("`race` branch needs a `$name`, got: `{hdr}`")))?;
        let (nm, tail) = take_while(name, is_ident_char);
        if nm.is_empty() || !tail.trim().is_empty() {
            return Err(perr(&format!("invalid `race` branch name: `{hdr}`")));
        }
        branches.push(crate::ast::Branch {
            name: SymbolName::from(nm),
            body,
        });
    }
    Ok((
        Node::Race {
            timeout_ms,
            branches,
            bind,
        },
        1 + region.len(),
    ))
}

/// `scope [$res = <acquire>]` + body, optionally followed by a sibling `finally` + cleanup block.
fn parse_scope(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let rest = rest.trim();
    let (bind, acquire) = if rest.is_empty() {
        (None, None)
    } else {
        let nm = rest
            .strip_prefix('$')
            .ok_or_else(|| perr("`scope` header must be `$name = <acquire>`"))?;
        let (name, r) = take_while(nm, is_ident_char);
        if name.is_empty() {
            return Err(perr("`scope` has an empty resource name"));
        }
        let acq = r
            .trim_start()
            .strip_prefix('=')
            .ok_or_else(|| perr("`scope` expects `=` after `$name`"))?;
        let acquire = parse_condition_expr(acq, "scope acquire")?;
        (Some(SymbolName::from(name)), Some(Box::new(acquire)))
    };
    let body_region = child_region(lines, indent);
    let (body, _) = parse_stmts(body_region, indent)?;
    let mut used = 1 + body_region.len();
    let mut finally = Vec::new();
    if let Some(cand) = lines.get(used) {
        if cand.indent == indent && cand.text == "finally" {
            let fin_region = child_region(&lines[used..], indent);
            let (f, _) = parse_stmts(fin_region, indent)?;
            finally = f;
            used += 1 + fin_region.len();
        }
    }
    Ok((
        Node::Scope {
            acquire,
            bind,
            body,
            finally,
        },
        used,
    ))
}

/// `saga` + `step` … `undo` arm pairs (each `step` optionally followed by a sibling `undo`).
fn parse_saga(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    if !rest.trim().is_empty() {
        return Err(perr("`saga` takes no header — use `step`/`undo` arms"));
    }
    let region = child_region(lines, indent);
    let steps = parse_saga_steps(region)?;
    Ok((Node::Saga { steps }, 1 + region.len()))
}

fn parse_saga_steps(region: &[Line]) -> Result<Vec<crate::ast::SagaStep>> {
    let mut steps = Vec::new();
    if region.is_empty() {
        return Ok(steps);
    }
    let arm_indent = region[0].indent;
    let mut i = 0;
    while i < region.len() {
        if region[i].indent != arm_indent {
            return Err(perr_at(
                region[i].number,
                &format!("unexpected indentation in `saga`: `{}`", region[i].text),
            ));
        }
        if kw(&region[i].text, "step").is_none() {
            return Err(perr_at(
                region[i].number,
                &format!("expected `step`, got: `{}`", region[i].text),
            ));
        }
        let body_region = child_region(&region[i..], arm_indent);
        let (body, _) = parse_stmts(body_region, arm_indent)?;
        i += 1 + body_region.len();
        let mut undo = Vec::new();
        if i < region.len()
            && region[i].indent == arm_indent
            && kw(&region[i].text, "undo").is_some()
        {
            let undo_region = child_region(&region[i..], arm_indent);
            let (u, _) = parse_stmts(undo_region, arm_indent)?;
            undo = u;
            i += 1 + undo_region.len();
        }
        steps.push(crate::ast::SagaStep { body, undo });
    }
    Ok(steps)
}

/// `pipe [-> $bind]` + indented call steps.
fn parse_pipe(rest: &str, lines: &[Line], indent: usize) -> Result<(Node, usize)> {
    let bind = parse_optional_arrow_bind(rest, "pipe")?;
    let region = child_region(lines, indent);
    let (steps, _) = parse_stmts(region, indent)?;
    Ok((Node::Pipe { steps, bind }, 1 + region.len()))
}

/// `thing <kind> <selector> "<value>"` — an external reference. `rest` is the text after `thing`.
/// `<kind>` is a known kind word or `custom "<name>"`; `<selector>` is `id`/`name`/`path`/`query`/`key`.
fn parse_thing(rest: &str) -> Result<(Node, &str)> {
    let (kind_word, r) = take_while(rest.trim_start(), is_ident_char);
    if kind_word.is_empty() {
        return Err(perr(
            "`thing` expects a kind (e.g. `person`, `file`, `url`, or `custom \"…\"`)",
        ));
    }
    let (kind, r) = if kind_word == "custom" {
        let (name, r2) = take_string(r.trim_start(), "thing custom kind")?;
        (crate::ast::ThingKind::Custom(name), r2)
    } else {
        (
            thing_kind_from_word(kind_word)
                .ok_or_else(|| perr(&format!("unknown `thing` kind: `{kind_word}`")))?,
            r,
        )
    };
    let (sel_word, r) = take_while(r.trim_start(), is_ident_char);
    let (value, r) = take_string(r.trim_start(), "thing selector")?;
    let selector = selector_from_word(sel_word, value).ok_or_else(|| {
        perr(&format!(
            "unknown `thing` selector: `{sel_word}` (use id/name/path/query/key)"
        ))
    })?;
    Ok((
        Node::Thing {
            thing: crate::ast::ThingRef { kind, selector },
        },
        r,
    ))
}

fn thing_kind_from_word(w: &str) -> Option<crate::ast::ThingKind> {
    use crate::ast::ThingKind::*;
    Some(match w {
        "context" => Context,
        "file" => File,
        "person" => Person,
        "ticket" => Ticket,
        "email" => Email,
        "repo" => Repo,
        "dataset" => Dataset,
        "calendar_event" => CalendarEvent,
        "url" => Url,
        "secret" => Secret,
        _ => return None,
    })
}

fn selector_from_word(w: &str, v: String) -> Option<crate::ast::Selector> {
    use crate::ast::Selector::*;
    Some(match w {
        "id" => Id(v),
        "name" => Name(v),
        "path" => Path(v),
        "query" => Query(v),
        "key" => Key(v),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// Try to parse a native `expr` formula with `$var` syntax. Scans for `$`-prefixed identifiers,
/// extracts them as variables, strips the `$` from the formula text, and builds an `Expr` node
/// with `vars: {name: Var(name)}`. Returns `Some(Node::Expr)` if at least one `$var` is found,
/// or `None` if the text has no variables (so it's not a valid native expr — ordinary expression
/// parsing should handle it).
fn try_parse_native_expr(text: &str) -> Option<Node> {
    let mut var_names = std::collections::BTreeSet::new();
    let mut vars = std::collections::BTreeMap::new();
    let mut formula = String::new();

    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            // Check if this is followed by a valid identifier start
            if let Some(&next_c) = chars.peek() {
                if next_c.is_ascii_alphabetic() || next_c == '_' {
                    // Collect the full identifier (with dots for field access)
                    let mut ident = String::new();
                    while let Some(&id_c) = chars.peek() {
                        if id_c.is_ascii_alphanumeric() || id_c == '_' || id_c == '.' {
                            ident.push(id_c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    // Extract the root name (before the first dot)
                    let root_name = ident.split('.').next().unwrap_or(&ident).to_string();
                    var_names.insert(root_name);
                    // Add to formula without the $
                    formula.push_str(&ident);
                } else {
                    // Not a valid identifier, keep the $ in the formula
                    formula.push(c);
                }
            } else {
                // $ at end of string
                formula.push(c);
            }
        } else {
            formula.push(c);
        }
    }

    // If no variables found, this isn't a native expr
    if var_names.is_empty() {
        return None;
    }

    // Build the vars map with each name -> Var(name)
    for name in &var_names {
        vars.insert(
            name.clone(),
            Box::new(Node::Var {
                name: SymbolName::from(name.as_str()),
            }),
        );
    }

    Some(Node::Expr { formula, vars })
}

/// Parse exactly one expression that must span the whole of `s` (no trailing tokens).
fn parse_full_expr(s: &str, ctx: &str) -> Result<Node> {
    let (node, tail) = parse_expr(s)?;
    if !tail.trim().is_empty() {
        return Err(perr(&format!("trailing text in {ctx}: `{tail}`")));
    }
    Ok(node)
}

/// Parse a condition expression with native-expr fallback. First tries the normal expression parser;
/// if that fails or leaves a tail (operator tokens), attempts to parse as a native expr formula with
/// `$var` syntax. This enables conditions like `when $count > 3` and `until len($queue) == 0`.
fn parse_condition_expr(s: &str, ctx: &str) -> Result<Node> {
    let s = s.trim();
    // Try normal parsing first
    match parse_expr(s) {
        Ok((node, tail)) => {
            let tail_trimmed = tail.trim();
            // If there's no tail or the tail is empty, we got a complete expression
            if tail_trimmed.is_empty() {
                return Ok(node);
            }
            // There's a tail — likely operator tokens like `> 3` or `== 'ok'`
            // Fall through to native expr parsing
        }
        Err(_) => {
            // Normal parsing failed, fall through to native expr parsing
        }
    }

    // Try native expr parsing
    if let Some(expr_node) = try_parse_native_expr(s) {
        return Ok(expr_node);
    }

    // Neither worked, return the original parse error
    parse_full_expr(s, ctx)
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
            //
            // L-53 optional-access: a trailing `?` (`$plan.kind?`) marks the access lenient — an absent
            // key/out-of-range index yields `null` instead of erroring. Plain `$plan.kind` is strict.
            if let Some(dot) = name.find('.') {
                let (var, path) = name.split_at(dot); // `path` keeps the leading `.`
                if var.is_empty() {
                    return Err(perr("field access needs a symbol before `.`"));
                }
                let (optional, rest) = match rest.strip_prefix('?') {
                    Some(after) => (true, after),
                    None => (false, rest),
                };
                return Ok((
                    Node::Jq {
                        path: path.to_string(),
                        input: Box::new(Node::Var { name: var.into() }),
                        optional,
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
        '"' => {
            let (v, rest) = take_json(s)?;
            Ok((Node::Lit { value: v }, rest))
        }
        // A brace/bracket that is valid JSON (quoted keys, literal values) stays a `Lit`; otherwise —
        // an unquoted key or a `$var`/expr leaf — it is a value-template `Obj`/`List`. This try-JSON-
        // then-template split is exactly the round-trip partner of the formatter's "render a template
        // natively only when it has a dynamic leaf" rule, so the two never collide.
        '{' => match take_json(s) {
            Ok((v, rest)) => Ok((Node::Lit { value: v }, rest)),
            Err(_) => parse_obj_template(s),
        },
        '[' => match take_json(s) {
            Ok((v, rest)) => Ok((Node::Lit { value: v }, rest)),
            Err(_) => parse_list_template(s),
        },
        c if c == '-' || c.is_ascii_digit() => {
            let (v, rest) = take_json(s)?;
            Ok((Node::Lit { value: v }, rest))
        }
        c if c.is_ascii_alphabetic() || c == '_' => {
            let (ident, rest) = take_while(s, is_op_char);
            let rest_trim = rest.trim_start();
            if let Some(args_str) = rest_trim.strip_prefix('(') {
                // `parse(v, as: "T")` is the coercion node (a named `as:` arg), not an op call.
                if ident == "parse" {
                    return parse_parse_node(args_str);
                }
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
                    // `peek $sym` reads a symbol's current value (an expression, not `peek(…)`).
                    "peek" => {
                        let after = rest.trim_start();
                        let nm = after
                            .strip_prefix('$')
                            .ok_or_else(|| perr("`peek` expects `$name`"))?;
                        let (name, tail) = take_while(nm, is_ident_char);
                        if name.is_empty() {
                            return Err(perr("`peek` has an empty name"));
                        }
                        Ok((Node::Peek { name: name.into() }, tail))
                    }
                    // `thing <kind> <selector> "<value>"` — an external reference.
                    "thing" => parse_thing(rest),
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
        let (node, rest) = parse_delimited_expr(s)?;
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
        let (node, rest) = parse_delimited_expr(s)?;
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

/// Parse a value-template object `{ key: expr, … }` (the `Obj` node). Reached only when the braces are
/// not valid JSON (an unquoted key or a `$var`/expr leaf), so a pure-JSON object stays a `Lit`. Keys
/// may be bareword or JSON-quoted; each value is an arbitrary expression (so `$var`, `$v.path`,
/// `op(args)`, `fmt(…)`, and nested `{}`/`[]` leaves all work via `parse_expr`).
fn parse_obj_template(s: &str) -> Result<(Node, &str)> {
    let s = s.strip_prefix('{').ok_or_else(|| perr("expected `{`"))?;
    let mut fields: BTreeMap<String, Box<Node>> = BTreeMap::new();
    let mut s = s.trim_start();
    if let Some(r) = s.strip_prefix('}') {
        return Ok((Node::Obj { fields }, r));
    }
    loop {
        let (key, rest) = parse_obj_key(s)?;
        let rest = rest
            .trim_start()
            .strip_prefix(':')
            .ok_or_else(|| perr(&format!("expected `:` after object key `{key}`")))?;
        let (val, rest) = parse_delimited_expr(rest)?;
        fields.insert(key, Box::new(val));
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix(',') {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix('}') {
            return Ok((Node::Obj { fields }, r));
        }
        return Err(perr(&format!(
            "expected `,` or `}}` in object template, got: `{rest}`"
        )));
    }
}

/// Parse a value-template list `[ expr, … ]` (the `List` node). Reached only when the brackets are not
/// valid JSON (a `$var`/expr item), so a pure-JSON array stays a `Lit`.
fn parse_list_template(s: &str) -> Result<(Node, &str)> {
    let s = s.strip_prefix('[').ok_or_else(|| perr("expected `[`"))?;
    let mut items = Vec::new();
    let mut s = s.trim_start();
    if let Some(r) = s.strip_prefix(']') {
        return Ok((Node::List { items }, r));
    }
    loop {
        let (item, rest) = parse_delimited_expr(s)?;
        items.push(item);
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix(',') {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix(']') {
            return Ok((Node::List { items }, r));
        }
        return Err(perr(&format!(
            "expected `,` or `]` in list template, got: `{rest}`"
        )));
    }
}

/// Parse an object-template key: a JSON-quoted string or a bareword identifier.
fn parse_obj_key(s: &str) -> Result<(String, &str)> {
    let s = s.trim_start();
    if s.starts_with('"') {
        let (v, rest) = take_json(s)?;
        match v {
            serde_json::Value::String(k) => Ok((k, rest)),
            _ => Err(perr(
                "object-template key must be a bareword or quoted string",
            )),
        }
    } else {
        let (k, rest) = take_while(s, |c| c.is_ascii_alphanumeric() || c == '_');
        if k.is_empty() {
            return Err(perr(&format!(
                "expected an object-template key, got: `{s}`"
            )));
        }
        Ok((k.to_string(), rest))
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
    let (nm, tail) = take_while(nm, is_ident_char);
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
        if name.is_empty() || !name.chars().all(is_ident_char) {
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

/// Parse a semantic-effect tag. Delegates to [`FlowEffect::from_tag`], the single source of truth
/// for the tag vocabulary.
fn effect_from_tag(tag: &str) -> Option<FlowEffect> {
    FlowEffect::from_tag(tag)
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
            optional: false,
            path: path.into(),
            input: Box::new(input),
        }
    }
    fn s(v: &str) -> serde_json::Value {
        serde_json::Value::String(v.into())
    }

    #[test]
    fn durability_nodes_round_trip_natively() {
        // memo / once / checkpoint / await — formerly @json-only (L-60).
        let ast = DraftAst {
            body: vec![
                Node::Memo {
                    name: "x".into(),
                    value: Box::new(var("y")),
                    ty: None,
                    effect: None,
                },
                Node::Memo {
                    name: "n".into(),
                    value: Box::new(lit(s("hi"))),
                    ty: Some(TypeRef::String),
                    effect: Some(FlowEffect::Read),
                },
                Node::Once {
                    label: "charge".into(),
                    body: vec![call("pay", vec![])],
                    bind: Some("receipt".into()),
                },
                Node::Checkpoint {
                    label: "phase-1".into(),
                },
                Node::Await {
                    binding: Some("reply".into()),
                    source: "user_input".into(),
                    as_type: None,
                },
                Node::Await {
                    binding: None,
                    source: "webhook".into(),
                    as_type: None,
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "durability nodes should render natively:\n{text}"
        );
        assert!(text.contains("memo $x = $y"), "{text}");
        assert!(text.contains("@effect(read)"), "{text}");
        assert!(text.contains("memo $n: String = \"hi\""), "{text}");
        assert!(text.contains("once \"charge\" -> $receipt"), "{text}");
        assert!(text.contains("checkpoint \"phase-1\""), "{text}");
        assert!(text.contains("await $reply = \"user_input\""), "{text}");
        assert!(text.contains("await \"webhook\""), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn guardrail_and_sugar_nodes_round_trip_natively() {
        // confirm / throttle / debounce / verify + peek / parse — formerly @json-only (L-61).
        let ast = DraftAst {
            body: vec![
                Node::Confirm {
                    message: "delete prod?".into(),
                    risk: Some("high".into()),
                    body: vec![call("delete", vec![])],
                },
                Node::Confirm {
                    message: "ok?".into(),
                    risk: None,
                    body: vec![],
                },
                Node::Throttle {
                    name: "api".into(),
                    max: 5,
                    window_ms: 1000,
                    body: vec![call("fetch", vec![])],
                },
                Node::Debounce {
                    name: "save".into(),
                    wait_ms: 250,
                    body: vec![call("persist", vec![])],
                },
                Node::Verify {
                    cmd: Box::new(call("bash", vec![lit(s("echo hi"))])),
                    expect: Box::new(lit(s("hi"))),
                    message: Some("must greet".into()),
                },
                bind("v", Node::Peek { name: "x".into() }),
                bind(
                    "n",
                    Node::Parse {
                        value: Box::new(jq(".price", var("raw"))),
                        as_type: "f64".into(),
                    },
                ),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "batch-2 nodes should render natively:\n{text}"
        );
        assert!(
            text.contains("confirm \"delete prod?\" risk high"),
            "{text}"
        );
        assert!(text.contains("throttle \"api\" 5 per 1000"), "{text}");
        assert!(text.contains("debounce \"save\" 250"), "{text}");
        assert!(
            text.contains("verify bash(\"echo hi\") contains \"hi\": \"must greet\""),
            "{text}"
        );
        assert!(text.contains("$v = peek $x"), "{text}");
        assert!(
            text.contains("$n = parse($raw.price, as: \"f64\")"),
            "{text}"
        );
        assert_round_trips(&ast);
    }

    #[test]
    fn arm_body_control_flow_round_trips_natively() {
        // try / race / scope / saga / pipe — formerly @json-only (L-62).
        let ast = DraftAst {
            body: vec![
                Node::Try {
                    body: vec![call("risky", vec![])],
                    catch: Some("err".into()),
                    handler: vec![call("log", vec![var("err")])],
                },
                Node::Race {
                    timeout_ms: 5000,
                    branches: vec![
                        crate::ast::Branch {
                            name: "fast".into(),
                            body: vec![call("cheap", vec![])],
                        },
                        crate::ast::Branch {
                            name: "slow".into(),
                            body: vec![call("expensive", vec![])],
                        },
                    ],
                    bind: Some("winner".into()),
                },
                Node::Scope {
                    acquire: Some(Box::new(call("lock", vec![]))),
                    bind: Some("h".into()),
                    body: vec![call("use_it", vec![var("h")])],
                    finally: vec![call("release", vec![var("h")])],
                },
                Node::Saga {
                    steps: vec![
                        crate::ast::SagaStep {
                            body: vec![call("charge", vec![])],
                            undo: vec![call("refund", vec![])],
                        },
                        crate::ast::SagaStep {
                            body: vec![call("ship", vec![])],
                            undo: vec![],
                        },
                    ],
                },
                Node::Pipe {
                    steps: vec![call("a", vec![]), call("b", vec![])],
                    bind: Some("out".into()),
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "batch-3 nodes should render natively:\n{text}"
        );
        assert!(text.contains("catch $err"), "{text}");
        assert!(text.contains("race 5000 -> $winner"), "{text}");
        assert!(text.contains("branch $fast"), "{text}");
        assert!(text.contains("scope $h = lock()"), "{text}");
        assert!(text.contains("finally"), "{text}");
        assert!(
            text.contains("saga") && text.contains("step") && text.contains("undo"),
            "{text}"
        );
        assert!(text.contains("pipe -> $out"), "{text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn thing_references_round_trip_natively() {
        use crate::ast::{Selector, ThingKind, ThingRef};
        let mk = |kind: ThingKind, selector: Selector| Node::Thing {
            thing: ThingRef { kind, selector },
        };
        let ast = DraftAst {
            body: vec![
                bind("f", mk(ThingKind::File, Selector::Path("src/x.rs".into()))),
                bind("p", mk(ThingKind::Person, Selector::Name("john".into()))),
                bind("u", mk(ThingKind::Url, Selector::Id("https://x".into()))),
                bind(
                    "c",
                    mk(
                        ThingKind::Custom("widget".into()),
                        Selector::Key("w-1".into()),
                    ),
                ),
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "thing refs should render natively:\n{text}"
        );
        assert!(text.contains("$f = thing file path \"src/x.rs\""), "{text}");
        assert!(text.contains("$p = thing person name \"john\""), "{text}");
        assert!(text.contains("$u = thing url id \"https://x\""), "{text}");
        assert!(
            text.contains("$c = thing custom \"widget\" key \"w-1\""),
            "{text}"
        );
        assert_round_trips(&ast);
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
    fn all_literal_and_empty_templates_use_json_while_lit_json_stays_native() {
        // The round-trip disjointness rule, pinned from both sides: an all-literal or empty `Obj`/`List`
        // has no native spelling and uses `@json` (so its text can never collide with a `Lit`'s
        // `{…}`/`[…]`), while a `Lit` holding a JSON object/array still renders as compact JSON.
        let all_lit: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj", "fields": { "ok": {"kind": "lit", "value": true} }
        }))
        .unwrap();
        let empty_obj: Node = serde_json::from_value(serde_json::json!({"kind": "obj"})).unwrap();
        let empty_list: Node = serde_json::from_value(serde_json::json!({"kind": "list"})).unwrap();
        for node in [all_lit, empty_obj, empty_list] {
            let ast = DraftAst {
                body: vec![bind("r", node)],
                ..Default::default()
            };
            assert!(
                format(&ast).contains("@json"),
                "all-literal/empty template should use @json"
            );
            assert_round_trips(&ast);
        }

        // `Lit` JSON objects/arrays stay native compact JSON (NOT @json) and round-trip as `Lit`.
        let lit_ast = DraftAst {
            body: vec![
                bind("o", lit(serde_json::json!({"a": 1}))),
                bind("xs", lit(serde_json::json!([1, 2, 3]))),
            ],
            ..Default::default()
        };
        let text = format(&lit_ast);
        assert!(!text.contains("@json"), "Lit JSON stays native: {text}");
        assert!(text.contains(r#"$o = {"a":1}"#), "{text}");
        assert_round_trips(&lit_ast);
    }

    #[test]
    fn obj_and_list_templates_are_native_when_dynamic() {
        // A template with a dynamic ($var/expr) leaf spells natively as `{ k: expr }` / `[ … ]` and
        // round-trips: field-access leaves, nested templates, mixed static/dynamic, and a
        // non-identifier (quoted) key.
        let template: Node = serde_json::from_value(serde_json::json!({
            "kind": "obj",
            "fields": {
                "intent": {"kind": "jq", "path": ".intent", "input": {"kind": "var", "name": "x"}},
                "ok": {"kind": "lit", "value": true},
                "items": {"kind": "list", "items": [
                    {"kind": "var", "name": "a"},
                    {"kind": "lit", "value": 1}
                ]},
                "nested": {"kind": "obj", "fields": { "deep": {"kind": "var", "name": "d"} }},
                "a-b": {"kind": "var", "name": "q"}
            }
        }))
        .unwrap();
        let ast = DraftAst {
            body: vec![Node::Return {
                value: Box::new(template),
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            !text.contains("@json"),
            "dynamic template should be native: {text}"
        );
        assert!(text.contains("$x.intent"), "field-access leaf: {text}");
        assert!(
            text.contains("ok: true"),
            "literal leaf rendered inline: {text}"
        );
        assert!(
            text.contains(r#""a-b": $q"#),
            "non-ident key is quoted: {text}"
        );
        assert_round_trips(&ast);
    }

    #[test]
    fn assert_round_trips_natively() {
        // `assert <cond> [, "<message>"]`; a comma inside `op(a,b)` belongs to the cond, not the message.
        let ast = DraftAst {
            body: vec![
                Node::Assert {
                    cond: Box::new(var("hits")),
                    message: Some("grep returned nothing".into()),
                },
                Node::Assert {
                    cond: Box::new(var("gate")),
                    message: None,
                },
                Node::Assert {
                    cond: Box::new(call("ok", vec![var("a"), var("b")])),
                    message: Some("two-arg cond".into()),
                },
            ],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            text.contains("assert $hits, \"grep returned nothing\""),
            "{text}"
        );
        assert!(text.contains("assert $gate\n"), "no-message form: {text}");
        assert!(!text.contains("@json"), "assert is native: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn retry_round_trips_natively_with_a_json_guard_fallback() {
        let full = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: Some("exponential".into()),
                delay_ms: Some(500),
                body: vec![call("flaky", vec![])],
                bind: Some("out".into()),
            }],
            ..Default::default()
        };
        let text = format(&full);
        assert!(
            text.contains("retry 3 backoff exponential delay 500 -> $out"),
            "{text}"
        );
        assert!(!text.contains("@json"), "native: {text}");
        assert_round_trips(&full);

        let minimal = DraftAst {
            body: vec![Node::Retry {
                max: 1,
                backoff: None,
                delay_ms: None,
                body: vec![call("once", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        assert!(
            format(&minimal).contains("retry 1\n"),
            "minimal: {}",
            format(&minimal)
        );
        assert_round_trips(&minimal);

        // A `backoff` with whitespace can't be spelled natively → falls back to `@json` (still exact).
        let weird = DraftAst {
            body: vec![Node::Retry {
                max: 2,
                backoff: Some("a b".into()),
                delay_ms: None,
                body: vec![call("x", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        assert!(
            format(&weird).contains("@json"),
            "guard fallback: {}",
            format(&weird)
        );
        assert_round_trips(&weird);
    }

    #[test]
    fn parallel_round_trips_natively() {
        let ast = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: "readme".into(),
                        body: vec![call("read", vec![lit(s("README.md"))])],
                    },
                    Branch {
                        name: "todos".into(),
                        body: vec![call("grep", vec![lit(s("TODO"))])],
                    },
                ],
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("parallel\n"), "{text}");
        assert!(text.contains("branch $readme"), "{text}");
        assert!(text.contains("branch $todos"), "{text}");
        assert!(!text.contains("@json"), "native: {text}");
        assert_round_trips(&ast);

        // Single-branch and empty `parallel` also round-trip.
        let single = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![Branch {
                    name: "only".into(),
                    body: vec![call("go", vec![])],
                }],
            }],
            ..Default::default()
        };
        assert_round_trips(&single);
        let empty = DraftAst {
            body: vec![Node::Parallel { branches: vec![] }],
            ..Default::default()
        };
        assert_round_trips(&empty);
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
    fn with_tools_round_trips_natively() {
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["read_many".into(), "git_status".into()],
                body: vec![call("read_many", vec![])],
                bind: Some("scoped".into()),
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            text.contains(r#"with_tools ["read_many","git_status"] -> $scoped"#),
            "{text}"
        );
        assert!(!text.contains("@json"), "with_tools native: {text}");
        assert_round_trips(&ast);
    }

    #[test]
    fn with_tools_empty_allowlist_round_trips() {
        // `with_tools []` — the strictest scope (no tool at all).
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec![],
                body: vec![call("noop", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        let text = format(&ast);
        assert!(text.contains("with_tools []"), "{text}");
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
        let ast = DraftAst {
            body: vec![
                // Permanently-@json shapes at statement position: unspellable (dotted) names can
                // never be spelled natively, so they exercise the statement-position escape
                // regardless of which node kinds gain native syntax (memo/once/checkpoint/await/…).
                Node::Var { name: "a.b".into() },
                Node::Bind {
                    name: "c.d".into(),
                    value: Box::new(lit(s("x"))),
                    ty: None,
                    effect: None,
                },
                // `jq(".bitcoin.usd", $raw)` inline is *native* field-access sugar; a bracket path
                // is not, so it uses the inline @json escape.
                Node::Bind {
                    name: "price".into(),
                    value: Box::new(Node::Jq {
                        path: ".prices[0]".into(),
                        input: Box::new(var("raw")),
                        optional: false,
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
        };
        let text = format(&ast);
        // Statement-position `@json` lines are really present (explicit escape coverage).
        assert!(
            text.lines()
                .filter(|l| l.trim_start().starts_with("@json "))
                .count()
                >= 2,
            "expected statement-position @json lines: {text}"
        );
        assert_round_trips(&ast);
    }

    // ----- F21: name guards — the round-trip is total for unspellable names -----

    /// Confirmed counterexample 1 (F8/F21): `Var{name:"a.b"}` in expression position used to format
    /// as `$a.b`, which reparses as `Jq{".b", $a}` — a *different program*, silently.
    #[test]
    fn dotted_var_name_round_trips_via_json_fallback() {
        let expr_pos = DraftAst {
            body: vec![bind("x", var("a.b"))],
            ..Default::default()
        };
        let text = format(&expr_pos);
        assert!(text.contains("@json"), "expression position: {text}");
        assert_round_trips(&expr_pos);

        // Statement position: the parser no longer admits `.` in declared names, so the formatter
        // must fall back here too (it used to round-trip only by charset luck).
        let stmt_pos = DraftAst {
            body: vec![var("a.b")],
            ..Default::default()
        };
        let text = format(&stmt_pos);
        assert!(text.contains("@json"), "statement position: {text}");
        assert_round_trips(&stmt_pos);
    }

    /// Confirmed counterexample 2 (F21): symbol/op names with spaces used to produce *unparseable*
    /// text (loud failure). They now fall back to `@json` and round-trip exactly.
    #[test]
    fn space_names_round_trip_via_json_fallback() {
        let ast = DraftAst {
            body: vec![
                bind("a b", lit(serde_json::json!(1))),
                call("git status", vec![lit(serde_json::json!("."))]),
                var("a b"),
                bind("y", call("git status", vec![])), // inline op position
            ],
            ..Default::default()
        };
        assert_round_trips(&ast);
    }

    /// Every declared-name position falls back to `@json` when the name is unspellable — one node
    /// per position, each with a name the surface cannot spell.
    #[test]
    fn every_name_position_guards_unspellable_names() {
        let bad: SymbolName = "a b".into();
        let some_bad = || Some(SymbolName::from("a b"));
        let nodes: Vec<(&str, Node)> = vec![
            (
                "bind name",
                Node::Bind {
                    name: bad.clone(),
                    value: Box::new(lit(serde_json::json!(1))),
                    ty: None,
                    effect: None,
                },
            ),
            (
                "bind ty (builtin-colliding Named)",
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(lit(serde_json::json!(1))),
                    ty: Some(TypeRef::Named("Bool".into())),
                    effect: None,
                },
            ),
            (
                "memo name",
                Node::Memo {
                    name: bad.clone(),
                    value: Box::new(lit(serde_json::json!(1))),
                    ty: None,
                    effect: None,
                },
            ),
            (
                "each item",
                Node::Each {
                    source: Box::new(var("xs")),
                    item: bad.clone(),
                    body: vec![],
                    collect: None,
                    flat: false,
                },
            ),
            (
                "each collect",
                Node::Each {
                    source: Box::new(var("xs")),
                    item: "x".into(),
                    body: vec![],
                    collect: some_bad(),
                    flat: false,
                },
            ),
            (
                "repeat collect",
                Node::Repeat {
                    max: 2,
                    until: None,
                    body: vec![],
                    collect: some_bad(),
                },
            ),
            (
                "seq bind",
                Node::Seq {
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "ctx name",
                Node::Ctx {
                    name: bad.clone(),
                    purpose: None,
                    include: vec![],
                    exclude: vec![],
                    budget: None,
                },
            ),
            (
                "ctx include sym",
                Node::Ctx {
                    name: "p".into(),
                    purpose: None,
                    include: vec![bad.clone()],
                    exclude: vec![],
                    budget: None,
                },
            ),
            (
                "ctx exclude sym",
                Node::Ctx {
                    name: "p".into(),
                    purpose: None,
                    include: vec![],
                    exclude: vec![bad.clone()],
                    budget: None,
                },
            ),
            (
                "ctx_append ctx",
                Node::CtxAppend {
                    ctx: bad.clone(),
                    add: vec![],
                },
            ),
            (
                "ctx_append sym",
                Node::CtxAppend {
                    ctx: "p".into(),
                    add: vec![bad.clone()],
                },
            ),
            (
                "fallback bind",
                Node::Fallback {
                    branches: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "loop bind",
                Node::Loop {
                    for_ms: 1,
                    every_ms: 1,
                    until: None,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "timeout bind",
                Node::Timeout {
                    ms: 1,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "budget bind",
                Node::Budget {
                    limit: 1,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "with_tools bind",
                Node::CapScope {
                    tools: vec![],
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "retry bind",
                Node::Retry {
                    max: 1,
                    backoff: None,
                    delay_ms: None,
                    body: vec![],
                    bind: some_bad(),
                },
            ),
            (
                "parallel branch name",
                Node::Parallel {
                    branches: vec![Branch {
                        name: bad.clone(),
                        body: vec![],
                    }],
                },
            ),
            ("call op (statement)", call("not an op", vec![])),
            ("call op (inline)", bind("x", call("not an op", vec![]))),
            (
                "call op literally `fmt` (inline collides with the Fmt node)",
                bind("x", call("fmt", vec![lit(serde_json::json!("hi"))])),
            ),
            ("jq input var name", bind("x", jq(".kind", var("a b")))),
            ("var (statement)", var("a b")),
            ("var (expression)", bind("x", var("a b"))),
        ];
        for (what, node) in nodes {
            let ast = DraftAst {
                body: vec![node],
                ..Default::default()
            };
            let text = format(&ast);
            assert!(text.contains("@json"), "{what} must @json-fallback: {text}");
            assert_round_trips(&ast);
        }
    }

    // ----- F8 text side: `.` is no longer a declared-name character -----

    #[test]
    fn dotted_declared_names_are_parse_errors() {
        for bad in [
            "flow x\n  $a.b = 1",
            "flow x\n  $a.b: Number = 1",
            "flow x\n  $pack.x += $a",
            "flow x\n  each $it.x in $xs",
            "flow x\n  seq -> $out.y",
            "flow x\n  repeat 2 -> $c.d",
            "flow x\n  ctx $p.q",
            "flow x\n  ctx $p\n    include $a.b",
            "flow x\n  parallel\n    branch $l.r\n      do go",
        ] {
            assert!(parse(bad).is_err(), "expected Err for: {bad:?}");
        }
        // Expression position is untouched: `$plan.kind` stays jq field-access sugar.
        let ast = parse("flow x\n  $k = $plan.kind\n").unwrap();
        assert_eq!(ast.body, vec![bind("k", jq(".kind", var("plan")))]);
        // …and a bare `$a` statement still parses as a var reference.
        let ast = parse("flow x\n  $a\n").unwrap();
        assert_eq!(ast.body, vec![var("a")]);
    }

    /// The documented flow-header exception: the header has no `@json` escape, so an unspellable
    /// flow name cannot round-trip — but it must fail **loudly** (a parse error), never silently
    /// reparse as a different program. The analyzer rejects such names before they ever format.
    #[test]
    fn unspellable_flow_header_name_is_a_loud_parse_error() {
        let ast = DraftAst {
            name: Some("a b".into()),
            ..Default::default()
        };
        let text = format(&ast);
        assert!(
            parse(&text).is_err(),
            "space flow name must fail loudly, got Ok for: {text}"
        );
    }

    // ----- F22: parse errors carry 1-based source line numbers -----

    #[test]
    fn parse_errors_carry_line_numbers() {
        // Comment and blank lines still count toward the source line number.
        let err = parse("flow x\n\n  # a comment\n  $a =").unwrap_err();
        assert!(err.to_string().contains("line 4:"), "{err}");
        // The innermost statement line wins, not the enclosing block header.
        let err = parse("flow x\n  when $ok\n    do\n").unwrap_err();
        assert!(err.to_string().contains("line 3:"), "{err}");
        // Header errors point at the header line.
        let err = parse("not a flow").unwrap_err();
        assert!(err.to_string().contains("line 1:"), "{err}");
        // Module declarations locate their lines too.
        let err = parse_program("agent a\n  model \"m\"\n\nwidget w\n").unwrap_err();
        assert!(err.to_string().contains("line 4:"), "{err}");
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

    // ----- Multi-line string literals (L-39): `"""…"""`, verbatim, delimiter-terminated -----

    #[test]
    fn multiline_string_literal_parses_verbatim_across_physical_lines() {
        let src = "\
flow
  $x = \"\"\"first line
second line
third line\"\"\"
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![bind("x", lit(s("first line\nsecond line\nthird line")))]
        );
    }

    #[test]
    fn multiline_string_content_is_taken_literally_no_comment_no_escape_processing() {
        // `#` and backslashes inside the block are ordinary characters, not a comment start or an
        // escape sequence — the whole point of the spelling is "no interpretation between the
        // delimiters".
        let src = "\
flow
  $x = \"\"\"line one # not a comment
back\\slash and a \"single\" quote are literal\"\"\"
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![bind(
                "x",
                lit(s(
                    "line one # not a comment\nback\\slash and a \"single\" quote are literal"
                ))
            )]
        );
    }

    #[test]
    fn multiline_string_works_as_a_call_arg_and_inside_an_object_template() {
        let src = "\
flow
  do write \"out.txt\", \"\"\"payload
line 2\"\"\"
  $t = { path: \"out.txt\", content: \"\"\"templated
value\"\"\" }
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![
                call("write", vec![lit(s("out.txt")), lit(s("payload\nline 2"))]),
                bind(
                    "t",
                    Node::Obj {
                        fields: {
                            let mut m = std::collections::BTreeMap::new();
                            m.insert("path".to_string(), Box::new(lit(s("out.txt"))));
                            m.insert("content".to_string(), Box::new(lit(s("templated\nvalue"))));
                            m
                        },
                    }
                ),
            ]
        );
    }

    #[test]
    fn empty_multiline_string_parses_as_empty_string() {
        let src = "flow\n  $x = \"\"\"\"\"\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body, vec![bind("x", lit(s("")))]);
    }

    #[test]
    fn unterminated_multiline_string_is_a_located_parse_error() {
        let src = "flow\n  $x = \"\"\"never closed\n";
        let err = parse(src).unwrap_err();
        assert!(
            err.to_string().contains("line 2:"),
            "expected the opening line in the error, got: {err}"
        );
        assert!(
            err.to_string().to_lowercase().contains("unterminated"),
            "expected an 'unterminated' diagnostic, got: {err}"
        );
    }

    #[test]
    fn multiline_block_inside_a_pure_json_object_stays_a_lit_not_a_template() {
        // The dominant corpus case (`do edit {"path":...,"content":"""..."""}`): quoted keys make
        // this valid JSON, so it must desugar to a plain escaped string BEFORE the parser's
        // try-JSON-then-template split runs, staying a `Lit`, not falling through to the `Obj`
        // value-template reader (bareword-key path).
        let src = "\
flow
  do edit {\"path\": \"f.txt\", \"content\": \"\"\"line1
line2\"\"\"}
";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![call(
                "edit",
                vec![lit(
                    serde_json::json!({"path": "f.txt", "content": "line1\nline2"})
                )]
            )]
        );
    }

    #[test]
    fn escaped_triple_quotes_inside_a_normal_string_are_not_mistaken_for_a_block() {
        // `\"\"\"` (three ESCAPED quotes) inside an ordinary `"…"` string must stay ordinary string
        // content — the in-string escape tracking keeps the multi-line-block detector from firing
        // while a normal string is open.
        let src = "flow\n  $x = \"a\\\"\\\"\\\"b\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body, vec![bind("x", lit(s("a\"\"\"b")))]);
    }

    #[test]
    fn two_multiline_blocks_in_one_statement() {
        let src = "flow\n  do op \"\"\"x\ny\"\"\", \"\"\"a\nb\"\"\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![call("op", vec![lit(s("x\ny")), lit(s("a\nb"))])]
        );
    }

    #[test]
    fn multiline_content_preserves_blank_lines_indentation_and_statement_look_alikes() {
        // `#`, blank lines, indentation, and a line that looks like its own statement are all just
        // literal characters inside the block — nothing about it is interpreted.
        let src = "flow\n  $x = \"\"\"# not a comment\n\n  indented\n$y = 1\"\"\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(
            ast.body,
            vec![bind("x", lit(s("# not a comment\n\n  indented\n$y = 1")))]
        );
    }

    // ----- Totality: malformed input errors, never panics -----

    #[test]
    fn malformed_input_returns_parse_error() {
        // Inputs with a located line: the error must carry a `line N:` prefix (F22).
        for (bad, line) in [
            ("not a flow", 1),
            ("flow x\n\telse", 2),            // tab indentation
            ("flow x\n  $ = 1", 2),           // empty symbol
            ("flow x\n  $a = ", 2),           // missing expression
            ("flow x\n  do", 2),              // do without op
            ("flow x\n  each $f $xs", 2),     // each without `in`
            ("flow x\n  repeat\n", 2),        // repeat without count
            ("flow x\n  when", 2),            // when without condition
            ("flow x\n  $a = read(\"x\"", 2), // unbalanced parens
            ("flow x\n  else", 2),            // dangling else
            ("flow x\n  @json {oops}", 2),    // invalid json
        ] {
            let err = parse(bad).unwrap_err();
            assert!(
                err.to_string().contains(&format!("line {line}:")),
                "expected `line {line}:` for {bad:?}, got: {err}"
            );
        }
        // Empty input has no line to point at.
        assert!(parse("").is_err());
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

    // -----------------------------------------------------------------------
    // Program / module layer
    // -----------------------------------------------------------------------

    const CANONICAL_APP: &str = "\
# app.flux — the whole app in native flux-lang
agent assistant
  model \"claude-sonnet-4-6\"
  tools [search, send]
  datasources [docs]
  description \"answers from the docs\"

channel slack
  bot_token secret \"SLACK_BOT_TOKEN\"
  app_token secret \"SLACK_APP_TOKEN\"

datasource docs
  kind \"markdown\"
  path \"./docs\"

op repo_health(path: String, prior: Ctx) -> Health
  description \"Check git state and summarize failures\"
  risk \"medium\"
  idempotency \"idempotent\"
  effects [read, process, model]
  limits {dispatches: 20, timeout_ms: 120000, context_chars: 8000}
  expose true

  $status = git_status()
  $tests = cargo_test({args: [\"--workspace\"]})
  ctx $pack
    purpose \"repo-health\"
    budget 8000
    include $prior, $status, $tests
  $summary = ai.reason({ask: \"Summarize repo health\", ctx: $pack})
  return {status: $status, tests: $tests, summary: $summary}

trigger on_msg
  on \"slack\"
  run greet
  agent assistant

journey greet
  agent assistant
  flow
    $r = complete($text)
    return $r
";

    #[test]
    fn parse_program_reads_the_full_typed_module_surface() {
        let Module::Program(p) = parse_program(CANONICAL_APP).unwrap() else {
            panic!("module declarations must sniff as a program");
        };

        // agent
        assert_eq!(p.agents.len(), 1);
        let a = &p.agents[0];
        assert_eq!(a.name, "assistant");
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(a.tools, vec!["search", "send"]);
        assert_eq!(a.datasources, vec!["docs"]);
        assert_eq!(a.description.as_deref(), Some("answers from the docs"));

        // channel — kind defaults to the name; secrets are markers, never plaintext
        assert_eq!(p.channels.len(), 1);
        assert_eq!(p.channels[0].kind, "slack");
        assert_eq!(
            p.channels[0].settings["bot_token"],
            serde_json::json!({ "$secret": "SLACK_BOT_TOKEN" })
        );

        // datasource
        assert_eq!(p.datasources.len(), 1);
        assert_eq!(p.datasources[0].kind, "markdown");
        assert_eq!(p.datasources[0].path.as_deref(), Some("./docs"));

        // composite op
        assert_eq!(p.ops.len(), 1);
        let op = &p.ops[0];
        assert_eq!(op.name, "repo_health");
        assert_eq!(op.params.len(), 2);
        assert_eq!(
            op.returns.as_ref().map(TypeRef::label).as_deref(),
            Some("Health")
        );
        assert_eq!(op.meta.risk, Risk::Medium);
        assert_eq!(op.meta.idempotency, Idempotency::Idempotent);
        assert_eq!(
            op.meta.effects,
            vec![Effect::Read, Effect::Process, Effect::Network]
        );
        assert_eq!(op.meta.limits.dispatches, Some(20));
        assert!(op.meta.expose);
        assert!(matches!(op.body.body.last(), Some(Node::Return { .. })));

        // trigger
        assert_eq!(p.triggers[0].on, "slack");
        assert_eq!(p.triggers[0].run, "greet");
        assert_eq!(p.triggers[0].agent.as_deref(), Some("assistant"));

        // journey + its inline flow body (defaulted to the journey name)
        let flow = p.flow_named("greet").expect("journey resolves by name");
        assert_eq!(flow.name.as_deref(), Some("greet"));
        assert_eq!(p.journeys[0].agent.as_deref(), Some("assistant"));
        assert!(
            matches!(flow.body.last(), Some(Node::Return { .. })),
            "the flow body parsed as native statements"
        );
    }

    #[test]
    fn program_permissions_and_agent_narrowing_parse() {
        let src = r#"permissions
  allow [search, "ai.reason", send]
  deny [write, bash]

agent guide
  tools [search]
  datasources [handbook]
  allow [search, "ai.reason", send]
  deny [bash]

trigger questions
  on "user_input"
  run answer

journey answer
  agent guide
  flow
    return ""
"#;
        let Module::Program(program) = parse_program(src).expect("parse program") else {
            panic!("expected program")
        };
        let app = program.permissions.expect("top-level permissions");
        assert_eq!(
            app.allow.as_deref(),
            Some(&["search".into(), "ai.reason".into(), "send".into()][..])
        );
        assert_eq!(app.deny, ["write", "bash"]);
        let agent = program.agents.first().expect("guide");
        let permissions = agent.permissions.as_ref().expect("agent permissions");
        assert_eq!(
            permissions.allow.as_deref(),
            Some(&["search".into(), "ai.reason".into(), "send".into()][..])
        );
        assert_eq!(permissions.deny, ["bash"]);
        assert_eq!(program.journeys[0].agent.as_deref(), Some("guide"));
    }

    #[test]
    fn permission_allow_absent_inherits_but_explicit_empty_denies_all() {
        let Module::Program(inherited) = parse_program("permissions\n  deny [bash]\n").unwrap()
        else {
            panic!("program")
        };
        assert_eq!(inherited.permissions.unwrap().allow, None);

        let Module::Program(empty) = parse_program("permissions\n  allow []\n").unwrap() else {
            panic!("program")
        };
        assert_eq!(empty.permissions.unwrap().allow, Some(Vec::new()));
    }

    #[test]
    fn duplicate_or_unknown_permission_declarations_are_rejected() {
        for (src, needle) in [
            ("permissions\n\npermissions\n", "only once"),
            (
                "permissions\n  allow [read]\n  allow [search]\n",
                "duplicate permissions attribute `allow`",
            ),
            (
                "permissions\n  maybe [read]\n",
                "unknown permissions attribute",
            ),
            (
                "agent guide\n  allow [read]\n  allow [search]\n",
                "duplicate agent attribute `allow`",
            ),
        ] {
            let err = parse_program(src).unwrap_err().to_string();
            assert!(err.contains(needle), "`{src}`: {err}");
        }
    }

    #[test]
    fn agent_bound_trigger_needs_no_run_journey() {
        // An agent-bound trigger routes to a model turn (`run_agent`), so a `run` journey name is
        // optional — the runtime never reads it. This is the shape the support-bot example uses.
        let src = "trigger on_msg\n  on \"slack\"\n  agent assistant\n";
        let Module::Program(p) = parse_program(src).unwrap() else {
            panic!("program")
        };
        assert_eq!(p.triggers[0].on, "slack");
        assert_eq!(p.triggers[0].agent.as_deref(), Some("assistant"));
        assert!(p.triggers[0].run.is_empty(), "no journey to run");
    }

    #[test]
    fn trigger_with_neither_run_nor_agent_is_an_error() {
        let src = "trigger on_msg\n  on \"slack\"\n";
        let err = parse_program(src).unwrap_err().to_string();
        assert!(
            err.contains("`run`") && err.contains("`agent`"),
            "error names both routes: {err}"
        );
    }

    #[test]
    fn channel_kind_can_be_overridden() {
        let src = "channel myslack\n  kind \"slack\"\n";
        let Module::Program(p) = parse_program(src).unwrap() else {
            panic!("program")
        };
        assert_eq!(p.channels[0].name, "myslack");
        assert_eq!(p.channels[0].kind, "slack");
    }

    #[test]
    fn setting_values_cover_literals_lists_records_and_secrets() {
        assert_eq!(parse_setting("\"hi\"").unwrap(), serde_json::json!("hi"));
        assert_eq!(parse_setting("42").unwrap(), serde_json::json!(42));
        assert_eq!(parse_setting("true").unwrap(), serde_json::json!(true));
        assert_eq!(parse_setting("null").unwrap(), serde_json::Value::Null);
        // bare identifiers coerce to strings (so `tools [search, send]` works)
        assert_eq!(
            parse_setting("markdown").unwrap(),
            serde_json::json!("markdown")
        );
        assert_eq!(
            parse_setting("[a, b]").unwrap(),
            serde_json::json!(["a", "b"])
        );
        assert_eq!(
            parse_setting("{ k: 1, name: hello }").unwrap(),
            serde_json::json!({ "k": 1, "name": "hello" })
        );
        assert_eq!(
            parse_setting("secret \"TOKEN\"").unwrap(),
            serde_json::json!({ "$secret": "TOKEN" })
        );
    }

    #[test]
    fn a_lone_flow_is_a_bare_flow_not_a_program() {
        let m = parse_program("flow greet\n  return null").unwrap();
        assert!(matches!(m, Module::Flow(_)));
    }

    #[test]
    fn an_unknown_top_level_decl_is_a_clean_error() {
        let err = parse_program("widget foo\n  x 1\n").unwrap_err();
        assert!(format!("{err}").contains("unknown top-level declaration"));
    }

    #[test]
    fn parse_when_native_comparison() {
        let src = "flow test\n  when $count > 3\n    return true\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::When { cond, .. } = &ast.body[0] else {
            panic!("expected When node");
        };
        match cond.as_ref() {
            Node::Expr { formula, vars } => {
                assert_eq!(formula, "count > 3");
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("count"));
                match vars.get("count").unwrap().as_ref() {
                    Node::Var { name } => assert_eq!(name.0, "count"),
                    _ => panic!("expected Var node"),
                }
            }
            _ => panic!("expected Expr node, got {:?}", cond),
        }
    }

    #[test]
    fn parse_until_native_call_predicate() {
        let src = "flow test\n  repeat 10\n    until len($queue) == 0\n    return null\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::Repeat { until, .. } = &ast.body[0] else {
            panic!("expected Repeat node");
        };
        let Some(until_node) = until else {
            panic!("expected until condition");
        };
        match until_node.as_ref() {
            Node::Expr { formula, vars } => {
                assert!(formula.contains("len(queue)"));
                assert!(formula.contains("== 0"));
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("queue"));
            }
            _ => panic!("expected Expr node, got {:?}", until_node),
        }
    }

    #[test]
    fn parse_bind_rhs_native_expr() {
        let src = "flow test\n  $ok = $score >= 0.8\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::Bind { name, value, .. } = &ast.body[0] else {
            panic!("expected Bind node");
        };
        assert_eq!(name.0, "ok");
        match value.as_ref() {
            Node::Expr { formula, vars } => {
                assert_eq!(formula, "score >= 0.8");
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("score"));
            }
            _ => panic!("expected Expr node, got {:?}", value),
        }
    }

    #[test]
    fn parse_assert_native_expr_with_message() {
        let src = "flow test\n  assert $n > 0, \"must be positive\"\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::Assert { cond, message } = &ast.body[0] else {
            panic!("expected Assert node");
        };
        match cond.as_ref() {
            Node::Expr { formula, vars } => {
                assert_eq!(formula, "n > 0");
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("n"));
            }
            _ => panic!("expected Expr node, got {:?}", cond),
        }
        assert_eq!(message.as_ref().unwrap(), "must be positive");
    }

    #[test]
    fn roundtrip_native_expr_preserves_json() {
        use crate::format::format;

        let test_cases = vec![
            "flow test\n  when $count > 3\n    return true\n",
            "flow test\n  $ok = $score >= 0.8\n",
            "flow test\n  unless $flag == false\n    return null\n",
            "flow test\n  assert $n > 0, \"positive\"\n",
        ];

        for src in test_cases {
            let ast = parse(src).unwrap();
            let formatted = format(&ast);
            let reparsed = parse(&formatted).unwrap();
            assert_eq!(ast, reparsed, "roundtrip failed for: {}", src);
        }
    }

    #[test]
    fn native_expr_fallback_does_not_collide_with_existing_valid_forms() {
        let src = r#"flow test
  $plain = $score
  $call = len($queue)
  when $ready
    return $plain
  assert ok($plain)
  $ok = $score >= 0.8
"#;
        let ast = parse(src).unwrap();

        let Node::Bind { value, .. } = &ast.body[0] else {
            panic!("expected first bind");
        };
        assert!(matches!(value.as_ref(), Node::Var { .. }));

        let Node::Bind { value, .. } = &ast.body[1] else {
            panic!("expected second bind");
        };
        assert!(matches!(value.as_ref(), Node::Call { op, .. } if op == "len"));

        let Node::When { cond, .. } = &ast.body[2] else {
            panic!("expected when");
        };
        assert!(matches!(cond.as_ref(), Node::Var { .. }));

        let Node::Assert { cond, .. } = &ast.body[3] else {
            panic!("expected assert");
        };
        assert!(matches!(cond.as_ref(), Node::Call { op, .. } if op == "ok"));

        let Node::Bind { value, .. } = &ast.body[4] else {
            panic!("expected native expr bind");
        };
        assert!(matches!(value.as_ref(), Node::Expr { formula, .. } if formula == "score >= 0.8"));
    }

    #[test]
    fn parse_when_with_dotted_access() {
        let src = "flow test\n  when $issue.state == 'opened'\n    return true\n";
        let ast = parse(src).unwrap();
        assert_eq!(ast.body.len(), 1);
        let Node::When { cond, .. } = &ast.body[0] else {
            panic!("expected When node");
        };
        match cond.as_ref() {
            Node::Expr { formula, vars } => {
                // The formula should have issue.state (without $)
                assert!(formula.contains("issue.state"));
                assert!(formula.contains("== 'opened'"));
                // Only the root name should be in vars
                assert_eq!(vars.len(), 1);
                assert!(vars.contains_key("issue"));
            }
            _ => panic!("expected Expr node, got {:?}", cond),
        }
    }

    #[test]
    fn format_native_expr_with_dollar() {
        use crate::format::format;
        let src = "flow test\n  when $count > 3\n    return true\n";
        let ast = parse(src).unwrap();
        let formatted = format(&ast);
        // The formatted output should contain the native expr with $
        assert!(
            formatted.contains("when $count > 3"),
            "formatted: {}",
            formatted
        );
        // Should not fall back to @json
        assert!(!formatted.contains("@json"), "formatted: {}", formatted);
    }

    #[test]
    fn comprehensive_native_expr_integration() {
        use crate::format::format;

        let src = r#"flow test
  $count = 10
  $score = 0.85

  when $count > 5
    $high = true

  $ok = $score >= 0.8

  unless $count == 0
    return true

  assert $score > 0.5, "score too low"

  repeat 3
    until $count == 0
    $count = 1

  return false
"#;

        let ast = parse(src).unwrap();
        let formatted = format(&ast);

        // All native expressions should be preserved with $
        assert!(formatted.contains("when $count > 5"));
        assert!(formatted.contains("$ok = $score >= 0.8"));
        assert!(formatted.contains("unless $count == 0"));
        assert!(formatted.contains("assert $score > 0.5"));
        assert!(formatted.contains("until $count == 0"));

        // Roundtrip test
        let reparsed = parse(&formatted).unwrap();
        assert_eq!(ast, reparsed, "Roundtrip failed");
    }
}

//! Semantic lowering for the lossless Flux-Lang CST.
//!
//! This module deliberately consumes rowan node ownership directly: declarations own blocks,
//! statements own expression children/clauses, and expressions lower recursively from expression
//! nodes. Token text is decoded only at leaves (names, numbers, strings and scalar header fields).
//! It never reconstructs indentation or feeds source back through a second accepting parser.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{
    Branch, DraftAst, FallbackBranch, FlowEffect, MatchCase, Node, Param, RouteCase, SagaStep,
    Selector, SymbolName, ThingKind, ThingRef, TypeRef,
};
use crate::lower_cst::LowerError;
use crate::program::{
    AgentDecl, AgentLoopDecl, ChannelDecl, CompositeLimits, CompositeOpDecl, CompositeOpMeta,
    DatasourceDecl, JourneyDecl, Module, PermissionDecl, Program, TriggerDecl,
};
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use flux_spec::{Effect, Idempotency, Risk};

type DecodeResult<T> = std::result::Result<T, LowerError>;

pub(crate) fn lower_flow(root: &SyntaxNode) -> DecodeResult<DraftAst> {
    let mut flows = root
        .children()
        .filter(|node| node.kind() == SyntaxKind::FLOW_DECL);
    let flow = flows
        .next()
        .ok_or_else(|| error(root, "empty input: expected a `flow` header"))?;
    if flows.next().is_some()
        || root.children().any(|node| {
            !matches!(node.kind(), SyntaxKind::FLOW_DECL | SyntaxKind::DECL)
                || (node.kind() == SyntaxKind::DECL && declaration_keyword(&node) != "goal")
        })
    {
        return Err(error(
            root,
            "expected a single `flow` declaration, not a module",
        ));
    }
    lower_flow_decl(&flow)
}

pub(crate) fn lower_module(root: &SyntaxNode) -> DecodeResult<Module> {
    let mut program = Program::default();
    let mut saw_module_decl = false;
    let mut saw_permissions = false;

    for declaration in root.children() {
        match declaration.kind() {
            SyntaxKind::FLOW_DECL => program.flows.push(lower_flow_decl(&declaration)?),
            SyntaxKind::OP_DECL => {
                program.ops.push(lower_composite_decl(&declaration)?);
                saw_module_decl = true;
            }
            SyntaxKind::DECL => {
                let header = declaration_header(&declaration)?;
                let (keyword, rest) = split_word(&header);
                match keyword {
                    "goal" => {}
                    "permissions" => {
                        if !rest.trim().is_empty() {
                            return Err(error(
                                &declaration,
                                "`permissions` is a singleton declaration and takes no name",
                            ));
                        }
                        if saw_permissions {
                            return Err(error(
                                &declaration,
                                "a program may declare `permissions` only once",
                            ));
                        }
                        program.permissions = Some(lower_permissions(&declaration)?);
                        saw_permissions = true;
                        saw_module_decl = true;
                    }
                    "agent_loop" => {
                        let name = parse_decl_name(rest, "agent_loop", &declaration)?;
                        let body = declaration
                            .children()
                            .find(|node| node.kind() == SyntaxKind::BLOCK)
                            .map(|block| lower_block(&block))
                            .transpose()?
                            .unwrap_or_default();
                        program.agent_loops.push(AgentLoopDecl {
                            name: name.clone(),
                            flow: DraftAst {
                                name: Some(name),
                                body,
                                ..Default::default()
                            },
                        });
                        saw_module_decl = true;
                    }
                    "agent" => {
                        program.agents.push(lower_agent(rest, &declaration)?);
                        saw_module_decl = true;
                    }
                    "channel" => {
                        program.channels.push(lower_channel(rest, &declaration)?);
                        saw_module_decl = true;
                    }
                    "datasource" => {
                        program
                            .datasources
                            .push(lower_datasource(rest, &declaration)?);
                        saw_module_decl = true;
                    }
                    "trigger" => {
                        program.triggers.push(lower_trigger(rest, &declaration)?);
                        saw_module_decl = true;
                    }
                    "journey" => {
                        program.journeys.push(lower_journey(rest, &declaration)?);
                        saw_module_decl = true;
                    }
                    other => {
                        return Err(error(
                            &declaration,
                            format!("unknown top-level declaration: `{other}`"),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    if !saw_module_decl && program.flows.len() == 1 {
        Ok(Module::Flow(program.flows.pop().expect("one flow")))
    } else {
        Ok(Module::Program(program))
    }
}

fn lower_flow_decl(declaration: &SyntaxNode) -> DecodeResult<DraftAst> {
    let header = declaration
        .children()
        .find(|node| node.kind() == SyntaxKind::FLOW_HEADER)
        .ok_or_else(|| error(declaration, "expected a `flow` header"))?;
    let (name, params, returns) = parse_callable_header(&header, "flow", false)?;
    let body = declaration
        .children()
        .filter(|node| node.kind() == SyntaxKind::BLOCK)
        .map(|block| lower_block(&block))
        .collect::<DecodeResult<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(DraftAst {
        name,
        params,
        returns,
        body,
    })
}

fn lower_composite_decl(declaration: &SyntaxNode) -> DecodeResult<CompositeOpDecl> {
    let header = declaration
        .children()
        .find(|node| node.kind() == SyntaxKind::OP_HEADER)
        .ok_or_else(|| error(declaration, "expected an `op` header"))?;
    let (name, params, returns) = parse_callable_header(&header, "op", true)?;
    let name = name.ok_or_else(|| error(&header, "`op` needs a name"))?;
    let mut meta = CompositeOpMeta::default();
    let mut body = Vec::new();
    if let Some(block) = declaration
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
    {
        let mut pending_effect = None;
        for child in block.children() {
            match child.kind() {
                SyntaxKind::OP_META => lower_composite_meta(&mut meta, &child)?,
                SyntaxKind::EFFECT_ANNOT => pending_effect = Some(lower_effect(&child)?),
                kind if is_statement(kind) => {
                    let mut statement = lower_statement(&child)?;
                    if let Some(effect) = pending_effect.take() {
                        statement = attach_effect(statement, effect, &child)?;
                    }
                    body.push(statement);
                }
                _ => {}
            }
        }
        if pending_effect.is_some() {
            return Err(error(&block, "`@effect` must directly precede a bind"));
        }
    }
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

// L-114: statement blocks nest, and this lowerer follows them with one stack frame per level
// (`lower_block` → `lower_statement` → `lower_when`/`lower_each`/… → `lower_block`). The parser's own
// depth guard bounds the trees `parse_cst` builds, but [`crate::parser::Parse`] is a public struct
// with public fields, so a caller can hand `cst_to_draft` a green tree the parser never produced —
// and a deep enough one would overflow the stack and `SIGABRT` here rather than in the parser. Cap
// the walk with the shape the `expr` evaluator uses: a shared counter with an RAII decrement, so it
// can never leak across lowerings on the same thread. Sync-only path, so a thread-local is sound.
//
// The ceiling must admit every tree the parser accepts, so keep it above the parser's own
// `MAX_PARSE_DEPTH` — no `.flux` source can trip this, only a hand-built tree.
//
// It must also be low enough that *reaching* it fits the stack, or the guard aborts at exactly the
// depth it exists to refuse. That is not hypothetical: at the original 256 the descent measured
// ~1837 KiB in a debug build — 90% of the 2 MiB platform-default thread stack (`DEFAULT_MIN_STACK_SIZE`
// on linux-gnu, and what both a libtest thread and a tokio worker get). The guard returned its
// bounded error locally on ~200 KiB of margin and `SIGABRT`ed in CI, where codegen differs just
// enough to spend it. Measured cost is ~7.1 KiB per level (four frames: `lower_block` →
// `lower_statement` → `lower_when`/… → `lower_block`), so 160 costs ~1.1 MiB and leaves ~45% of a
// default stack free. Raising this without re-measuring that descent reintroduces the abort.
const MAX_LOWER_DEPTH: usize = 160;

// Checked at compile time so the two ceilings can never drift into an order where a program the
// parser accepts fails to lower: the guard must bound the adversary, not the author.
const _: () = assert!(MAX_LOWER_DEPTH > crate::parser::MAX_PARSE_DEPTH);

thread_local! {
    static LOWER_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the shared block-nesting depth for its lifetime; decrement on drop (even on an early
/// `?`/`return`), so the counter can never leak across lowerings on the same thread.
struct DepthGuard {
    over: bool,
}

impl DepthGuard {
    fn enter() -> Self {
        let n = LOWER_DEPTH.with(|cell| {
            let n = cell.get() + 1;
            cell.set(n);
            n
        });
        DepthGuard {
            over: n > MAX_LOWER_DEPTH,
        }
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        LOWER_DEPTH.with(|cell| cell.set(cell.get().saturating_sub(1)));
    }
}

fn lower_block(block: &SyntaxNode) -> DecodeResult<Vec<Node>> {
    let depth = DepthGuard::enter();
    if depth.over {
        return Err(error(block, "statement nesting too deep"));
    }
    let mut body = Vec::new();
    let mut pending_effect = None;
    for child in block.children() {
        match child.kind() {
            SyntaxKind::EFFECT_ANNOT => pending_effect = Some(lower_effect(&child)?),
            kind if is_statement(kind) => {
                let mut statement = lower_statement(&child)?;
                if let Some(effect) = pending_effect.take() {
                    statement = attach_effect(statement, effect, &child)?;
                }
                body.push(statement);
            }
            kind => {
                return Err(error(
                    &child,
                    format!("unexpected `{kind:?}` in a statement block"),
                ));
            }
        }
    }
    if pending_effect.is_some() {
        return Err(error(block, "`@effect` must directly precede a bind"));
    }
    Ok(body)
}

fn lower_statement(statement: &SyntaxNode) -> DecodeResult<Node> {
    use SyntaxKind::*;
    match statement.kind() {
        BIND_STMT => lower_bind(statement, false),
        MEMO_STMT => lower_bind(statement, true),
        CALL_STMT => lower_call_statement(statement),
        WHEN_STMT => lower_when(statement),
        UNLESS_STMT => Ok(Node::Unless {
            cond: Box::new(required_expression(statement, "unless condition")?),
            body: lower_first_block(statement)?,
        }),
        EACH_STMT => lower_each(statement),
        REPEAT_STMT => lower_repeat(statement),
        MATCH_STMT => lower_match(statement, false),
        ROUTE_STMT => lower_match(statement, true),
        FALLBACK_STMT => lower_fallback(statement),
        PARALLEL_STMT => lower_parallel(statement, false),
        RACE_STMT => lower_parallel(statement, true),
        LOOP_STMT => lower_loop(statement),
        TIMEOUT_STMT => lower_timeout(statement),
        BUDGET_STMT => lower_budget(statement),
        WITH_TOOLS_STMT => lower_cap_scope(statement),
        RETRY_STMT => lower_retry(statement),
        SEQ_STMT => Ok(Node::Seq {
            bind: parse_optional_arrow(&rest_after_keyword(statement, "seq")?, "seq", statement)?,
            body: lower_first_block(statement)?,
        }),
        CTX_STMT => lower_ctx(statement),
        CTX_APPEND_STMT => lower_ctx_append(statement),
        RETURN_STMT => Ok(Node::Return {
            value: Box::new(
                expression_children(statement)
                    .next()
                    .map(|node| lower_expression(&node))
                    .transpose()?
                    .unwrap_or(Node::Lit {
                        value: serde_json::Value::Null,
                    }),
            ),
        }),
        ASSERT_STMT => lower_assert(statement),
        JSON_ESCAPE => lower_json_escape(statement),
        ONCE_STMT => lower_once(statement),
        CHECKPOINT_STMT => lower_checkpoint(statement),
        AWAIT_STMT => lower_await(statement),
        CONFIRM_STMT => lower_confirm(statement),
        THROTTLE_STMT => lower_throttle(statement),
        DEBOUNCE_STMT => lower_debounce(statement),
        VERIFY_STMT => lower_verify(statement),
        TRY_STMT => lower_try(statement),
        SCOPE_STMT => lower_scope(statement),
        SAGA_STMT => lower_saga(statement),
        PIPE_STMT => Ok(Node::Pipe {
            bind: parse_optional_arrow(&rest_after_keyword(statement, "pipe")?, "pipe", statement)?,
            steps: lower_first_block(statement)?,
        }),
        kind => Err(error(
            statement,
            format!("unsupported statement node `{kind:?}`"),
        )),
    }
}

fn lower_bind(statement: &SyntaxNode, memo: bool) -> DecodeResult<Node> {
    let header = semantic_line(statement);
    let rest = if memo {
        keyword_rest(&header, "memo").ok_or_else(|| error(statement, "expected `memo`"))?
    } else {
        header.as_str()
    };
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('$').unwrap_or(rest);
    let (name, tail) = take_while(rest, is_ident_char);
    if name.is_empty() {
        return Err(error(statement, "empty bind symbol"));
    }
    let tail = tail.trim_start();
    let ty = if let Some(typed) = tail.strip_prefix(':') {
        let eq = typed
            .find('=')
            .ok_or_else(|| error(statement, "expected `=` in typed bind"))?;
        Some(parse_type(&typed[..eq]))
    } else {
        None
    };
    let value = required_expression(statement, "bind value")?;
    if memo {
        Ok(Node::Memo {
            name: name.into(),
            value: Box::new(value),
            ty,
            effect: None,
        })
    } else {
        Ok(Node::Bind {
            name: name.into(),
            value: Box::new(value),
            ty,
            effect: None,
        })
    }
}

fn lower_call_statement(statement: &SyntaxNode) -> DecodeResult<Node> {
    let header = semantic_line(statement);
    if keyword_rest(&header, "do").is_some() {
        let op = statement
            .children()
            .find(|node| node.kind() == SyntaxKind::NAME)
            .map(|node| compact_text(&node))
            .filter(|name| !name.is_empty())
            .ok_or_else(|| error(statement, "`do` expects an operation name"))?;
        let args = statement
            .children()
            .find(|node| node.kind() == SyntaxKind::ARG_LIST)
            .map(|args| lower_arg_list(&args))
            .transpose()?
            .unwrap_or_default();
        Ok(Node::Call { op, args })
    } else {
        let expression = expression_children(statement)
            .next()
            .ok_or_else(|| error(statement, "expected an expression statement"))?;
        lower_expression(&expression)
    }
}

fn lower_when(statement: &SyntaxNode) -> DecodeResult<Node> {
    let cond = required_expression(statement, "when condition")?;
    let then = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
        .map(|block| lower_block(&block))
        .transpose()?
        .unwrap_or_default();
    let otherwise = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::ELSE_CLAUSE)
        .and_then(|clause| {
            clause
                .children()
                .find(|node| node.kind() == SyntaxKind::BLOCK)
        })
        .map(|block| lower_block(&block))
        .transpose()?
        .unwrap_or_default();
    Ok(Node::When {
        cond: Box::new(cond),
        then,
        otherwise,
    })
}

/// `each <item> in <source> [-> [flat] <collect>]`.
///
/// The header is read from the statement's own token structure — its direct token children, with
/// the source expression opaque — never from reconstructed text (L-115). Scanning the line for the
/// first `->` finds one *inside* the source expression's strings, so `each x in "a->b"` and
/// `each part in split($text, "->")` were rejected with a diagnostic naming a collect target the
/// header never wrote, and `format` emitted un-reparseable text for any `Each` whose source carries
/// an arrow. The parser already decided where the arrow is; this reads that decision back.
fn lower_each(statement: &SyntaxNode) -> DecodeResult<Node> {
    let header = direct_header_tokens(statement);
    let mut tokens = header.iter();
    if tokens.next().map(SyntaxToken::text) != Some("each") {
        return Err(error(statement, "expected `each`"));
    }
    let item = symbol_token(tokens.next(), statement, "each item")?;
    if tokens.next().map(SyntaxToken::text) != Some("in") {
        return Err(error(statement, "`each` expects `item in source`"));
    }
    let (collect, flat) = match tokens.next() {
        None => (None, false),
        Some(arrow) if arrow.kind() == SyntaxKind::ARROW => {
            // `flat` is a reserved word, so a *bare* `flat` here is always the modifier — a collect
            // target of that name can only be spelled `$flat`, which is a `VAR` token.
            let mut next = tokens.next();
            let flat = next
                .is_some_and(|token| token.kind() == SyntaxKind::IDENT && token.text() == "flat");
            if flat {
                next = tokens.next();
            }
            (Some(symbol_token(next, statement, "each collect")?), flat)
        }
        Some(token) => {
            return Err(error(
                statement,
                format!("unexpected text in `each` header: `{}`", token.text()),
            ));
        }
    };
    if let Some(extra) = tokens.next() {
        return Err(error(
            statement,
            format!("unexpected text after `each collect`: `{}`", extra.text()),
        ));
    }
    Ok(Node::Each {
        source: Box::new(required_expression(statement, "each source")?),
        item,
        body: lower_first_block(statement)?,
        collect,
        flat,
    })
}

fn lower_repeat(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = positional_rest(statement, "repeat")?;
    let (digits, tail) = take_while(rest.trim_start(), |c| c.is_ascii_digit());
    let max = digits
        .parse::<u32>()
        .map_err(|_| error(statement, "`repeat` expects a count"))?;
    let collect = parse_optional_arrow(tail, "repeat", statement)?;
    let mut header_until = None;
    for option in header_options(statement) {
        match option_label(&option).as_str() {
            "until" => set_option(
                &mut header_until,
                Box::new(option_expression(&option)?),
                &option,
            )?,
            _ => return Err(unknown_option(&option, "repeat")),
        }
    }
    let (body_until, body) = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
        .map(|block| lower_until_block(&block))
        .transpose()?
        .unwrap_or_default();
    Ok(Node::Repeat {
        max,
        until: single_until(header_until, body_until, statement, "repeat")?,
        body,
        collect,
    })
}

/// `until` is spelled either as a header option or as the first body line — never both.
fn single_until(
    header: Option<Box<Node>>,
    body: Option<Box<Node>>,
    statement: &SyntaxNode,
    context: &str,
) -> DecodeResult<Option<Box<Node>>> {
    match (header, body) {
        (Some(_), Some(_)) => Err(error(
            statement,
            format!("`{context}` accepts `until` once, in the header or the body"),
        )),
        (header, body) => Ok(header.or(body)),
    }
}

fn lower_match(statement: &SyntaxNode, routed: bool) -> DecodeResult<Node> {
    let subject = required_expression(
        statement,
        if routed {
            "route selector"
        } else {
            "match subject"
        },
    )?;
    let mut default = Vec::new();
    if routed {
        let mut cases = Vec::new();
        for arm in arm_children(statement) {
            match arm.kind() {
                SyntaxKind::CASE_ARM => {
                    let value = required_expression(&arm, "route case label")?;
                    let Node::Lit {
                        value: serde_json::Value::String(label),
                    } = value
                    else {
                        return Err(error(
                            &arm,
                            "a `route` `case` label must be a string literal",
                        ));
                    };
                    cases.push(RouteCase {
                        label,
                        body: lower_first_block(&arm)?,
                    });
                }
                SyntaxKind::DEFAULT_ARM => default = lower_first_block(&arm)?,
                _ => return Err(error(&arm, "expected `case` or `default`")),
            }
        }
        Ok(Node::Route {
            selector: Box::new(subject),
            cases,
            default,
        })
    } else {
        let mut cases = Vec::new();
        for arm in arm_children(statement) {
            match arm.kind() {
                SyntaxKind::CASE_ARM => cases.push(MatchCase {
                    value: required_expression(&arm, "case value")?,
                    body: lower_first_block(&arm)?,
                }),
                SyntaxKind::DEFAULT_ARM => default = lower_first_block(&arm)?,
                _ => return Err(error(&arm, "expected `case` or `default`")),
            }
        }
        Ok(Node::Match {
            subject: Box::new(subject),
            cases,
            default,
        })
    }
}

fn lower_fallback(statement: &SyntaxNode) -> DecodeResult<Node> {
    if let Some(option) = header_options(statement).first() {
        return Err(unknown_option(option, "fallback"));
    }
    let bind = parse_optional_arrow(
        &positional_rest(statement, "fallback")?,
        "fallback",
        statement,
    )?;
    let mut branches = Vec::new();
    for arm in arm_children(statement) {
        if arm.kind() != SyntaxKind::BRANCH_ARM {
            return Err(error(&arm, "`fallback` accepts `branch` arms only"));
        }
        branches.push(FallbackBranch {
            body: lower_first_block(&arm)?,
        });
    }
    Ok(Node::Fallback { branches, bind })
}

fn lower_parallel(statement: &SyntaxNode, race: bool) -> DecodeResult<Node> {
    let (timeout_ms, bind) = if race {
        let rest = positional_rest(statement, "race")?;
        let rest = rest.trim();
        // `race 5s` spells the timeout positionally; `race timeout: 5s` names it.
        let (mut timeout, tail) = if rest.starts_with("->") || rest.is_empty() {
            (None, rest)
        } else {
            let (value, tail) = take_duration(rest, statement)?;
            (Some(value), tail)
        };
        for option in header_options(statement) {
            match option_label(&option).as_str() {
                "timeout" => set_option(&mut timeout, option_duration(&option)?, &option)?,
                _ => return Err(unknown_option(&option, "race")),
            }
        }
        (
            Some(timeout.ok_or_else(|| error(statement, "`race` expects a timeout"))?),
            parse_optional_arrow(tail, "race", statement)?,
        )
    } else {
        if !positional_rest(statement, "parallel")?.trim().is_empty() {
            return Err(error(statement, "`parallel` takes no header"));
        }
        if let Some(option) = header_options(statement).first() {
            return Err(unknown_option(option, "parallel"));
        }
        (None, None)
    };
    let mut branches = Vec::new();
    for arm in arm_children(statement) {
        if arm.kind() != SyntaxKind::BRANCH_ARM {
            return Err(error(&arm, "expected a `branch $name` arm"));
        }
        let arm_rest = rest_after_keyword(&arm, "branch")?;
        let name = parse_symbol_exact(&arm_rest, &arm, "branch name")?;
        branches.push(Branch {
            name,
            body: lower_first_block(&arm)?,
        });
    }
    if let Some(timeout_ms) = timeout_ms {
        Ok(Node::Race {
            timeout_ms,
            branches,
            bind,
        })
    } else {
        Ok(Node::Parallel { branches })
    }
}

fn lower_loop(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = positional_rest(statement, "loop")?;
    let rest = keyword_rest(rest.trim_start(), "for")
        .ok_or_else(|| error(statement, "`loop` expects `for <ms>`"))?;
    let (for_ms, rest) = take_duration(rest, statement)?;
    // `every` is positional in the legacy header and a named option in the canonical one.
    let (mut every_ms, tail) = match keyword_rest(rest.trim_start(), "every") {
        Some(after) => {
            let (every, tail) = take_duration(after, statement)?;
            (Some(every), tail)
        }
        None => (None, rest),
    };
    let bind = parse_optional_arrow(tail, "loop", statement)?;
    let mut header_until = None;
    for option in header_options(statement) {
        match option_label(&option).as_str() {
            "every" => set_option(&mut every_ms, option_duration(&option)?, &option)?,
            "until" => set_option(
                &mut header_until,
                Box::new(option_expression(&option)?),
                &option,
            )?,
            _ => return Err(unknown_option(&option, "loop")),
        }
    }
    let (body_until, body) = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
        .map(|block| lower_until_block(&block))
        .transpose()?
        .unwrap_or_default();
    Ok(Node::Loop {
        for_ms,
        every_ms: every_ms.ok_or_else(|| error(statement, "`loop` expects `every <ms>`"))?,
        until: single_until(header_until, body_until, statement, "loop")?,
        body,
        bind,
    })
}

fn lower_timeout(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(statement, "timeout")?;
    let (ms, tail) = take_duration(&rest, statement)?;
    Ok(Node::Timeout {
        ms,
        body: lower_first_block(statement)?,
        bind: parse_optional_arrow(tail, "timeout", statement)?,
    })
}

fn lower_budget(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(statement, "budget")?;
    let (digits, tail) = take_while(rest.trim_start(), |c| c.is_ascii_digit());
    let limit = digits
        .parse::<u32>()
        .map_err(|_| error(statement, "`budget` expects a numeric limit"))?;
    Ok(Node::Budget {
        limit,
        body: lower_first_block(statement)?,
        bind: parse_optional_arrow(tail, "budget", statement)?,
    })
}

fn lower_cap_scope(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(statement, "with_tools")?;
    let (value, tail) = parse_setting_prefix(rest.trim_start(), statement, 0)?;
    let tools = string_list(&value, "with_tools", statement)?;
    Ok(Node::CapScope {
        tools,
        body: lower_first_block(statement)?,
        bind: parse_optional_arrow(tail, "with_tools", statement)?,
    })
}

fn lower_retry(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = positional_rest(statement, "retry")?;
    let (digits, mut tail) = take_while(rest.trim_start(), |c| c.is_ascii_digit());
    let max = digits
        .parse::<u32>()
        .map_err(|_| error(statement, "`retry` expects a numeric max"))?;
    let mut backoff = None;
    if let Some(after) = keyword_rest(tail.trim_start(), "backoff") {
        let (name, rest) = take_while(after.trim_start(), is_name_char);
        if name.is_empty() {
            return Err(error(statement, "`retry backoff` expects a name"));
        }
        backoff = Some(name.to_string());
        tail = rest;
    }
    let mut delay_ms = None;
    if let Some(after) = keyword_rest(tail.trim_start(), "delay") {
        let (delay, rest) = take_duration(after, statement)?;
        delay_ms = Some(delay);
        tail = rest;
    }
    for option in header_options(statement) {
        match option_label(&option).as_str() {
            "backoff" => set_option(&mut backoff, option_word(&option)?, &option)?,
            "delay" => set_option(&mut delay_ms, option_duration(&option)?, &option)?,
            _ => return Err(unknown_option(&option, "retry")),
        }
    }
    Ok(Node::Retry {
        max,
        backoff,
        delay_ms,
        body: lower_first_block(statement)?,
        bind: parse_optional_arrow(tail, "retry", statement)?,
    })
}

fn lower_until_block(block: &SyntaxNode) -> DecodeResult<(Option<Box<Node>>, Vec<Node>)> {
    let until = block
        .children()
        .find(|node| node.kind() == SyntaxKind::UNTIL_CLAUSE)
        .map(|clause| required_expression(&clause, "until condition").map(Box::new))
        .transpose()?;
    let mut body = Vec::new();
    let mut pending_effect = None;
    for child in block.children() {
        match child.kind() {
            SyntaxKind::UNTIL_CLAUSE => {
                if !body.is_empty() || pending_effect.is_some() {
                    return Err(error(&child, "`until` must be the first loop-body line"));
                }
            }
            SyntaxKind::EFFECT_ANNOT => pending_effect = Some(lower_effect(&child)?),
            kind if is_statement(kind) => {
                let mut node = lower_statement(&child)?;
                if let Some(effect) = pending_effect.take() {
                    node = attach_effect(node, effect, &child)?;
                }
                body.push(node);
            }
            _ => return Err(error(&child, "unexpected loop-body node")),
        }
    }
    if pending_effect.is_some() {
        return Err(error(block, "`@effect` must directly precede a bind"));
    }
    Ok((until, body))
}

fn lower_ctx(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(statement, "ctx")?;
    let name = parse_symbol_exact(&rest, statement, "ctx name")?;
    let mut purpose = None;
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    let mut budget = None;
    if let Some(block) = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
    {
        for entry in block.children() {
            if entry.kind() != SyntaxKind::CTX_ENTRY {
                return Err(error(&entry, "expected a context attribute"));
            }
            let text = semantic_line(&entry);
            let (key, value) = split_word(&text);
            match key {
                "purpose" => purpose = Some(parse_string_exact(value, &entry, "purpose")?),
                "budget" => {
                    budget = Some(
                        value
                            .trim()
                            .parse::<u64>()
                            .map_err(|_| error(&entry, "invalid `budget`"))?,
                    )
                }
                "include" => include = parse_symbol_list(value, &entry)?,
                "exclude" => exclude = parse_symbol_list(value, &entry)?,
                _ => return Err(error(&entry, format!("unknown `ctx` attribute: `{text}`"))),
            }
        }
    }
    Ok(Node::Ctx {
        name,
        purpose,
        include,
        exclude,
        budget,
    })
}

fn lower_ctx_append(statement: &SyntaxNode) -> DecodeResult<Node> {
    let header = semantic_line(statement);
    let (lhs, _) = header
        .split_once("+=")
        .ok_or_else(|| error(statement, "context append expects `name += values`"))?;
    let ctx = parse_symbol_exact(lhs, statement, "context append name")?;
    let mut add = Vec::new();
    for expression in expression_children(statement) {
        match lower_expression(&expression)? {
            Node::Var { name } => add.push(name),
            _ => return Err(error(&expression, "context append accepts symbols only")),
        }
    }
    Ok(Node::CtxAppend { ctx, add })
}

fn lower_assert(statement: &SyntaxNode) -> DecodeResult<Node> {
    let cond = required_expression(statement, "assert condition")?;
    let message = if has_direct_token(statement, SyntaxKind::COMMA) {
        header_tokens(statement)
            .into_iter()
            .rev()
            .find(|(kind, _)| *kind == SyntaxKind::STRING)
            .map(|(_, text)| parse_string_token(&text, statement))
            .transpose()?
    } else {
        None
    };
    Ok(Node::Assert {
        cond: Box::new(cond),
        message,
    })
}

fn lower_json_escape(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(statement, "@json")?;
    let value = parse_json_exact(rest.trim(), statement)?;
    serde_json::from_value(value)
        .map_err(|err| error(statement, format!("invalid `@json` node: {err}")))
}

fn lower_once(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(statement, "once")?;
    let (label, tail) = parse_string_prefix(rest.trim_start(), statement, "once")?;
    Ok(Node::Once {
        label,
        body: lower_first_block(statement)?,
        bind: parse_optional_arrow(tail, "once", statement)?,
    })
}

fn lower_checkpoint(statement: &SyntaxNode) -> DecodeResult<Node> {
    Ok(Node::Checkpoint {
        label: parse_string_exact(
            &rest_after_keyword(statement, "checkpoint")?,
            statement,
            "checkpoint",
        )?,
    })
}

fn lower_await(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = positional_rest(statement, "await")?;
    let binding = if rest.trim_start().starts_with('"') {
        None
    } else {
        let (lhs, _) = rest
            .split_once('=')
            .ok_or_else(|| error(statement, "`await` binding expects `name = source`"))?;
        let name = lhs.split_once(':').map_or(lhs, |(name, _)| name);
        Some(parse_symbol_exact(name, statement, "await binding")?)
    };
    let tokens = header_tokens(statement);
    let as_type = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::NAME)
        .map(|node| parse_type(&compact_text(&node)));
    let source = tokens
        .iter()
        .find(|(kind, _)| *kind == SyntaxKind::STRING)
        .map(|(_, text)| parse_string_token(text, statement))
        .transpose()?
        .ok_or_else(|| error(statement, "`await` expects a quoted source"))?;
    let mut condition = expression_children(statement)
        .next()
        .map(|node| lower_expression(&node).map(Box::new))
        .transpose()?;
    for option in header_options(statement) {
        match option_label(&option).as_str() {
            "when" => set_option(
                &mut condition,
                Box::new(option_expression(&option)?),
                &option,
            )?,
            _ => return Err(unknown_option(&option, "await")),
        }
    }
    Ok(Node::Await {
        binding,
        source,
        as_type,
        condition,
    })
}

fn lower_confirm(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = positional_rest(statement, "confirm")?;
    let (message, tail) = parse_string_prefix(rest.trim_start(), statement, "confirm")?;
    let tail = tail.trim();
    let mut risk = if tail.is_empty() {
        None
    } else {
        let after = keyword_rest(tail, "risk")
            .ok_or_else(|| error(statement, "unexpected text in `confirm` header"))?;
        let (risk, extra) = take_while(after.trim_start(), is_name_char);
        if risk.is_empty() || !extra.trim().is_empty() {
            return Err(error(statement, "invalid `confirm risk` level"));
        }
        Some(risk.to_string())
    };
    for option in header_options(statement) {
        match option_label(&option).as_str() {
            "risk" => set_option(&mut risk, option_word(&option)?, &option)?,
            _ => return Err(unknown_option(&option, "confirm")),
        }
    }
    Ok(Node::Confirm {
        message,
        risk,
        body: lower_first_block(statement)?,
    })
}

fn lower_throttle(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = positional_rest(statement, "throttle")?;
    let (name, rest) = parse_string_prefix(rest.trim_start(), statement, "throttle")?;
    let rest = rest.trim();
    let (mut max, mut window_ms) = (None, None);
    if !rest.is_empty() {
        let (digits, rest) = take_while(rest, |c| c.is_ascii_digit());
        max = Some(
            digits
                .parse::<u32>()
                .map_err(|_| error(statement, "`throttle` expects a numeric max"))?,
        );
        let rest = keyword_rest(rest.trim_start(), "per")
            .ok_or_else(|| error(statement, "`throttle` expects `per <window_ms>`"))?;
        let (window, tail) = take_duration(rest, statement)?;
        if !tail.trim().is_empty() {
            return Err(error(statement, "trailing text after `throttle` header"));
        }
        window_ms = Some(window);
    }
    for option in header_options(statement) {
        match option_label(&option).as_str() {
            "max" => set_option(&mut max, option_count(&option)?, &option)?,
            "per" => set_option(&mut window_ms, option_duration(&option)?, &option)?,
            _ => return Err(unknown_option(&option, "throttle")),
        }
    }
    Ok(Node::Throttle {
        name,
        max: max.ok_or_else(|| error(statement, "`throttle` expects a max"))?,
        window_ms: window_ms.ok_or_else(|| error(statement, "`throttle` expects a window"))?,
        body: lower_first_block(statement)?,
    })
}

fn lower_debounce(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = positional_rest(statement, "debounce")?;
    let (name, rest) = parse_string_prefix(rest.trim_start(), statement, "debounce")?;
    let rest = rest.trim();
    let mut wait_ms = None;
    if !rest.is_empty() {
        let (wait, tail) = take_duration(rest, statement)?;
        if !tail.trim().is_empty() {
            return Err(error(statement, "trailing text after `debounce` header"));
        }
        wait_ms = Some(wait);
    }
    for option in header_options(statement) {
        match option_label(&option).as_str() {
            "wait" => set_option(&mut wait_ms, option_duration(&option)?, &option)?,
            _ => return Err(unknown_option(&option, "debounce")),
        }
    }
    Ok(Node::Debounce {
        name,
        wait_ms: wait_ms.ok_or_else(|| error(statement, "`debounce` expects a wait"))?,
        body: lower_first_block(statement)?,
    })
}

fn lower_verify(statement: &SyntaxNode) -> DecodeResult<Node> {
    let mut expressions = expression_children(statement);
    let cmd = expressions
        .next()
        .ok_or_else(|| error(statement, "`verify` expects a command expression"))?;
    let expect = expressions
        .next()
        .ok_or_else(|| error(statement, "`verify` expects an expected expression"))?;
    let message = if has_direct_token(statement, SyntaxKind::COLON) {
        header_tokens(statement)
            .into_iter()
            .rev()
            .find(|(kind, _)| *kind == SyntaxKind::STRING)
            .map(|(_, text)| parse_string_token(&text, statement))
            .transpose()?
    } else {
        None
    };
    Ok(Node::Verify {
        cmd: Box::new(lower_expression(&cmd)?),
        expect: Box::new(lower_expression(&expect)?),
        message,
    })
}

fn lower_try(statement: &SyntaxNode) -> DecodeResult<Node> {
    let body = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
        .map(|block| lower_block(&block))
        .transpose()?
        .unwrap_or_default();
    let clause = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::CATCH_CLAUSE);
    let catch = clause
        .as_ref()
        .map(|clause| rest_after_keyword(clause, "catch"))
        .transpose()?
        .filter(|rest| !rest.trim().is_empty())
        .map(|rest| parse_symbol_exact(&rest, statement, "catch binding"))
        .transpose()?;
    let handler = clause
        .and_then(|clause| {
            clause
                .children()
                .find(|node| node.kind() == SyntaxKind::BLOCK)
        })
        .map(|block| lower_block(&block))
        .transpose()?
        .unwrap_or_default();
    Ok(Node::Try {
        body,
        catch,
        handler,
    })
}

fn lower_scope(statement: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(statement, "scope")?;
    let bind = if rest.trim().is_empty() {
        None
    } else {
        let (lhs, _) = rest
            .split_once('=')
            .ok_or_else(|| error(statement, "`scope` expects `name = acquire`"))?;
        Some(parse_symbol_exact(lhs, statement, "scope binding")?)
    };
    let acquire = expression_children(statement)
        .next()
        .map(|node| lower_expression(&node).map(Box::new))
        .transpose()?;
    let body = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
        .map(|block| lower_block(&block))
        .transpose()?
        .unwrap_or_default();
    let finally = statement
        .children()
        .find(|node| node.kind() == SyntaxKind::FINALLY_CLAUSE)
        .and_then(|clause| {
            clause
                .children()
                .find(|node| node.kind() == SyntaxKind::BLOCK)
        })
        .map(|block| lower_block(&block))
        .transpose()?
        .unwrap_or_default();
    Ok(Node::Scope {
        acquire,
        bind,
        body,
        finally,
    })
}

fn lower_saga(statement: &SyntaxNode) -> DecodeResult<Node> {
    let mut steps = Vec::new();
    let mut pending: Option<SagaStep> = None;
    for arm in arm_children(statement) {
        match arm.kind() {
            SyntaxKind::STEP_ARM => {
                if let Some(step) = pending.take() {
                    steps.push(step);
                }
                pending = Some(SagaStep {
                    body: lower_first_block(&arm)?,
                    undo: Vec::new(),
                });
            }
            SyntaxKind::UNDO_CLAUSE => {
                let Some(step) = pending.as_mut() else {
                    return Err(error(&arm, "`undo` must follow a `step`"));
                };
                step.undo = lower_first_block(&arm)?;
            }
            _ => return Err(error(&arm, "expected `step` or `undo` in saga")),
        }
    }
    if let Some(step) = pending {
        steps.push(step);
    }
    Ok(Node::Saga { steps })
}

fn lower_first_block(node: &SyntaxNode) -> DecodeResult<Vec<Node>> {
    node.children()
        .find(|child| child.kind() == SyntaxKind::BLOCK)
        .map(|block| lower_block(&block))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn arm_children(node: &SyntaxNode) -> Vec<SyntaxNode> {
    node.children()
        .find(|child| child.kind() == SyntaxKind::BLOCK)
        .map(|block| block.children().collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

fn required_expression(node: &SyntaxNode, what: &str) -> DecodeResult<Node> {
    expression_children(node)
        .next()
        .ok_or_else(|| error(node, format!("expected {what}")))
        .and_then(|expression| lower_expression(&expression))
}

fn expression_children(node: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> + '_ {
    node.children().filter(|child| is_expression(child.kind()))
}

fn is_expression(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        CALL_EXPR
            | VAR_EXPR
            | FIELD_EXPR
            | LIT_EXPR
            | FMT_EXPR
            | PARSE_EXPR
            | PEEK_EXPR
            | THING_EXPR
            | JSON_EXPR
            | OBJ_EXPR
            | LIST_EXPR
            | BIN_EXPR
            | UNARY_EXPR
            | PAREN_EXPR
    )
}

fn lower_expression(expression: &SyntaxNode) -> DecodeResult<Node> {
    use SyntaxKind::*;
    match expression.kind() {
        CALL_EXPR => {
            let op = expression
                .children()
                .find(|node| node.kind() == NAME)
                .map(|node| compact_text(&node))
                .filter(|name| !name.is_empty())
                .ok_or_else(|| error(expression, "call expression needs an operation name"))?;
            let args = expression
                .children()
                .find(|node| node.kind() == ARG_LIST)
                .map(|args| lower_arg_list(&args))
                .transpose()?
                .unwrap_or_default();
            Ok(Node::Call { op, args })
        }
        FMT_EXPR => {
            let args = expression
                .children()
                .find(|node| node.kind() == ARG_LIST)
                .map(|args| lower_arg_list(&args))
                .transpose()?
                .unwrap_or_default();
            match args.as_slice() {
                [Node::Lit {
                    value: serde_json::Value::String(template),
                }] => Ok(Node::Fmt {
                    template: template.clone(),
                }),
                _ => Err(error(
                    expression,
                    "fmt(...) takes a single string-literal template",
                )),
            }
        }
        PARSE_EXPR => {
            let args = expression
                .children()
                .find(|node| node.kind() == ARG_LIST)
                .ok_or_else(|| error(expression, "`parse` needs arguments"))?;
            let entries = lower_arg_entries(&args)?;
            let Some(ArgEntry::PositionalNode(_, value)) = entries.first() else {
                return Err(error(
                    expression,
                    "`parse(…)` expects `<value>, as: \"type\"`",
                ));
            };
            let Some(ArgEntry::Named(
                as_name,
                Node::Lit {
                    value: serde_json::Value::String(as_type),
                },
            )) = entries.get(1)
            else {
                return Err(error(
                    expression,
                    "`parse(…)` expects `<value>, as: \"type\"`",
                ));
            };
            if entries.len() != 2 || as_name != "as" {
                return Err(error(
                    expression,
                    "`parse(…)` expects `<value>, as: \"type\"`",
                ));
            }
            Ok(Node::Parse {
                value: Box::new(value.clone()),
                as_type: as_type.clone(),
            })
        }
        VAR_EXPR => {
            let text = expression
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .find(|token| matches!(token.kind(), SyntaxKind::VAR | SyntaxKind::IDENT))
                .map(|token| token.text().to_string())
                .ok_or_else(|| error(expression, "empty symbol after `$`"))?;
            Ok(Node::Var {
                name: symbol(&text),
            })
        }
        FIELD_EXPR => lower_field(expression),
        LIT_EXPR => lower_literal(expression),
        PEEK_EXPR => {
            let name = header_tokens(expression)
                .into_iter()
                .find(|(kind, text)| {
                    matches!(kind, SyntaxKind::VAR | SyntaxKind::IDENT) && text != "peek"
                })
                .map(|(_, text)| symbol(&text))
                .ok_or_else(|| error(expression, "`peek` expects a name"))?;
            Ok(Node::Peek { name })
        }
        THING_EXPR => lower_thing(expression),
        JSON_EXPR => {
            let rest = rest_after_keyword(expression, "@json")?;
            let value = parse_json_exact(rest.trim(), expression)?;
            serde_json::from_value(value)
                .map_err(|err| error(expression, format!("invalid `@json` node: {err}")))
        }
        OBJ_EXPR => lower_object(expression),
        LIST_EXPR => lower_list(expression),
        BIN_EXPR | PAREN_EXPR => lower_formula(expression),
        UNARY_EXPR => lower_unary(expression),
        NAME => Err(error(
            expression,
            format!("unexpected token: `{}`", compact_text(expression)),
        )),
        kind => Err(error(
            expression,
            format!("unsupported expression node `{kind:?}`"),
        )),
    }
}

fn lower_arg_list(args: &SyntaxNode) -> DecodeResult<Vec<Node>> {
    let entries = lower_arg_entries(args)?;
    let has_named = entries
        .iter()
        .any(|entry| matches!(entry, ArgEntry::Named(..)));
    let all_bare_puns = entries.len() > 1
        && entries.iter().all(|entry| match entry {
            ArgEntry::PositionalNode(node, Node::Var { .. }) => compact_text(node)
                .chars()
                .next()
                .is_some_and(|ch| ch != '$'),
            _ => false,
        });

    if has_named || all_bare_puns {
        let mut fields = BTreeMap::new();
        for entry in entries {
            let (name, value) = match entry {
                ArgEntry::Named(name, value) => (name, value),
                ArgEntry::PositionalNode(node, Node::Var { name })
                    if !compact_text(&node).starts_with('$') =>
                {
                    (name.0.clone(), Node::Var { name })
                }
                ArgEntry::PositionalNode(_, _) => {
                    return Err(error(
                        args,
                        "named calls may only mix `name: value` entries with bare identifier puns",
                    ));
                }
            };
            if fields.insert(name.clone(), Box::new(value)).is_some() {
                return Err(error(args, format!("duplicate named argument `{name}`")));
            }
        }
        Ok(vec![Node::Obj { fields }])
    } else {
        Ok(entries
            .into_iter()
            .map(|entry| match entry {
                ArgEntry::PositionalNode(_, value) => Ok(value),
                ArgEntry::Named(name, _) => {
                    Err(error(args, format!("unexpected named argument `{name}`")))
                }
            })
            .collect::<DecodeResult<Vec<_>>>()?)
    }
}

enum ArgEntry {
    PositionalNode(SyntaxNode, Node),
    Named(String, Node),
}

fn lower_arg_entries(args: &SyntaxNode) -> DecodeResult<Vec<ArgEntry>> {
    args.children()
        .filter_map(|node| match node.kind() {
            SyntaxKind::NAMED_ARG => Some((true, node)),
            kind if is_expression(kind) => Some((false, node)),
            _ => None,
        })
        .map(|(named, node)| {
            if named {
                let name = node
                    .children()
                    .find(|child| child.kind() == SyntaxKind::NAME)
                    .map(|child| compact_text(&child))
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| error(&node, "named argument needs a name"))?;
                let value = required_expression(&node, "named argument value")?;
                Ok(ArgEntry::Named(name, value))
            } else {
                let value = lower_expression(&node)?;
                Ok(ArgEntry::PositionalNode(node, value))
            }
        })
        .collect()
}

fn lower_field(expression: &SyntaxNode) -> DecodeResult<Node> {
    let base_node = expression_children(expression)
        .next()
        .ok_or_else(|| error(expression, "field access needs a base expression"))?;
    let base = lower_expression(&base_node)?;
    let direct_tokens = expression.children_with_tokens().filter_map(|element| {
        element.into_token().and_then(|token| {
            (!matches!(
                token.kind(),
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE
            ))
            .then(|| (token.kind(), token.text().to_string()))
        })
    });
    let mut segment = String::new();
    let mut optional = false;
    let mut bracket_index = false;
    for (kind, text) in direct_tokens {
        match kind {
            SyntaxKind::DOT => segment.push('.'),
            SyntaxKind::IDENT => segment.push_str(&text),
            SyntaxKind::NUMBER => {
                if bracket_index {
                    segment.push('.');
                }
                segment.push_str(&text);
            }
            SyntaxKind::STRING if bracket_index => {
                let key = serde_json::from_str::<String>(&text)
                    .map_err(|err| error(expression, format!("invalid quoted field key: {err}")))?;
                segment.push('[');
                segment.push_str(
                    &serde_json::to_string(&key)
                        .expect("serializing a Rust string to JSON cannot fail"),
                );
                segment.push(']');
            }
            SyntaxKind::L_BRACK => bracket_index = true,
            SyntaxKind::R_BRACK => bracket_index = false,
            SyntaxKind::QUESTION => optional = true,
            _ => {}
        }
    }
    if segment == "." || segment.is_empty() {
        return Err(error(expression, "expected a field name after `.`"));
    }
    match base {
        Node::Jq {
            mut path, input, ..
        } => {
            path.push_str(&segment);
            Ok(Node::Jq {
                path,
                input,
                optional,
            })
        }
        input => Ok(Node::Jq {
            path: if segment.starts_with('[') {
                format!(".{segment}")
            } else {
                segment
            },
            input: Box::new(input),
            optional,
        }),
    }
}

fn lower_literal(expression: &SyntaxNode) -> DecodeResult<Node> {
    let text = semantic_line(expression);
    let value = match text.as_str() {
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        "null" => serde_json::Value::Null,
        _ => parse_json_exact(&text, expression)?,
    };
    Ok(Node::Lit { value })
}

fn lower_unary(expression: &SyntaxNode) -> DecodeResult<Node> {
    let text = semantic_line(expression);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        if value.is_number() {
            return Ok(Node::Lit { value });
        }
    }
    lower_formula(expression)
}

fn lower_formula(expression: &SyntaxNode) -> DecodeResult<Node> {
    let text = semantic_line(expression);
    native_formula(&text).ok_or_else(|| error(expression, "expected a native expression"))
}

fn native_formula(text: &str) -> Option<Node> {
    let mut names = BTreeSet::new();
    let mut formula = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$'
            && chars
                .peek()
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == '_')
        {
            let mut name = String::new();
            while let Some(next) = chars.peek() {
                if next.is_ascii_alphanumeric() || matches!(next, '_' | '.') {
                    name.push(*next);
                    chars.next();
                } else {
                    break;
                }
            }
            names.insert(name.split('.').next().unwrap_or(&name).to_string());
            formula.push_str(&name);
        } else {
            formula.push(ch);
        }
    }
    if names.is_empty() {
        return None;
    }
    let vars = names
        .into_iter()
        .map(|name| {
            let value = Box::new(Node::Var {
                name: name.as_str().into(),
            });
            (name, value)
        })
        .collect();
    Some(Node::Expr { formula, vars })
}

fn lower_object(expression: &SyntaxNode) -> DecodeResult<Node> {
    let text = compact_semantic_text(expression);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        return Ok(Node::Lit { value });
    }
    let mut fields = BTreeMap::new();
    for field in expression
        .children()
        .filter(|node| node.kind() == SyntaxKind::OBJ_FIELD)
    {
        let key_node = field
            .children()
            .find(|node| node.kind() == SyntaxKind::NAME)
            .ok_or_else(|| error(&field, "expected an object key"))?;
        let raw_key = semantic_line(&key_node);
        let key = if raw_key.starts_with('"') {
            parse_string_exact(&raw_key, &key_node, "object key")?
        } else {
            raw_key
        };
        let value = expression_children(&field)
            .next()
            .map(|node| lower_expression(&node))
            .transpose()?
            .unwrap_or_else(|| Node::Var {
                name: key.as_str().into(),
            });
        fields.insert(key, Box::new(value));
    }
    Ok(Node::Obj { fields })
}

fn lower_list(expression: &SyntaxNode) -> DecodeResult<Node> {
    let text = compact_semantic_text(expression);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        return Ok(Node::Lit { value });
    }
    Ok(Node::List {
        items: expression_children(expression)
            .map(|node| lower_expression(&node))
            .collect::<DecodeResult<Vec<_>>>()?,
    })
}

fn lower_thing(expression: &SyntaxNode) -> DecodeResult<Node> {
    let rest = rest_after_keyword(expression, "thing")?;
    let (kind_word, rest) = take_while(rest.trim_start(), is_ident_char);
    let (kind, rest) = if kind_word == "custom" {
        let (name, rest) = parse_string_prefix(rest.trim_start(), expression, "thing custom kind")?;
        (ThingKind::Custom(name), rest)
    } else {
        let kind = match kind_word {
            "context" => ThingKind::Context,
            "file" => ThingKind::File,
            "person" => ThingKind::Person,
            "ticket" => ThingKind::Ticket,
            "email" => ThingKind::Email,
            "repo" => ThingKind::Repo,
            "dataset" => ThingKind::Dataset,
            "calendar_event" => ThingKind::CalendarEvent,
            "url" => ThingKind::Url,
            "secret" => ThingKind::Secret,
            other => {
                return Err(error(
                    expression,
                    format!("unknown `thing` kind: `{other}`"),
                ));
            }
        };
        (kind, rest)
    };
    let (selector_word, rest) = take_while(rest.trim_start(), is_ident_char);
    let (value, tail) = parse_string_prefix(rest.trim_start(), expression, "thing selector")?;
    if !tail.trim().is_empty() {
        return Err(error(expression, "trailing text after `thing` selector"));
    }
    let selector = match selector_word {
        "id" => Selector::Id(value),
        "name" => Selector::Name(value),
        "path" => Selector::Path(value),
        "query" => Selector::Query(value),
        "key" => Selector::Key(value),
        other => {
            return Err(error(
                expression,
                format!("unknown `thing` selector: `{other}`"),
            ));
        }
    };
    Ok(Node::Thing {
        thing: ThingRef { kind, selector },
    })
}

// ---------------------------------------------------------------------------
// Module declarations and metadata
// ---------------------------------------------------------------------------

fn declaration_keyword(declaration: &SyntaxNode) -> String {
    declaration
        .children()
        .find(|node| node.kind() == SyntaxKind::DECL_HEADER)
        .map(|header| split_word(&semantic_line(&header)).0.to_string())
        .unwrap_or_default()
}

fn declaration_header(declaration: &SyntaxNode) -> DecodeResult<String> {
    declaration
        .children()
        .find(|node| node.kind() == SyntaxKind::DECL_HEADER)
        .map(|header| semantic_line(&header))
        .ok_or_else(|| error(declaration, "declaration is missing its header"))
}

fn declaration_attrs(declaration: &SyntaxNode) -> DecodeResult<Vec<(SyntaxNode, String, String)>> {
    let Some(block) = declaration
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
    else {
        return Ok(Vec::new());
    };
    block
        .children()
        .map(|attribute| {
            if attribute.kind() != SyntaxKind::DECL_ATTR {
                return Err(error(&attribute, "expected a `key value` attribute"));
            }
            let text = semantic_line(&attribute);
            let (key, value) = split_word(&text);
            if key.is_empty() {
                return Err(error(&attribute, "expected a `key value` attribute"));
            }
            Ok((attribute, key.to_string(), value.trim_start().to_string()))
        })
        .collect()
}

fn lower_permissions(declaration: &SyntaxNode) -> DecodeResult<PermissionDecl> {
    let mut allow = None;
    let mut saw_allow = false;
    let mut deny = None;
    for (node, key, value) in declaration_attrs(declaration)? {
        match key.as_str() {
            "allow" => {
                if saw_allow {
                    return Err(error(&node, "duplicate permissions attribute `allow`"));
                }
                allow = Some(string_list(
                    &parse_setting_exact(&value, &node)?,
                    "allow",
                    &node,
                )?);
                saw_allow = true;
            }
            "deny" => {
                if deny.is_some() {
                    return Err(error(&node, "duplicate permissions attribute `deny`"));
                }
                deny = Some(string_list(
                    &parse_setting_exact(&value, &node)?,
                    "deny",
                    &node,
                )?);
            }
            other => {
                return Err(error(
                    &node,
                    format!("unknown permissions attribute `{other}`"),
                ));
            }
        }
    }
    Ok(PermissionDecl {
        allow,
        deny: deny.unwrap_or_default(),
    })
}

fn lower_agent(rest: &str, declaration: &SyntaxNode) -> DecodeResult<AgentDecl> {
    let name = parse_decl_name(rest, "agent", declaration)?;
    let mut agent = AgentDecl {
        name,
        ..Default::default()
    };
    let mut settings = serde_json::Map::new();
    let mut allow = None;
    let mut saw_allow = false;
    let mut deny = None;
    for (node, key, value) in declaration_attrs(declaration)? {
        match key.as_str() {
            "model" => agent.model = Some(parse_string_setting(&value, &node, "model")?),
            "tools" => {
                agent.tools = string_list(&parse_setting_exact(&value, &node)?, "tools", &node)?
            }
            "datasources" => {
                agent.datasources =
                    string_list(&parse_setting_exact(&value, &node)?, "datasources", &node)?
            }
            "description" => {
                agent.description = Some(parse_string_setting(&value, &node, "description")?)
            }
            "loop" => agent.agent_loop = Some(parse_string_setting(&value, &node, "loop")?),
            "allow" => {
                if saw_allow {
                    return Err(error(&node, "duplicate agent attribute `allow`"));
                }
                allow = Some(string_list(
                    &parse_setting_exact(&value, &node)?,
                    "allow",
                    &node,
                )?);
                saw_allow = true;
            }
            "deny" => {
                if deny.is_some() {
                    return Err(error(&node, "duplicate agent attribute `deny`"));
                }
                deny = Some(string_list(
                    &parse_setting_exact(&value, &node)?,
                    "deny",
                    &node,
                )?);
            }
            _ => {
                settings.insert(key, parse_setting_exact(&value, &node)?);
            }
        }
    }
    if saw_allow || deny.is_some() {
        agent.permissions = Some(PermissionDecl {
            allow,
            deny: deny.unwrap_or_default(),
        });
    }
    agent.settings = settings_value(settings);
    Ok(agent)
}

fn lower_channel(rest: &str, declaration: &SyntaxNode) -> DecodeResult<ChannelDecl> {
    let name = parse_decl_name(rest, "channel", declaration)?;
    let mut kind = None;
    let mut settings = serde_json::Map::new();
    for (node, key, value) in declaration_attrs(declaration)? {
        if key == "kind" {
            kind = Some(parse_string_setting(&value, &node, "kind")?);
        } else {
            settings.insert(key, parse_setting_exact(&value, &node)?);
        }
    }
    Ok(ChannelDecl {
        name: name.clone(),
        kind: kind.unwrap_or(name),
        settings: settings_value(settings),
    })
}

fn lower_datasource(rest: &str, declaration: &SyntaxNode) -> DecodeResult<DatasourceDecl> {
    let name = parse_decl_name(rest, "datasource", declaration)?;
    let mut kind = None;
    let mut path = None;
    let mut settings = serde_json::Map::new();
    for (node, key, value) in declaration_attrs(declaration)? {
        match key.as_str() {
            "kind" => kind = Some(parse_string_setting(&value, &node, "kind")?),
            "path" => path = Some(parse_string_setting(&value, &node, "path")?),
            _ => {
                settings.insert(key, parse_setting_exact(&value, &node)?);
            }
        }
    }
    Ok(DatasourceDecl {
        name: name.clone(),
        kind: kind.unwrap_or(name),
        path,
        settings: settings_value(settings),
    })
}

fn lower_trigger(rest: &str, declaration: &SyntaxNode) -> DecodeResult<TriggerDecl> {
    let name = parse_decl_name(rest, "trigger", declaration)?;
    let mut on = None;
    let mut run = None;
    let mut agent = None;
    for (node, key, value) in declaration_attrs(declaration)? {
        match key.as_str() {
            "on" => on = Some(parse_string_setting(&value, &node, "on")?),
            "run" => run = Some(parse_string_setting(&value, &node, "run")?),
            "agent" => agent = Some(parse_string_setting(&value, &node, "agent")?),
            other => return Err(error(&node, format!("unknown trigger attribute `{other}`"))),
        }
    }
    let run = run.unwrap_or_default();
    if agent.is_none() && run.is_empty() {
        return Err(error(
            declaration,
            "a `trigger` needs a `run` journey/flow name or an `agent` to run",
        ));
    }
    Ok(TriggerDecl {
        name,
        on: on.ok_or_else(|| error(declaration, "a `trigger` needs an `on` event label"))?,
        run,
        agent,
    })
}

fn lower_journey(rest: &str, declaration: &SyntaxNode) -> DecodeResult<JourneyDecl> {
    let name = parse_decl_name(rest, "journey", declaration)?;
    let block = declaration
        .children()
        .find(|node| node.kind() == SyntaxKind::BLOCK)
        .ok_or_else(|| error(declaration, format!("journey `{name}` needs a `flow` body")))?;
    let mut agent = None;
    let mut flow = None;
    for child in block.children() {
        match child.kind() {
            SyntaxKind::DECL_ATTR => {
                let text = semantic_line(&child);
                let (key, value) = split_word(&text);
                if key != "agent" {
                    return Err(error(
                        &child,
                        format!("unexpected line in journey `{name}`: `{text}`"),
                    ));
                }
                agent = Some(parse_string_setting(value, &child, "agent")?);
            }
            SyntaxKind::FLOW_DECL => {
                let mut lowered = lower_flow_decl(&child)?;
                if lowered.name.is_none() {
                    lowered.name = Some(name.clone());
                }
                flow = Some(lowered);
            }
            _ => return Err(error(&child, "unexpected journey body node")),
        }
    }
    Ok(JourneyDecl {
        name: name.clone(),
        agent,
        flow: flow
            .ok_or_else(|| error(declaration, format!("journey `{name}` needs a `flow` body")))?,
    })
}

fn lower_composite_meta(meta: &mut CompositeOpMeta, node: &SyntaxNode) -> DecodeResult<()> {
    let text = semantic_line(node);
    let (key, value) = split_word(&text);
    match key {
        "description" => meta.description = parse_string_setting(value, node, "description")?,
        "risk" => {
            meta.risk = match normalize(&parse_string_setting(value, node, "risk")?).as_str() {
                "low" => Risk::Low,
                "medium" => Risk::Medium,
                "high" => Risk::High,
                "destructive" => Risk::Destructive,
                other => return Err(error(node, format!("unknown risk `{other}`"))),
            }
        }
        "idempotency" => {
            meta.idempotency =
                match normalize(&parse_string_setting(value, node, "idempotency")?).as_str() {
                    "idempotent" => Idempotency::Idempotent,
                    "non_idempotent" => Idempotency::NonIdempotent,
                    "conditional" => Idempotency::Conditional,
                    other => return Err(error(node, format!("unknown idempotency `{other}`"))),
                }
        }
        "effects" => meta.effects = parse_effects(&parse_setting_exact(value, node)?, node)?,
        "limits" => meta.limits = parse_limits(&parse_setting_exact(value, node)?, node)?,
        "expose" => {
            meta.expose = parse_setting_exact(value, node)?
                .as_bool()
                .ok_or_else(|| error(node, "`expose` must be a boolean"))?
        }
        "view" => meta.view = Some(parse_string_setting(value, node, "view")?),
        _ => return Err(error(node, format!("unknown composite metadata `{key}`"))),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Leaf decoding helpers
// ---------------------------------------------------------------------------

fn parse_callable_header(
    header: &SyntaxNode,
    keyword: &str,
    name_required: bool,
) -> DecodeResult<(Option<String>, Vec<Param>, Option<TypeRef>)> {
    let text = semantic_line(header);
    let rest = if text == keyword {
        ""
    } else {
        let rest = text
            .strip_prefix(keyword)
            .ok_or_else(|| error(header, format!("expected a `{keyword}` header")))?;
        if !(rest.starts_with(char::is_whitespace) || rest.starts_with('(')) {
            return Err(error(header, format!("expected a `{keyword}` header")));
        }
        rest.trim_start()
    };
    let (name, rest) = if rest.is_empty() || rest.starts_with('(') || rest.starts_with("->") {
        (None, rest)
    } else {
        let (name, tail) = take_while(rest, is_name_char);
        (Some(name.to_string()), tail.trim_start())
    };
    if name_required && name.is_none() {
        return Err(error(header, format!("`{keyword}` needs a name")));
    }
    let (params, rest) = if let Some(after_open) = rest.strip_prefix('(') {
        let close = after_open
            .find(')')
            .ok_or_else(|| error(header, "unterminated parameter list"))?;
        let params = parse_params(&after_open[..close], header)?;
        (params, after_open[close + 1..].trim_start())
    } else {
        (Vec::new(), rest)
    };
    let returns = if let Some(after_arrow) = rest.strip_prefix("->") {
        let ty = after_arrow.trim();
        if ty.is_empty() {
            return Err(error(header, "expected a return type after `->`"));
        }
        Some(parse_type(ty))
    } else if rest.is_empty() {
        None
    } else {
        return Err(error(
            header,
            format!("unexpected text in {keyword} header: `{rest}`"),
        ));
    };
    Ok((name, params, returns))
}

fn parse_params(text: &str, at: &SyntaxNode) -> DecodeResult<Vec<Param>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|part| {
            let (name, ty) = part
                .split_once(':')
                .ok_or_else(|| error(at, format!("parameter missing `:`: `{part}`")))?;
            let name = name.trim();
            if name.is_empty() {
                return Err(error(at, "empty parameter name"));
            }
            Ok(Param {
                name: name.into(),
                ty: parse_type(ty),
            })
        })
        .collect()
}

fn parse_type(text: &str) -> TypeRef {
    let text = text.trim();
    match text {
        "Any" => TypeRef::Any,
        "Bool" => TypeRef::Bool,
        "Number" => TypeRef::Number,
        "String" => TypeRef::String,
        _ => text
            .strip_prefix("List<")
            .and_then(|inner| inner.strip_suffix('>'))
            .map(|inner| TypeRef::List(Box::new(parse_type(inner))))
            .unwrap_or_else(|| TypeRef::Named(text.to_string())),
    }
}

fn parse_decl_name(rest: &str, kind: &str, at: &SyntaxNode) -> DecodeResult<String> {
    let name = rest.trim();
    let (token, tail) = take_while(name, is_name_char);
    if token.is_empty() || !tail.trim().is_empty() {
        return Err(error(
            at,
            format!("`{kind}` name must be a single identifier, got: `{name}`"),
        ));
    }
    Ok(token.to_string())
}

fn lower_effect(node: &SyntaxNode) -> DecodeResult<FlowEffect> {
    let text = semantic_line(node);
    let tag = text
        .strip_prefix("@effect(")
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or_else(|| error(node, "malformed `@effect(...)` annotation"))?;
    FlowEffect::from_tag(tag).ok_or_else(|| error(node, format!("unknown effect: `{tag}`")))
}

fn attach_effect(node: Node, effect: FlowEffect, at: &SyntaxNode) -> DecodeResult<Node> {
    match node {
        Node::Bind {
            name, value, ty, ..
        } => Ok(Node::Bind {
            name,
            value,
            ty,
            effect: Some(effect),
        }),
        Node::Memo {
            name, value, ty, ..
        } => Ok(Node::Memo {
            name,
            value,
            ty,
            effect: Some(effect),
        }),
        _ => Err(error(at, "`@effect` can only annotate a bind")),
    }
}

fn is_statement(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        BIND_STMT
            | CALL_STMT
            | WHEN_STMT
            | UNLESS_STMT
            | EACH_STMT
            | REPEAT_STMT
            | MATCH_STMT
            | ROUTE_STMT
            | FALLBACK_STMT
            | PARALLEL_STMT
            | LOOP_STMT
            | TIMEOUT_STMT
            | BUDGET_STMT
            | WITH_TOOLS_STMT
            | RETRY_STMT
            | SEQ_STMT
            | CTX_STMT
            | CTX_APPEND_STMT
            | RETURN_STMT
            | ASSERT_STMT
            | JSON_ESCAPE
            | MEMO_STMT
            | ONCE_STMT
            | CHECKPOINT_STMT
            | AWAIT_STMT
            | CONFIRM_STMT
            | THROTTLE_STMT
            | DEBOUNCE_STMT
            | VERIFY_STMT
            | TRY_STMT
            | RACE_STMT
            | SCOPE_STMT
            | SAGA_STMT
            | PIPE_STMT
    )
}

fn rest_after_keyword(node: &SyntaxNode, keyword: &str) -> DecodeResult<String> {
    let line = semantic_line(node);
    keyword_rest(&line, keyword)
        .map(str::to_string)
        .ok_or_else(|| error(node, format!("expected `{keyword}`")))
}

// ---------------------------------------------------------------------------
// Canonical `name: value` control-header options (L-96)
// ---------------------------------------------------------------------------
//
// A parameterized control header spells its non-primary values the way a call spells named inputs:
// `confirm "Open issue?", risk: medium`. The parser lifts each option — leading comma included —
// into a `HEADER_OPTION` node, so what remains of the header text on either side is *exactly* the
// legacy space-keyword spelling. Every lowering below therefore runs the pre-existing positional
// decode over [`positional_header`] and then overlays the named options, which is what makes both
// spellings lower to the identical `DraftAst`.

/// The canonical `name: value` options of a control header, in source order.
fn header_options(statement: &SyntaxNode) -> Vec<SyntaxNode> {
    statement
        .children()
        .filter(|child| child.kind() == SyntaxKind::HEADER_OPTION)
        .collect()
}

/// The label of one header option (`risk` in `, risk: medium`).
fn option_label(option: &SyntaxNode) -> String {
    option
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::IDENT)
        .map(|token| token.text().to_string())
        .unwrap_or_default()
}

/// The scalar text of one header option's value — everything after its `:`, trimmed.
fn option_value_text(option: &SyntaxNode) -> String {
    let mut text = String::new();
    let mut seen_colon = false;
    for token in option
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        if !seen_colon {
            seen_colon = token.kind() == SyntaxKind::COLON;
            continue;
        }
        text.push_str(token.text());
    }
    text.trim().to_string()
}

/// The header text of `statement` with its option tail lifted out — the positional operands plus
/// any trailing `-> $bind`, i.e. the legacy space-keyword header verbatim.
fn positional_header(statement: &SyntaxNode) -> String {
    let mut text = String::new();
    collect_positional_header(statement, &mut text);
    text.trim().to_string()
}

/// Append `node`'s header tokens to `out`, skipping option and body subtrees. Returns `false` once
/// the header line has ended, so the caller stops walking siblings.
fn collect_positional_header(node: &SyntaxNode, out: &mut String) -> bool {
    for element in node.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Node(child) => match child.kind() {
                SyntaxKind::HEADER_OPTION => continue,
                SyntaxKind::BLOCK => return false,
                _ => {
                    if !collect_positional_header(&child, out) {
                        return false;
                    }
                }
            },
            rowan::NodeOrToken::Token(token) => match token.kind() {
                SyntaxKind::NEWLINE | SyntaxKind::COMMENT => return false,
                SyntaxKind::STRING if token.text().starts_with("\"\"\"") => {
                    let content = token
                        .text()
                        .strip_prefix("\"\"\"")
                        .and_then(|rest| rest.strip_suffix("\"\"\""))
                        .unwrap_or_default()
                        .replace("\r\n", "\n");
                    out.push_str(
                        &serde_json::to_string(&content).unwrap_or_else(|_| "\"\"".to_string()),
                    );
                }
                _ => out.push_str(token.text()),
            },
        }
    }
    true
}

/// The positional header of `statement` with its leading keyword removed.
fn positional_rest(statement: &SyntaxNode, keyword: &str) -> DecodeResult<String> {
    let header = positional_header(statement);
    keyword_rest(&header, keyword)
        .map(str::to_string)
        .ok_or_else(|| error(statement, format!("expected `{keyword}`")))
}

/// Record an option value, refusing a field the positional header already supplied.
fn set_option<T>(slot: &mut Option<T>, value: T, option: &SyntaxNode) -> DecodeResult<()> {
    if slot.is_some() {
        return Err(error(
            option,
            format!("`{}` is given twice in this header", option_label(option)),
        ));
    }
    *slot = Some(value);
    Ok(())
}

/// A bare-word option value (`backoff: exponential`, `risk: medium`).
fn option_word(option: &SyntaxNode) -> DecodeResult<String> {
    let text = option_value_text(option);
    let (word, rest) = take_while(&text, is_name_char);
    if word.is_empty() || !rest.trim().is_empty() {
        return Err(error(
            option,
            format!("`{}` expects a bare word", option_label(option)),
        ));
    }
    Ok(word.to_string())
}

/// A duration option value (`delay: 500ms`, `per: 1m`).
fn option_duration(option: &SyntaxNode) -> DecodeResult<u64> {
    let text = option_value_text(option);
    let (millis, rest) = take_duration(&text, option)?;
    if !rest.trim().is_empty() {
        return Err(error(
            option,
            format!("`{}` expects a duration", option_label(option)),
        ));
    }
    Ok(millis)
}

/// A count option value (`max: 5`).
fn option_count(option: &SyntaxNode) -> DecodeResult<u32> {
    option_value_text(option)
        .trim()
        .parse::<u32>()
        .map_err(|_| {
            error(
                option,
                format!("`{}` expects a count", option_label(option)),
            )
        })
}

/// An expression option value (`until: $done`, `when: $ready`) — the parser built a real
/// expression node for these, so nothing is decoded from text.
fn option_expression(option: &SyntaxNode) -> DecodeResult<Node> {
    required_expression(option, &format!("a `{}` expression", option_label(option)))
}

/// The error for an option a header does not define.
fn unknown_option(option: &SyntaxNode, context: &str) -> LowerError {
    error(
        option,
        format!("`{context}` has no `{}` option", option_label(option)),
    )
}

fn keyword_rest<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    if text == keyword {
        Some("")
    } else {
        text.strip_prefix(keyword).and_then(|rest| {
            rest.starts_with(char::is_whitespace)
                .then(|| rest.trim_start())
        })
    }
}

fn split_word(text: &str) -> (&str, &str) {
    let (word, rest) = take_while(text.trim_start(), is_name_char);
    (word, rest.trim_start())
}

fn parse_optional_arrow(
    tail: &str,
    context: &str,
    at: &SyntaxNode,
) -> DecodeResult<Option<SymbolName>> {
    let tail = tail.trim();
    if tail.is_empty() {
        return Ok(None);
    }
    let symbol_text = tail
        .strip_prefix("->")
        .ok_or_else(|| {
            error(
                at,
                format!("unexpected text in `{context}` header: `{tail}`"),
            )
        })?
        .trim_start();
    let symbol_text = symbol_text.strip_prefix('$').unwrap_or(symbol_text);
    let (name, rest) = take_while(symbol_text, is_ident_char);
    if name.is_empty() || !rest.trim().is_empty() {
        return Err(error(
            at,
            format!("malformed `$name` after `->` in `{context}`"),
        ));
    }
    Ok(Some(name.into()))
}

fn parse_symbol_prefix<'a>(
    text: &'a str,
    at: &SyntaxNode,
    context: &str,
) -> DecodeResult<(SymbolName, &'a str)> {
    let text = text.trim_start();
    let sigiled = text.starts_with('$');
    let text = text.strip_prefix('$').unwrap_or(text);
    let (name, rest) = take_while(text, is_ident_char);
    if name.is_empty()
        || (!sigiled
            && !name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_'))
    {
        return Err(error(at, format!("`{context}` expects a symbol name")));
    }
    Ok((name.into(), rest))
}

fn parse_symbol_exact(text: &str, at: &SyntaxNode, context: &str) -> DecodeResult<SymbolName> {
    let (name, rest) = parse_symbol_prefix(text, at, context)?;
    if !rest.trim().is_empty() {
        return Err(error(
            at,
            format!("unexpected text after `{context}`: `{}`", rest.trim()),
        ));
    }
    Ok(name)
}

fn parse_symbol_list(text: &str, at: &SyntaxNode) -> DecodeResult<Vec<SymbolName>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|part| {
            let part = part.trim();
            let name = part.strip_prefix('$').unwrap_or(part);
            if name.is_empty() || !name.chars().all(is_ident_char) {
                return Err(error(at, format!("invalid symbol: `{part}`")));
            }
            Ok(SymbolName::from(name))
        })
        .collect()
}

fn take_duration<'a>(text: &'a str, at: &SyntaxNode) -> DecodeResult<(u64, &'a str)> {
    let text = text.trim_start();
    let (digits, mut rest) = take_while(text, |ch| ch.is_ascii_digit() || ch == '_');
    let number = digits
        .replace('_', "")
        .parse::<u64>()
        .map_err(|_| error(at, format!("expected a number, got: `{text}`")))?;
    let multiplier = if let Some(tail) = rest.strip_prefix("ms") {
        rest = tail;
        1
    } else if let Some(tail) = rest.strip_prefix('s') {
        rest = tail;
        1_000
    } else if let Some(tail) = rest.strip_prefix('m') {
        rest = tail;
        60_000
    } else {
        1
    };
    let millis = number
        .checked_mul(multiplier)
        .ok_or_else(|| error(at, "duration overflows milliseconds"))?;
    Ok((millis, rest))
}

fn parse_string_token(text: &str, at: &SyntaxNode) -> DecodeResult<String> {
    if let Some(content) = text
        .strip_prefix("\"\"\"")
        .and_then(|rest| rest.strip_suffix("\"\"\""))
    {
        return Ok(content.replace("\r\n", "\n"));
    }
    serde_json::from_str::<String>(text)
        .map_err(|err| error(at, format!("invalid string literal: {err}")))
}

fn parse_string_prefix<'a>(
    text: &'a str,
    at: &SyntaxNode,
    context: &str,
) -> DecodeResult<(String, &'a str)> {
    let text = text.trim_start();
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
    match stream.next() {
        Some(Ok(serde_json::Value::String(value))) => Ok((value, &text[stream.byte_offset()..])),
        Some(Ok(_)) => Err(error(at, format!("`{context}` expects a quoted string"))),
        Some(Err(err)) => Err(error(at, format!("invalid string literal: {err}"))),
        None => Err(error(at, format!("`{context}` expects a quoted string"))),
    }
}

fn parse_string_exact(text: &str, at: &SyntaxNode, context: &str) -> DecodeResult<String> {
    let (value, tail) = parse_string_prefix(text, at, context)?;
    if !tail.trim().is_empty() {
        return Err(error(at, format!("trailing text after `{context}`")));
    }
    Ok(value)
}

fn parse_json_exact(text: &str, at: &SyntaxNode) -> DecodeResult<serde_json::Value> {
    serde_json::from_str(text).map_err(|err| error(at, format!("invalid JSON literal: {err}")))
}

/// L-81: recursion cap for the setting-value grammar. `[[[…` / `{a:{a:{…` nest unboundedly, so a
/// deeply nested settings literal (reachable from any `.flux` module header) would overflow the stack
/// and `SIGABRT`. Threaded through the three mutually-recursive parsers and turned into a `LowerError`.
const MAX_SETTING_DEPTH: usize = 256;

fn parse_setting_exact(text: &str, at: &SyntaxNode) -> DecodeResult<serde_json::Value> {
    let (value, tail) = parse_setting_prefix(text, at, 0)?;
    if !tail.trim().is_empty() {
        return Err(error(
            at,
            format!("trailing text after setting value: `{tail}`"),
        ));
    }
    Ok(value)
}

fn parse_setting_prefix<'a>(
    text: &'a str,
    at: &SyntaxNode,
    depth: usize,
) -> DecodeResult<(serde_json::Value, &'a str)> {
    if depth >= MAX_SETTING_DEPTH {
        return Err(error(at, "setting value nesting too deep"));
    }
    let text = text.trim_start();
    let first = text
        .chars()
        .next()
        .ok_or_else(|| error(at, "expected a setting value"))?;
    match first {
        '"' | '-' | '0'..='9' => {
            let mut stream =
                serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
            match stream.next() {
                Some(Ok(value)) => Ok((value, &text[stream.byte_offset()..])),
                Some(Err(err)) => Err(error(at, format!("invalid setting literal: {err}"))),
                None => Err(error(at, "expected a setting value")),
            }
        }
        '[' => parse_setting_list(text, at, depth),
        '{' => parse_setting_record(text, at, depth),
        ch if ch.is_ascii_alphabetic() || ch == '_' => {
            let (ident, rest) = take_while(text, is_name_char);
            match ident {
                "true" => Ok((serde_json::Value::Bool(true), rest)),
                "false" => Ok((serde_json::Value::Bool(false), rest)),
                "null" => Ok((serde_json::Value::Null, rest)),
                "secret" => {
                    let (name, tail) = parse_string_prefix(rest, at, "secret")?;
                    Ok((crate::program::secret_marker(&name), tail))
                }
                _ => Ok((serde_json::Value::String(ident.to_string()), rest)),
            }
        }
        other => Err(error(
            at,
            format!("unexpected character in setting value: `{other}`"),
        )),
    }
}

fn parse_setting_list<'a>(
    text: &'a str,
    at: &SyntaxNode,
    depth: usize,
) -> DecodeResult<(serde_json::Value, &'a str)> {
    let mut tail = text
        .strip_prefix('[')
        .ok_or_else(|| error(at, "expected `[`"))?
        .trim_start();
    let mut values = Vec::new();
    if let Some(rest) = tail.strip_prefix(']') {
        return Ok((serde_json::Value::Array(values), rest));
    }
    loop {
        let (value, rest) = parse_setting_prefix(tail, at, depth + 1)?;
        values.push(value);
        let rest = rest.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            tail = next.trim_start();
        } else if let Some(next) = rest.strip_prefix(']') {
            return Ok((serde_json::Value::Array(values), next));
        } else {
            return Err(error(
                at,
                format!("expected `,` or `]` in list, got: `{rest}`"),
            ));
        }
    }
}

fn parse_setting_record<'a>(
    text: &'a str,
    at: &SyntaxNode,
    depth: usize,
) -> DecodeResult<(serde_json::Value, &'a str)> {
    let mut tail = text
        .strip_prefix('{')
        .ok_or_else(|| error(at, "expected `{`"))?
        .trim_start();
    let mut values = serde_json::Map::new();
    if let Some(rest) = tail.strip_prefix('}') {
        return Ok((serde_json::Value::Object(values), rest));
    }
    loop {
        let (key, rest) = if tail.starts_with('"') {
            parse_string_prefix(tail, at, "record key")?
        } else {
            let (key, rest) = take_while(tail, |ch| ch.is_ascii_alphanumeric() || ch == '_');
            if key.is_empty() {
                return Err(error(at, "expected a record key"));
            }
            (key.to_string(), rest)
        };
        let rest = rest
            .trim_start()
            .strip_prefix(':')
            .ok_or_else(|| error(at, format!("expected `:` after record key `{key}`")))?;
        let (value, rest) = parse_setting_prefix(rest, at, depth + 1)?;
        values.insert(key, value);
        let rest = rest.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            tail = next.trim_start();
        } else if let Some(next) = rest.strip_prefix('}') {
            return Ok((serde_json::Value::Object(values), next));
        } else {
            return Err(error(
                at,
                format!("expected `,` or `}}` in record, got: `{rest}`"),
            ));
        }
    }
}

fn parse_string_setting(text: &str, at: &SyntaxNode, what: &str) -> DecodeResult<String> {
    match parse_setting_exact(text, at)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(error(at, format!("`{what}` must be a string"))),
    }
}

fn string_list(
    value: &serde_json::Value,
    what: &str,
    at: &SyntaxNode,
) -> DecodeResult<Vec<String>> {
    let serde_json::Value::Array(values) = value else {
        return Err(error(at, format!("`{what}` must be a list of strings")));
    };
    values
        .iter()
        .map(|value| match value {
            serde_json::Value::String(value) => Ok(value.clone()),
            _ => Err(error(at, format!("`{what}` must be a list of strings"))),
        })
        .collect()
}

fn parse_effects(value: &serde_json::Value, at: &SyntaxNode) -> DecodeResult<Vec<Effect>> {
    let mut effects = Vec::new();
    for name in string_list(value, "effects", at)? {
        let effect = match normalize(&name).as_str() {
            "read" => Effect::Read,
            "write" => Effect::Write,
            "network" | "model" => Effect::Network,
            "process" => Effect::Process,
            "browser" => Effect::Browser,
            "filesystem" | "file_system" => Effect::Filesystem,
            "local_system" => Effect::LocalSystem,
            other => return Err(error(at, format!("unknown effect `{other}`"))),
        };
        if !effects.contains(&effect) {
            effects.push(effect);
        }
    }
    Ok(effects)
}

fn parse_limits(value: &serde_json::Value, at: &SyntaxNode) -> DecodeResult<CompositeLimits> {
    let serde_json::Value::Object(values) = value else {
        return Err(error(at, "`limits` must be an object"));
    };
    let mut limits = CompositeLimits::default();
    for (key, value) in values {
        let number = value
            .as_u64()
            .ok_or_else(|| error(at, format!("limit `{key}` must be a positive integer")))?;
        match key.as_str() {
            "dispatches" => limits.dispatches = Some(number),
            "timeout_ms" => limits.timeout_ms = Some(number),
            "context_chars" => limits.context_chars = Some(number),
            "max_depth" => limits.max_depth = Some(number),
            other => return Err(error(at, format!("unknown limit `{other}`"))),
        }
    }
    Ok(limits)
}

fn settings_value(values: serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    if values.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(values)
    }
}

fn normalize(text: &str) -> String {
    text.trim().to_ascii_lowercase().replace('-', "_")
}

fn semantic_line(node: &SyntaxNode) -> String {
    let mut text = String::new();
    for token in node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match token.kind() {
            SyntaxKind::NEWLINE | SyntaxKind::COMMENT => break,
            SyntaxKind::STRING if token.text().starts_with("\"\"\"") => {
                let content = token
                    .text()
                    .strip_prefix("\"\"\"")
                    .and_then(|rest| rest.strip_suffix("\"\"\""))
                    .unwrap_or_default()
                    .replace("\r\n", "\n");
                text.push_str(
                    &serde_json::to_string(&content).unwrap_or_else(|_| "\"\"".to_string()),
                );
            }
            _ => text.push_str(token.text()),
        }
    }
    text.trim().to_string()
}

fn compact_text(node: &SyntaxNode) -> String {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            !matches!(
                token.kind(),
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE
            )
        })
        .map(|token| token.text().to_string())
        .collect()
}

/// Compact expression text suitable for JSON decoding. Unlike [`compact_text`], triple-quoted
/// strings are normalized to ordinary JSON strings so their interior newlines remain data rather
/// than layout.
fn compact_semantic_text(node: &SyntaxNode) -> String {
    let mut text = String::new();
    for token in node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match token.kind() {
            SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::NEWLINE => {}
            SyntaxKind::STRING if token.text().starts_with("\"\"\"") => {
                let content = token
                    .text()
                    .strip_prefix("\"\"\"")
                    .and_then(|rest| rest.strip_suffix("\"\"\""))
                    .unwrap_or_default()
                    .replace("\r\n", "\n");
                text.push_str(
                    &serde_json::to_string(&content).unwrap_or_else(|_| "\"\"".to_string()),
                );
            }
            _ => text.push_str(token.text()),
        }
    }
    text
}

fn has_direct_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.kind() == kind)
}

/// The tokens `node` owns *directly* on its header line, trivia dropped and in source order.
///
/// Child nodes are opaque: an operand the parser wrapped in an expression node contributes nothing.
/// That is the difference from [`semantic_line`], and the whole point — a keyword or operator found
/// here was written by the header itself, not by a string inside one of its operands.
fn direct_header_tokens(node: &SyntaxNode) -> Vec<SyntaxToken> {
    let mut tokens = Vec::new();
    for token in node
        .children_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match token.kind() {
            SyntaxKind::NEWLINE | SyntaxKind::COMMENT => break,
            SyntaxKind::WHITESPACE => {}
            _ => tokens.push(token),
        }
    }
    tokens
}

/// One declared symbol read from a single header token (`x`, `$x`) rather than from a text scan.
fn symbol_token(
    token: Option<&SyntaxToken>,
    at: &SyntaxNode,
    context: &str,
) -> DecodeResult<SymbolName> {
    let token = token.ok_or_else(|| error(at, format!("`{context}` expects a symbol name")))?;
    parse_symbol_exact(token.text(), at, context)
}

fn header_tokens(node: &SyntaxNode) -> Vec<(SyntaxKind, String)> {
    let mut tokens = Vec::new();
    for token in node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match token.kind() {
            SyntaxKind::NEWLINE | SyntaxKind::COMMENT => break,
            SyntaxKind::WHITESPACE => {}
            kind => tokens.push((kind, token.text().to_string())),
        }
    }
    tokens
}

fn symbol(text: &str) -> SymbolName {
    text.strip_prefix('$').unwrap_or(text).into()
}

fn take_while(text: &str, predicate: impl Fn(char) -> bool) -> (&str, &str) {
    let end = text
        .char_indices()
        .find_map(|(index, ch)| (!predicate(ch)).then_some(index))
        .unwrap_or(text.len());
    (&text[..end], &text[end..])
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn error(at: &SyntaxNode, message: impl Into<String>) -> LowerError {
    LowerError {
        message: message.into(),
        range: Some(at.text_range()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::FluxLang;
    use rowan::{GreenNodeBuilder, Language};

    /// `depth` nested `when` blocks around a `return`, built straight onto a green-node builder so
    /// the tree is deeper than any the parser's own guard would hand over.
    fn nested_when_blocks(depth: usize) -> SyntaxNode {
        let mut builder = GreenNodeBuilder::new();
        let raw = FluxLang::kind_to_raw;
        for _ in 0..depth {
            builder.start_node(raw(SyntaxKind::BLOCK));
            builder.start_node(raw(SyntaxKind::WHEN_STMT));
            builder.start_node(raw(SyntaxKind::VAR_EXPR));
            builder.token(raw(SyntaxKind::VAR), "$x");
            builder.finish_node();
        }
        builder.start_node(raw(SyntaxKind::BLOCK));
        builder.start_node(raw(SyntaxKind::RETURN_STMT));
        builder.finish_node();
        builder.finish_node();
        for _ in 0..depth {
            builder.finish_node(); // WHEN_STMT
            builder.finish_node(); // BLOCK
        }
        SyntaxNode::new_root(builder.finish())
    }

    /// L-114 (failing-first): the block walk must bottom out in a bounded [`LowerError`] rather than
    /// overflowing the stack. Before the guard this `SIGABRT`ed — the parser's L-81 cap sat one stage
    /// upstream and `Parse`'s public fields let a tree in that never went through it.
    ///
    /// The fixture is sized from [`MAX_LOWER_DEPTH`] rather than pinned to a literal, for two
    /// reasons. The guard short-circuits *at* the ceiling, so every depth past it walks exactly the
    /// same frames — a deeper tree proves nothing more. And depth costs stack twice over: **rowan's
    /// own green-node `Drop` is recursive**, and measured against a 2 MiB stack it aborts somewhere
    /// between 3,000 and 6,000 levels, so the original 3,000-level fixture was carrying a second
    /// cliff at only ~2x margin — one that would take the process down in this test's cleanup rather
    /// than in the code under test. Deriving the depth keeps it just past the ceiling and far from
    /// both cliffs; if the ceiling is ever raised above it, `expect_err` fails loudly rather than
    /// silently testing a tree the guard now admits.
    #[test]
    fn deeply_nested_blocks_are_bounded_not_aborting() {
        let depth = MAX_LOWER_DEPTH * 2;
        let deep = nested_when_blocks(depth);
        let error = lower_block(&deep).expect_err("a block walk past the ceiling must not recurse");
        assert_eq!(error.message, "statement nesting too deep");

        // The decrement-on-drop did not leak: a shallow block still lowers on this thread.
        let shallow = nested_when_blocks(2);
        assert_eq!(
            lower_block(&shallow)
                .expect("shallow nesting still lowers")
                .len(),
            1,
            "the depth guard must not leak across lowerings"
        );
    }

    /// The stack budget the guard is only useful *within* — the invariant the test above cannot see.
    ///
    /// A depth guard that returns a bounded error only on a generously-sized stack has not bounded
    /// anything; it has moved the abort to whichever thread has less. This is the regression that
    /// actually shipped: at `MAX_LOWER_DEPTH = 256` the descent needed ~1837 KiB against the 2 MiB
    /// platform default, so it passed on one machine and `SIGABRT`ed on CI's. Pinning the ceiling
    /// alone would not have caught it — the cost per level is a property of the lowering frames, not
    /// of the constant, so a refactor that fattens `lower_statement` regresses this with the ceiling
    /// untouched.
    ///
    /// So assert the budget directly: reaching the ceiling must fit a stack *well under* the 2 MiB a
    /// libtest thread and a tokio worker both get by default. A failure here aborts the process
    /// rather than failing the assertion — that abort **is** the signal, and it is deterministic
    /// instead of depending on how the day's codegen laid out the frames.
    #[test]
    fn reaching_the_ceiling_fits_a_thread_stack_smaller_than_the_default() {
        const BUDGET: usize = 1536 * 1024; // 75% of the 2 MiB default, measured cost ~1157 KiB

        let refused = std::thread::Builder::new()
            .stack_size(BUDGET)
            .spawn(|| {
                // A fresh thread starts at depth 0: `LOWER_DEPTH` is thread-local.
                let deep = nested_when_blocks(MAX_LOWER_DEPTH * 2);
                lower_block(&deep).map(|body| body.len())
            })
            .expect("spawn the stack-bounded probe")
            .join()
            .expect("the guarded descent must return, not overflow a 1.5 MiB stack");

        assert_eq!(
            refused
                .expect_err("the ceiling must refuse this tree")
                .message,
            "statement nesting too deep",
            "the guard must bottom out in its bounded error inside the budget"
        );
    }
}

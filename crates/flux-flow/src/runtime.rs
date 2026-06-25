//! The interpreter: execute a compiled Flux-Lang flow through the safety envelope. `execute_call`
//! dispatches one op (store the result as an immutable value, optionally bind a symbol, trace it);
//! `execute_flow` walks a whole graph — `bind` / `call` / `return` plus `when` (typed branch) and
//! `repeat` (bounded loop) — resolving each `$symbol` argument to the value the runtime owns
//! (`await` cross-turn suspend/resume is the next slice). `plan_risk` previews a plan's risk and
//! `PlanApprover` enforces whole-plan approval (destructive ops still escalate per op).
//!
//! This is the *only* caller of [`Executor::dispatch`](flux_runtime::Executor) in flux-flow — every
//! op runs through the same gate as any other tool, so there is no new bypass surface.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use flux_agent::AgentSink;
use flux_core::{Error, Result};
use flux_runtime::{ApprovalChoice, Approver, Executor, ToolRegistry, ToolResult};
use flux_spec::{IntentSet, Risk};

use crate::ast::{
    DraftAst, Node, RunEvent, StepId, SymbolName, TypeRef, Value, ValueId, Visibility,
};
use crate::registry::schema_params;
use crate::state::FlowStore;

/// How to bind a single op's result to a session symbol.
pub struct BindSpec<'a> {
    pub name: &'a SymbolName,
    pub ty: Option<&'a str>,
    pub visibility: Visibility,
}

/// The outcome of executing a single operation.
#[derive(Debug, Clone)]
pub struct CallOutcome {
    /// The stored value id, or `None` if the op errored (nothing is bound on error).
    pub value_id: Option<ValueId>,
    pub is_error: bool,
    /// The canonical value: bound to the symbol, spliced into `{{interpolation}}`, used for control
    /// flow (`when`/`return`). Deterministic execution works with this.
    pub content: String,
    /// The model-facing rendering (line-numbered read, diff, …). Equals `content` when the op set no
    /// distinct view. Surfaced to the sink/observation, never bound or interpolated.
    pub view: String,
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// A one-line, length-bounded summary of a value for the symbol table (never the raw bytes).
fn summarize(content: &str) -> String {
    let line = content.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 {
        let head: String = line.chars().take(77).collect();
        format!("{head}...")
    } else {
        line.to_string()
    }
}

/// Execute one registered operation through the envelope, store its result as an immutable value,
/// optionally bind it to a symbol, and append the run-event trace.
pub async fn execute_call(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    op: &str,
    input: serde_json::Value,
    bind: Option<BindSpec<'_>>,
) -> Result<CallOutcome> {
    let input_hash = sha256_hex(&serde_json::to_string(&input).unwrap_or_default());
    let step = StepId(format!("step_{}", &input_hash[..16]));

    store.append_event(
        session_id,
        &RunEvent::StepStarted {
            step: step.clone(),
            op: op.to_string(),
            input_hash,
        },
    )?;

    let result = executor.dispatch(op, input).await;
    // The model-facing view (line numbers, diff, guidance); falls back to canonical content.
    let view = result.view().to_string();

    if result.is_error {
        store.append_event(
            session_id,
            &RunEvent::StepFailed {
                step,
                error: result.content.clone(),
            },
        )?;
        return Ok(CallOutcome {
            value_id: None,
            is_error: true,
            content: result.content,
            view,
        });
    }

    // Bind/store the CANONICAL content (so `{{symbol}}` interpolation stays clean) — never the view.
    let value_id = store.put_value(session_id, &Value::String(result.content.clone()))?;
    store.append_event(
        session_id,
        &RunEvent::StepSucceeded {
            step,
            output: value_id.clone(),
        },
    )?;
    if let Some(b) = bind {
        store.bind(
            session_id,
            b.name,
            &value_id,
            b.ty,
            &summarize(&result.content),
            b.visibility,
        )?;
    }

    Ok(CallOutcome {
        value_id: Some(value_id),
        is_error: false,
        content: result.content,
        view,
    })
}

// ---------------------------------------------------------------------------
// Flow execution (linear v1)
// ---------------------------------------------------------------------------

/// The outcome of executing a whole flow.
#[derive(Debug, Clone)]
pub struct FlowOutcome {
    /// The value id the flow returned (only an explicit `return` sets this).
    pub returned: Option<ValueId>,
    /// The flow's result rendered as text (for display).
    pub result: String,
    /// How many operations were dispatched.
    pub steps: usize,
}

/// Whether body execution should keep going or unwind because a `return` fired.
enum Step {
    /// Keep executing the rest of the body.
    Next,
    /// A `return` executed — unwind the whole flow with this value.
    Return(Option<ValueId>),
}

/// A boxed, borrowed future producing `(last_text, control)` — the recursion-safe shape `exec_body`
/// returns so `when`/`repeat` can recurse into nested bodies.
type BodyFuture<'a> = Pin<Box<dyn Future<Output = Result<(String, Step)>> + Send + 'a>>;

/// Execute a compiled flow's body, dispatching each operation through the same [`execute_call`]
/// envelope. Handles `bind` / `call` / `return` plus `when` (typed branch) and `repeat` (bounded
/// loop). `await` (cross-turn suspend/resume) still returns a clear error — the engine loop covers
/// iteration for now. Every op still goes through [`Executor::dispatch`] — no new bypass surface.
pub async fn execute_flow(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    ast: &DraftAst,
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    let mut steps = 0usize;
    let (last, step) = exec_body(store, executor, session_id, &ast.body, sink, &mut steps).await?;
    let returned = match step {
        Step::Return(vid) => vid,
        Step::Next => None,
    };
    if let Some(vid) = &returned {
        store.append_event(session_id, &RunEvent::FlowReturned { value: vid.clone() })?;
    }
    Ok(FlowOutcome {
        returned,
        result: last,
        steps,
    })
}

/// Execute a sequence of nodes, returning the last produced text and whether a `return` unwound the
/// flow. Boxed because `when`/`repeat` recurse into nested bodies (async recursion needs indirection).
fn exec_body<'a>(
    store: &'a FlowStore,
    executor: &'a Executor,
    session_id: &'a str,
    body: &'a [Node],
    sink: &'a mut dyn AgentSink,
    steps: &'a mut usize,
) -> BodyFuture<'a> {
    Box::pin(async move {
        let mut last = String::new();
        for node in body {
            match node {
                Node::Bind {
                    name, value, ty, ..
                } => {
                    let Node::Call { op, args } = value.as_ref() else {
                        return Err(Error::Other(
                            "execution can only bind the result of a `call`".to_string(),
                        ));
                    };
                    let ty_label = ty.as_ref().map(TypeRef::label);
                    let bind = BindSpec {
                        name,
                        ty: ty_label.as_deref(),
                        visibility: Visibility::Visible,
                    };
                    let outcome =
                        run_call(store, executor, session_id, op, args, Some(bind), sink).await?;
                    *steps += 1;
                    if outcome.is_error {
                        return Err(Error::Other(format!(
                            "step `{op}` failed: {}",
                            outcome.content
                        )));
                    }
                    // The model reasons over intermediate results → feed the model-facing VIEW
                    // (line-numbered read, diff, …). Control flow (`when`/`return`) stays canonical.
                    last = outcome.view;
                }
                Node::Call { op, args } => {
                    let outcome =
                        run_call(store, executor, session_id, op, args, None, sink).await?;
                    *steps += 1;
                    if outcome.is_error {
                        return Err(Error::Other(format!(
                            "step `{op}` failed: {}",
                            outcome.content
                        )));
                    }
                    // The model reasons over intermediate results → feed the model-facing VIEW
                    // (line-numbered read, diff, …). Control flow (`when`/`return`) stays canonical.
                    last = outcome.view;
                }
                Node::Return { value } => {
                    let (content, vid) =
                        eval_return(store, executor, session_id, value, sink, steps).await?;
                    return Ok((content, Step::Return(vid)));
                }
                Node::When {
                    cond,
                    then,
                    otherwise,
                } => {
                    let take = eval_cond(store, executor, session_id, cond, sink, steps).await?;
                    let branch = if take { then } else { otherwise };
                    let (blast, step) =
                        exec_body(store, executor, session_id, branch, &mut *sink, &mut *steps)
                            .await?;
                    if !blast.is_empty() {
                        last = blast;
                    }
                    if let Step::Return(v) = step {
                        return Ok((last, Step::Return(v)));
                    }
                }
                Node::Repeat {
                    max,
                    until,
                    body: rbody,
                } => {
                    for _ in 0..*max {
                        let (blast, step) =
                            exec_body(store, executor, session_id, rbody, &mut *sink, &mut *steps)
                                .await?;
                        if !blast.is_empty() {
                            last = blast;
                        }
                        if let Step::Return(v) = step {
                            return Ok((last, Step::Return(v)));
                        }
                        // `until` is a *stop-when-true* guard, evaluated after each iteration.
                        if let Some(u) = until {
                            if eval_cond(store, executor, session_id, u, &mut *sink, &mut *steps)
                                .await?
                            {
                                break;
                            }
                        }
                    }
                }
                Node::Await { .. } => {
                    return Err(Error::Other(
                        "`await` execution (cross-turn suspend/resume) lands in a later slice"
                            .to_string(),
                    ));
                }
                Node::Var { .. } | Node::Lit { .. } | Node::Thing { .. } => {
                    return Err(Error::Other(
                        "a bare value is not an executable statement".to_string(),
                    ));
                }
            }
        }
        Ok((last, Step::Next))
    })
}

/// Evaluate a `when` / `repeat-until` condition to a boolean. `lit`/`var` are resolved without side
/// effects; a `call` executes (its content's truthiness is the result). An errored call is falsey.
async fn eval_cond(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    node: &Node,
    sink: &mut dyn AgentSink,
    steps: &mut usize,
) -> Result<bool> {
    match node {
        Node::Call { op, args } => {
            let outcome = run_call(store, executor, session_id, op, args, None, sink).await?;
            *steps += 1;
            if outcome.is_error {
                return Ok(false);
            }
            Ok(json_truthy(&serde_json::Value::String(outcome.content)))
        }
        Node::Lit { .. } | Node::Var { .. } => Ok(json_truthy(&eval_arg(node, store, session_id)?)),
        other => Err(Error::Other(format!(
            "unsupported condition `{}`",
            node_kind(other)
        ))),
    }
}

/// JSON truthiness for conditions: null/false/0/empty are falsey; a string is truthy unless it is
/// empty, `"false"`, or `"0"` (so a tool's textual `"false"` output reads as false).
fn json_truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        serde_json::Value::String(s) => {
            let t = s.trim();
            !t.is_empty() && !t.eq_ignore_ascii_case("false") && t != "0"
        }
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

/// Evaluate a call's arguments, map them to the op's named input, surface the call/result to the
/// sink, and dispatch through [`execute_call`].
async fn run_call(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    op: &str,
    args: &[Node],
    bind: Option<BindSpec<'_>>,
    sink: &mut dyn AgentSink,
) -> Result<CallOutcome> {
    let arg_values = args
        .iter()
        .map(|a| eval_arg(a, store, session_id))
        .collect::<Result<Vec<_>>>()?;
    let input = map_args_to_input(op, arg_values, executor.registry())?;
    sink.tool_call(op, &input);
    let outcome = execute_call(store, executor, session_id, op, input, bind).await?;
    // Surface the model-facing VIEW (numbered read, diff, …) to the sink — what the model/user sees.
    // The canonical `outcome.content` remains what control flow and interpolation use.
    sink.tool_result(
        op,
        &ToolResult {
            content: outcome.view.clone(),
            view: None,
            is_error: outcome.is_error,
        },
    );
    Ok(outcome)
}

/// Evaluate a call-argument expression to a concrete JSON value. `Lit` yields its raw JSON, with
/// `{{symbol}}` tokens inside strings substituted by the resolved symbol's text (so a model can embed a
/// stored value into a larger string — e.g. a `task` prompt). `Var` resolves a *standalone* symbol to
/// its stored value as natural JSON. The runtime injects the value it owns — symbols-over-values,
/// executed. Other node kinds are not valid argument positions in linear v1.
fn eval_arg(node: &Node, store: &FlowStore, session_id: &str) -> Result<serde_json::Value> {
    match node {
        Node::Lit { value } => Ok(interpolate(value, store, session_id)),
        Node::Var { name } => {
            let vid = store
                .resolve(session_id, name)?
                .ok_or_else(|| Error::Other(format!("unbound symbol ${}", name.0)))?;
            let value = store
                .get_value(&vid)?
                .ok_or_else(|| Error::Other(format!("dangling value for ${}", name.0)))?;
            Ok(value.to_json())
        }
        other => Err(Error::Other(format!(
            "unsupported call argument `{}` (only literals and $symbols are valid args in v1)",
            node_kind(other)
        ))),
    }
}

/// Substitute `{{symbol}}` tokens inside string literals with the resolved symbol's text value, so the
/// model can embed a stored value into a larger string. Recurses through strings in arrays/objects;
/// non-string scalars pass through. A token whose symbol isn't bound is left verbatim.
fn interpolate(
    value: &serde_json::Value,
    store: &FlowStore,
    session_id: &str,
) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        J::String(s) => J::String(interpolate_str(s, store, session_id)),
        J::Array(a) => J::Array(
            a.iter()
                .map(|v| interpolate(v, store, session_id))
                .collect(),
        ),
        J::Object(o) => J::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), interpolate(v, store, session_id)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Replace each `{{name}}` (or `{name}`) in `s` with the text of the **bound** symbol `name`. Accepting
/// both brace styles is robustness against the model's inconsistent templating; only a bound symbol is
/// substituted, so an unbound token (or any other `{…}` text) is left exactly as written.
fn interpolate_str(s: &str, store: &FlowStore, session_id: &str) -> String {
    if !s.contains('{') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let at_brace = &rest[open..]; // starts with '{'
        let (open_tok, close_tok): (&str, &str) = if at_brace.starts_with("{{") {
            ("{{", "}}")
        } else {
            ("{", "}")
        };
        let inner = &at_brace[open_tok.len()..];
        let Some(rel) = inner.find(close_tok) else {
            // No closing brace — emit the remainder verbatim and stop.
            out.push_str(at_brace);
            return out;
        };
        let name = inner[..rel].trim();
        match resolve_symbol_text(store, session_id, name) {
            Some(text) => {
                out.push_str(&text);
                rest = &inner[rel + close_tok.len()..];
            }
            None => {
                // Not a bound symbol — keep the open brace(s) and re-scan from just after them.
                out.push_str(open_tok);
                rest = inner;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of a bound symbol, or `None` if `name` is empty / unbound / unreadable.
fn resolve_symbol_text(store: &FlowStore, session_id: &str, name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let vid = store
        .resolve(session_id, &SymbolName(name.to_string()))
        .ok()??;
    let value = store.get_value(&vid).ok()??;
    Some(value_text(&value))
}

/// Map a call's positional argument *values* onto the op's named JSON input. A lone object argument
/// is taken as the whole named input; otherwise the values bind to the op's parameters in
/// `required ++ optional` order (from its JSON-Schema). The AST stays positional — this is the one
/// place positional args become the named object a tool expects.
fn map_args_to_input(
    op: &str,
    args: Vec<serde_json::Value>,
    registry: &ToolRegistry,
) -> Result<serde_json::Value> {
    let tool = registry
        .get(op)
        .ok_or_else(|| Error::Other(format!("unknown op `{op}`")))?;
    let schema = tool.spec().input_schema;

    // A lone object argument is already the named input map — pass it straight through.
    if let [serde_json::Value::Object(_)] = args.as_slice() {
        return Ok(args.into_iter().next().unwrap());
    }

    let (required, optional) = schema_params(&schema);
    let order: Vec<String> = required.into_iter().chain(optional).collect();
    let mut input = serde_json::Map::new();
    for (i, val) in args.into_iter().enumerate() {
        match order.get(i) {
            Some(name) => {
                input.insert(name.clone(), val);
            }
            None => {
                return Err(Error::Other(format!(
                    "op `{op}` accepts {} parameter(s) but {} argument(s) were supplied",
                    order.len(),
                    i + 1
                )))
            }
        }
    }
    Ok(serde_json::Value::Object(input))
}

/// Evaluate a `return` expression to `(text, value_id)`: `var` → the symbol's stored value; `call`
/// → execute it and use its output; `lit` → store the literal as the flow's return value.
async fn eval_return(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    value: &Node,
    sink: &mut dyn AgentSink,
    steps: &mut usize,
) -> Result<(String, Option<ValueId>)> {
    match value {
        Node::Var { name } => {
            let vid = store
                .resolve(session_id, name)?
                .ok_or_else(|| Error::Other(format!("return of unbound symbol ${}", name.0)))?;
            let value = store
                .get_value(&vid)?
                .ok_or_else(|| Error::Other(format!("dangling value for ${}", name.0)))?;
            Ok((value_text(&value), Some(vid)))
        }
        Node::Lit { value } => {
            let text = lit_text(value);
            let vid = store.put_value(session_id, &Value::String(text.clone()))?;
            Ok((text, Some(vid)))
        }
        Node::Call { op, args } => {
            let outcome = run_call(store, executor, session_id, op, args, None, sink).await?;
            *steps += 1;
            if outcome.is_error {
                return Err(Error::Other(format!(
                    "return step `{op}` failed: {}",
                    outcome.content
                )));
            }
            Ok((outcome.content, outcome.value_id))
        }
        other => Err(Error::Other(format!(
            "unsupported return expression `{}`",
            node_kind(other)
        ))),
    }
}

/// Render a stored value as text (a string value is itself; anything else is its compact JSON).
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(&other.to_json()).unwrap_or_default(),
    }
}

/// Render a literal JSON value as text (a JSON string is itself; anything else is compact JSON).
fn lit_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The node-kind tag (for error messages).
fn node_kind(node: &Node) -> &'static str {
    match node {
        Node::Call { .. } => "call",
        Node::Bind { .. } => "bind",
        Node::When { .. } => "when",
        Node::Repeat { .. } => "repeat",
        Node::Await { .. } => "await",
        Node::Return { .. } => "return",
        Node::Var { .. } => "var",
        Node::Lit { .. } => "lit",
        Node::Thing { .. } => "thing",
    }
}

// ---------------------------------------------------------------------------
// Plan risk + whole-plan approval
// ---------------------------------------------------------------------------

/// A best-effort risk preview of a compiled plan, aggregated from the ops it calls. Dispatch
/// re-checks every op at execution, so the safety floor never depends on this — it drives the one
/// whole-plan approval prompt.
#[derive(Debug, Clone, Default)]
pub struct PlanRisk {
    /// The highest [`Risk`] across the plan's ops (`None` if it calls nothing registered).
    pub max_risk: Option<Risk>,
    /// True if any op is destructive-shaped — forces a per-op re-confirm even inside an approved plan.
    pub destructive: bool,
    /// True if any op writes / executes / connects out.
    pub mutating: bool,
    /// The distinct op names the plan calls, in first-seen order.
    pub ops: Vec<String>,
}

impl PlanRisk {
    /// A one-line human summary (for the approval prompt).
    pub fn summary(&self) -> String {
        let base = match self.max_risk {
            Some(Risk::Destructive) => "destructive",
            Some(Risk::High) => "high",
            Some(Risk::Medium) => "medium",
            Some(Risk::Low) => "low",
            None => "no-op",
        };
        if self.destructive {
            format!("{base} · contains a destructive operation (will re-confirm)")
        } else if self.mutating {
            format!("{base} · mutating")
        } else {
            base.to_string()
        }
    }
}

/// Compute a plan's [`PlanRisk`] by walking every `call` node and looking up each op's spec (risk)
/// and intents (destructive / mutating) in `registry`. Only literal args are known statically, so
/// they are fed to `Tool::intents` for command/path-shaped detection; `$symbol` args are skipped.
pub fn plan_risk(ast: &DraftAst, registry: &ToolRegistry) -> PlanRisk {
    let mut risk = PlanRisk::default();
    walk_calls(&ast.body, &mut |op, args| {
        if !risk.ops.iter().any(|o| o == op) {
            risk.ops.push(op.to_string());
        }
        let Some(tool) = registry.get(op) else {
            return;
        };
        let spec = tool.spec();
        risk.max_risk = Some(match risk.max_risk {
            Some(r) => r.max(spec.risk),
            None => spec.risk,
        });
        if spec.risk == Risk::Destructive {
            risk.destructive = true;
        }
        let intents = tool.intents(&literal_input(args, &spec.input_schema));
        if intents.is_destructive() {
            risk.destructive = true;
        }
        if intents.is_mutating() {
            risk.mutating = true;
        }
    });
    risk
}

/// Visit every `call` node reachable in `nodes` (recursing through binds, branches, loops, returns,
/// and nested call args), invoking `f(op, args)` for each.
fn walk_calls<'a>(nodes: &'a [Node], f: &mut impl FnMut(&'a str, &'a [Node])) {
    for node in nodes {
        walk_node(node, f);
    }
}

fn walk_node<'a>(node: &'a Node, f: &mut impl FnMut(&'a str, &'a [Node])) {
    match node {
        Node::Call { op, args } => {
            f(op, args);
            walk_calls(args, f);
        }
        Node::Bind { value, .. } => walk_node(value, f),
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            walk_node(cond, f);
            walk_calls(then, f);
            walk_calls(otherwise, f);
        }
        Node::Repeat { until, body, .. } => {
            if let Some(u) = until {
                walk_node(u, f);
            }
            walk_calls(body, f);
        }
        Node::Return { value } => walk_node(value, f),
        Node::Var { .. } | Node::Lit { .. } | Node::Thing { .. } | Node::Await { .. } => {}
    }
}

/// Build a best-effort named input from a call's *literal* args only (for intent preview); non-literal
/// args (`$symbols`) are skipped and arity is not enforced.
fn literal_input(args: &[Node], schema: &serde_json::Value) -> serde_json::Value {
    if let [Node::Lit { value }] = args {
        if value.is_object() {
            return value.clone();
        }
    }
    let (required, optional) = schema_params(schema);
    let order: Vec<String> = required.into_iter().chain(optional).collect();
    let mut input = serde_json::Map::new();
    for (i, arg) in args.iter().enumerate() {
        if let Node::Lit { value } = arg {
            if let Some(name) = order.get(i) {
                input.insert(name.clone(), value.clone());
            }
        }
    }
    serde_json::Value::Object(input)
}

/// An [`Approver`] for a pre-approved plan: a non-destructive op whose name is in the approved set is
/// allowed without prompting; a **destructive** op (or any op not in the set) falls through to
/// `fallback`, so destructive operations still escalate to a per-op confirmation even inside an
/// approved plan — the safety invariant. Installed on the execution executor after the user approves
/// the rendered plan.
pub struct PlanApprover {
    approved: HashSet<String>,
    fallback: Arc<dyn Approver>,
}

impl PlanApprover {
    /// Approve the given op names as a unit; everything else (and any destructive op) defers to
    /// `fallback`.
    pub fn new(approved: impl IntoIterator<Item = String>, fallback: Arc<dyn Approver>) -> Self {
        Self {
            approved: approved.into_iter().collect(),
            fallback,
        }
    }
}

#[async_trait]
impl Approver for PlanApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        intents: &IntentSet,
    ) -> ApprovalChoice {
        if !intents.is_destructive() && self.approved.contains(tool) {
            ApprovalChoice::Allow
        } else {
            self.fallback.request(tool, subjects, intents).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

    /// A tool that echoes its `text` param back as content.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "echo",
                "echo text",
                json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            )
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            params: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// A tool whose canonical content ("RAW") differs from its model-facing view ("VIEW").
    struct TwoFaceTool;

    #[async_trait]
    impl Tool for TwoFaceTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "twoface",
                "two-face",
                json!({ "type": "object", "properties": {} }),
            )
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult::ok_view("RAW", "VIEW"))
        }
    }

    fn temp_executor(allow: bool) -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-{}-{}",
            std::process::id(),
            if allow { "allow" } else { "deny" }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let perms = if allow {
            PermissionManager::from_rules(&["echo".into()], &[])
        } else {
            PermissionManager::from_rules(&[], &["echo".into()])
        };
        Executor::new(
            reg,
            perms,
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
    }

    #[tokio::test]
    async fn single_op_stores_value_binds_symbol_and_traces() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let draft = SymbolName("draft".into());

        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "echo",
            json!({"text": "renewal follow-up"}),
            Some(BindSpec {
                name: &draft,
                ty: Some("Draft"),
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert!(!outcome.is_error);
        let vid = outcome.value_id.clone().unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("renewal follow-up".into()))
        );
        assert_eq!(store.resolve("sess", &draft).unwrap(), Some(vid));

        // the view projects a summary, not the raw value bytes
        let view = store.view("sess").unwrap();
        assert_eq!(view.symbols.len(), 1);
        assert_eq!(view.symbols[0].name, draft);
        assert_eq!(view.symbols[0].summary, "renewal follow-up");

        let events = store.events("sess").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepStarted { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepSucceeded { .. })));
    }

    #[tokio::test]
    async fn two_face_result_binds_canonical_shows_view() {
        // The two-face invariant: the bound symbol value (and `{{interpolation}}` source) is the
        // CANONICAL content, while the model/sink-facing outcome carries the distinct VIEW.
        let store = FlowStore::in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("flux-flow-twoface-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TwoFaceTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["twoface".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let sym = SymbolName("x".into());
        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "twoface",
            json!({}),
            Some(BindSpec {
                name: &sym,
                ty: None,
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert_eq!(outcome.content, "RAW", "canonical content");
        assert_eq!(outcome.view, "VIEW", "model-facing view");
        // The STORED/interpolated value is the canonical content, never the view.
        let vid = outcome.value_id.clone().unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("RAW".into()))
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn denied_op_is_traced_as_failed_and_not_bound() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(false);
        let draft = SymbolName("draft".into());

        let outcome = execute_call(
            &store,
            &ex,
            "sess",
            "echo",
            json!({"text": "x"}),
            Some(BindSpec {
                name: &draft,
                ty: Some("Draft"),
                visibility: Visibility::Visible,
            }),
        )
        .await
        .unwrap();

        assert!(outcome.is_error, "a denied op yields an error outcome");
        assert!(outcome.value_id.is_none());
        assert_eq!(store.resolve("sess", &draft).unwrap(), None);
        let events = store.events("sess").unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::StepFailed { .. })));
    }

    // ---- flow execution + risk (linear v1) ----

    /// A sink that records the op names it was told about.
    #[derive(Default)]
    struct CollectSink {
        calls: Vec<String>,
    }
    impl AgentSink for CollectSink {
        fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
            self.calls.push(name.to_string());
        }
    }

    fn flow_bind(name: &str, op: &str, args: Vec<Node>) -> Node {
        Node::Bind {
            name: SymbolName(name.into()),
            value: Box::new(Node::Call {
                op: op.into(),
                args,
            }),
            ty: None,
            effect: None,
        }
    }
    fn flow_lit(v: serde_json::Value) -> Node {
        Node::Lit { value: v }
    }
    fn flow_var(name: &str) -> Node {
        Node::Var {
            name: SymbolName(name.into()),
        }
    }

    #[test]
    fn map_args_maps_positional_to_required_param_names() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);

        // read("README.md") → {"path": "README.md"}
        let input = map_args_to_input("read", vec![json!("README.md")], &reg).unwrap();
        assert_eq!(input, json!({ "path": "README.md" }));

        // write("out.txt", "hi") → required order {path, content}
        let input = map_args_to_input("write", vec![json!("out.txt"), json!("hi")], &reg).unwrap();
        assert_eq!(input, json!({ "path": "out.txt", "content": "hi" }));

        // edit(path, old, new) → all three required params, in order
        let input =
            map_args_to_input("edit", vec![json!("f"), json!("a"), json!("b")], &reg).unwrap();
        assert_eq!(
            input,
            json!({ "path": "f", "old_string": "a", "new_string": "b" })
        );

        // A lone object argument passes straight through as the named input.
        let input =
            map_args_to_input("write", vec![json!({ "path": "x", "content": "y" })], &reg).unwrap();
        assert_eq!(input, json!({ "path": "x", "content": "y" }));

        // More args than the op has params (read takes 3: path, offset, limit) is a clear error,
        // not a silent drop.
        assert!(map_args_to_input(
            "read",
            vec![json!("a"), json!("b"), json!("c"), json!("d")],
            &reg
        )
        .is_err());
    }

    #[test]
    fn eval_arg_resolves_a_var_to_its_stored_value() {
        let store = FlowStore::in_memory().unwrap();
        let vid = store
            .put_value("sess", &Value::String("hello".into()))
            .unwrap();
        store
            .bind(
                "sess",
                &SymbolName("greeting".into()),
                &vid,
                None,
                "hello",
                Visibility::Visible,
            )
            .unwrap();

        // A $symbol resolves to the natural JSON of its stored value.
        assert_eq!(
            eval_arg(&flow_var("greeting"), &store, "sess").unwrap(),
            json!("hello")
        );
        // A literal is returned verbatim; an unbound symbol is a clear error.
        assert_eq!(
            eval_arg(&flow_lit(json!(42)), &store, "sess").unwrap(),
            json!(42)
        );
        assert!(eval_arg(&flow_var("missing"), &store, "sess").is_err());
    }

    #[test]
    fn eval_arg_interpolates_curly_symbols_in_strings() {
        let store = FlowStore::in_memory().unwrap();
        let vid = store
            .put_value("sess", &Value::String("the lines".into()))
            .unwrap();
        store
            .bind(
                "sess",
                &SymbolName("hits".into()),
                &vid,
                None,
                "the lines",
                Visibility::Visible,
            )
            .unwrap();

        // Both {{symbol}} and {symbol} inside a string lit resolve to the bound symbol's text.
        assert_eq!(
            eval_arg(&flow_lit(json!("reverse: {{hits}}")), &store, "sess").unwrap(),
            json!("reverse: the lines")
        );
        assert_eq!(
            eval_arg(&flow_lit(json!("reverse: {hits}")), &store, "sess").unwrap(),
            json!("reverse: the lines")
        );
        // An unbound token (either style, or unrelated `{…}` text) is left verbatim (no silent loss).
        assert_eq!(
            eval_arg(&flow_lit(json!("x {{nope}} {also} y")), &store, "sess").unwrap(),
            json!("x {{nope}} {also} y")
        );
        // Interpolation recurses into string fields of an object lit.
        assert_eq!(
            eval_arg(&flow_lit(json!({ "task": "do {{hits}}" })), &store, "sess").unwrap(),
            json!({ "task": "do the lines" })
        );
    }

    #[tokio::test]
    async fn execute_flow_runs_a_linear_plan_through_dispatch() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // $a = echo("hi"); $b = echo($a); return $b
        let ast = DraftAst {
            body: vec![
                flow_bind("a", "echo", vec![flow_lit(json!("hi"))]),
                flow_bind("b", "echo", vec![flow_var("a")]),
                Node::Return {
                    value: Box::new(flow_var("b")),
                },
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();

        assert_eq!(outcome.steps, 2, "both echo ops dispatched");
        assert_eq!(outcome.result, "hi");
        assert_eq!(sink.calls, vec!["echo", "echo"]);
        // $b holds the value $a flowed into it (symbols carried the value, not the prose).
        let vid = store
            .resolve("sess", &SymbolName("b".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("hi".into()))
        );
        // The trace records the return.
        assert!(store
            .events("sess")
            .unwrap()
            .iter()
            .any(|e| matches!(e, RunEvent::FlowReturned { .. })));
    }

    #[tokio::test]
    async fn execute_flow_when_takes_the_true_branch() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // when true { $taken = echo("then") } else { $taken = echo("else") }
        let ast = DraftAst {
            body: vec![Node::When {
                cond: Box::new(flow_lit(json!(true))),
                then: vec![flow_bind("taken", "echo", vec![flow_lit(json!("then"))])],
                otherwise: vec![flow_bind("taken", "echo", vec![flow_lit(json!("else"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 1, "only the taken branch's op runs");
        let vid = store
            .resolve("sess", &SymbolName("taken".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("then".into()))
        );
    }

    #[tokio::test]
    async fn execute_flow_when_takes_the_false_branch() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::When {
                cond: Box::new(flow_lit(json!(false))),
                then: vec![flow_bind("taken", "echo", vec![flow_lit(json!("then"))])],
                otherwise: vec![flow_bind("taken", "echo", vec![flow_lit(json!("else"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        let vid = store
            .resolve("sess", &SymbolName("taken".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("else".into()))
        );
    }

    #[tokio::test]
    async fn execute_flow_repeat_caps_at_max_and_until_breaks_early() {
        // repeat max 3 { echo } → runs 3 times (no `until`).
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 3,
                until: None,
                body: vec![flow_bind("x", "echo", vec![flow_lit(json!("hi"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 3, "repeat runs exactly max times");

        // repeat max 3 until true { echo } → the always-true guard stops it after one iteration.
        let store = FlowStore::in_memory().unwrap();
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 3,
                until: Some(Box::new(flow_lit(json!(true)))),
                body: vec![flow_bind("x", "echo", vec![flow_lit(json!("hi"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            outcome.steps, 1,
            "`until` true after iteration 1 breaks the loop"
        );
    }

    #[tokio::test]
    async fn execute_flow_still_errors_on_await() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Await {
                binding: None,
                source: "input".into(),
                as_type: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("await"));
    }

    #[test]
    fn plan_risk_flags_destructive_and_mutating() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);

        // bash "rm -rf build" (destructive) + write (mutating).
        let ast = DraftAst {
            body: vec![
                flow_bind("x", "bash", vec![flow_lit(json!("rm -rf build"))]),
                flow_bind(
                    "y",
                    "write",
                    vec![flow_lit(json!("out.txt")), flow_lit(json!("data"))],
                ),
            ],
            ..Default::default()
        };
        let risk = plan_risk(&ast, &reg);
        assert!(risk.destructive, "rm -rf is destructive-shaped");
        assert!(risk.mutating);
        assert_eq!(risk.max_risk, Some(Risk::High)); // bash is High, write Medium
        assert_eq!(risk.ops, vec!["bash".to_string(), "write".to_string()]);

        // A read-only plan is neither destructive nor mutating.
        let safe = DraftAst {
            body: vec![flow_bind("r", "read", vec![flow_lit(json!("README.md"))])],
            ..Default::default()
        };
        let risk = plan_risk(&safe, &reg);
        assert!(!risk.destructive);
        assert!(!risk.mutating);
        assert_eq!(risk.max_risk, Some(Risk::Low));
    }

    #[tokio::test]
    async fn plan_approver_allows_approved_nondestructive_and_escalates_destructive() {
        use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};
        use std::sync::atomic::{AtomicBool, Ordering};

        /// A fallback approver that records being consulted, then denies.
        struct Recording {
            hit: AtomicBool,
        }
        #[async_trait]
        impl Approver for Recording {
            async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
                self.hit.store(true, Ordering::Relaxed);
                ApprovalChoice::Deny
            }
        }

        let fallback = Arc::new(Recording {
            hit: AtomicBool::new(false),
        });

        // An approved, non-destructive op is allowed without consulting the fallback.
        let approver = PlanApprover::new(["write".to_string()], fallback.clone());
        let empty = IntentSet::new();
        assert!(matches!(
            approver.request("write", &[], &empty).await,
            ApprovalChoice::Allow
        ));
        assert!(
            !fallback.hit.load(Ordering::Relaxed),
            "approved op must not prompt"
        );

        // A destructive op falls through to the fallback even though it is in the approved set.
        let mut destructive = IntentSet::new();
        destructive.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "rm -rf /".into(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        let approver = PlanApprover::new(["bash".to_string()], fallback.clone());
        assert!(matches!(
            approver.request("bash", &[], &destructive).await,
            ApprovalChoice::Deny
        ));
        assert!(
            fallback.hit.load(Ordering::Relaxed),
            "a destructive op must still escalate to the fallback"
        );
    }
}

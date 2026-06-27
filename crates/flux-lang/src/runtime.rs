//! The reference interpreter: execute a compiled Flux-Lang flow against an injected execution host.
//! `execute_call` dispatches one op (store the result as an immutable value, optionally bind a symbol,
//! trace it); `execute_flow` walks a whole graph — `bind` / `call` / `return` plus `when` (typed
//! branch) and `repeat` (bounded loop) — resolving each `$symbol` argument to the value the store
//! owns (`await` cross-turn suspend/resume is the next slice).
//!
//! All effects are injected as L0 traits: operations dispatch through [`OpHost`], values live in a
//! [`ValueStore`], and observations stream to a [`FlowSink`]. The language has no dependency on any
//! concrete runtime, provider, or tool — the engine adapts its safety envelope onto these traits, so
//! every op still runs through the same gate as any other tool (no new bypass surface).

use std::future::Future;
use std::pin::Pin;

use sha2::{Digest, Sha256};

use flux_core::{Error, Usage};
use flux_spec::IntentSet;

use crate::ast::{
    DraftAst, Node, PhysicalPlan, RunEvent, Stage, StepId, SymbolName, TypeRef, Value, ValueId,
    Visibility,
};
use crate::host::{ApprovalChoice, OpHost, OpOutcome};
use crate::opspec::OpCatalog;
use crate::sink::FlowSink;
use crate::store::ValueStore;
use crate::{FlowError, Result};

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

/// Bind an already-stored value id to a session symbol, deriving the one-line summary from its text.
/// Used by `seq`/`each`/`pipe`/`parallel` to bind a block's result (the value already exists in the
/// store; only the symbol mapping is new).
fn bind_existing(
    store: &dyn ValueStore,
    session_id: &str,
    name: &SymbolName,
    vid: &ValueId,
) -> Result<()> {
    let summary = store
        .get_value(vid)?
        .map(|v| summarize(&value_text(&v)))
        .unwrap_or_default();
    store
        .bind(session_id, name, vid, None, &summary, Visibility::Visible)
        .map_err(FlowError::Core)
}

/// Execute one registered operation through the envelope, store its result as an immutable value,
/// optionally bind it to a symbol, and append the run-event trace.
pub async fn execute_call(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
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
    /// The flow's result rendered as text (for display) — the *last* node's view. This is what a
    /// one-shot CLI prints and what an explicit `return` carries.
    pub result: String,
    /// The model-facing transcript: every read/call node's view, labeled and concatenated. The engine
    /// feeds THIS back between rounds so the model sees *all* of a plan's reads — not just the last —
    /// which is what lets "read N files, then answer" converge instead of re-reading every round.
    pub transcript: String,
    /// How many operations were dispatched.
    pub steps: usize,
    /// Set when the flow **suspended** on a top-level `await` instead of completing. The engine
    /// persists this (with the flow body) and calls [`resume_flow`] once a value for the awaited
    /// `source` arrives; `None` on a normal completion or `return`.
    pub suspension: Option<Suspension>,
}

/// A suspended flow's resume point: the top-level `await` node it stopped at and the external input it
/// waits for. The engine persists this alongside the flow body and, when a value for `source` arrives,
/// calls [`resume_flow`] with the same body + this node so execution continues from the *next*
/// statement — the already-run prefix (and its side effects) is never re-executed.
#[derive(Debug, Clone, PartialEq)]
pub struct Suspension {
    /// The top-level index of the `await` that suspended the flow.
    pub node: crate::ast::NodeId,
    /// The external input the flow is waiting for (the `await`'s `source`).
    pub source: String,
}

/// Whether body execution should keep going or unwind because a `return` fired.
enum Step {
    /// Keep executing the rest of the body.
    Next,
    /// A `return` executed — unwind the whole flow with this value.
    Return(Option<ValueId>),
}

/// A boxed, borrowed future producing `(last_text, last_value, control)` — the recursion-safe shape
/// `exec_body` returns so `when`/`repeat`/`each`/`seq`/`parallel` can recurse into nested bodies. The
/// `last_value` is the value id the body's final op produced (so `seq`/`each`/`parallel` can bind it).
type BodyFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(String, Option<ValueId>, Step)>> + Send + 'a>>;

/// A recorded [`AgentSink`] call, buffered so a `parallel` branch's output can be replayed into the
/// real sink after the concurrent join (rather than interleaving on a shared `&mut` sink).
enum SinkEvent {
    Text(String),
    Thinking(String),
    Planning(bool),
    ToolCall(String, serde_json::Value),
    ToolResult(String, OpOutcome),
    Observation(flux_evidence::Observation),
    TurnEnd(Option<Usage>),
}

/// A buffering sink for one `parallel` branch: it records the sink calls the branch makes, then
/// [`replay`](BufferSink::replay)s them — in order — into the real sink once all branches have joined.
#[derive(Default)]
struct BufferSink {
    events: Vec<SinkEvent>,
}

impl BufferSink {
    /// Drain the recorded events into `sink`, preserving their order within the branch.
    fn replay(self, sink: &mut dyn FlowSink) {
        for ev in self.events {
            match ev {
                SinkEvent::Text(t) => sink.text_delta(&t),
                SinkEvent::Thinking(t) => sink.thinking_delta(&t),
                SinkEvent::Planning(a) => sink.planning(a),
                SinkEvent::ToolCall(n, i) => sink.tool_call(&n, &i),
                SinkEvent::ToolResult(n, r) => sink.tool_result(&n, &r),
                SinkEvent::Observation(o) => sink.observation(&o),
                SinkEvent::TurnEnd(u) => sink.turn_end(u),
            }
        }
    }
}

impl FlowSink for BufferSink {
    fn text_delta(&mut self, text: &str) {
        self.events.push(SinkEvent::Text(text.to_string()));
    }
    fn thinking_delta(&mut self, text: &str) {
        self.events.push(SinkEvent::Thinking(text.to_string()));
    }
    fn planning(&mut self, active: bool) {
        self.events.push(SinkEvent::Planning(active));
    }
    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.events
            .push(SinkEvent::ToolCall(name.to_string(), input.clone()));
    }
    fn tool_result(&mut self, name: &str, result: &OpOutcome) {
        self.events
            .push(SinkEvent::ToolResult(name.to_string(), result.clone()));
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.events.push(SinkEvent::Observation(o.clone()));
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.events.push(SinkEvent::TurnEnd(usage));
    }
}

/// Execute a compiled flow's body, dispatching each operation through the same [`execute_call`]
/// envelope. Handles `bind` / `call` / `return` plus the control-flow nodes. A top-level `await`
/// **suspends** the flow (returning [`FlowOutcome::suspension`]); the engine persists it and calls
/// [`resume_flow`] when the awaited input arrives. Every op still goes through
/// [`Executor::dispatch`] — no new bypass surface.
pub async fn execute_flow(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    ast: &DraftAst,
    sink: &mut dyn FlowSink,
) -> Result<FlowOutcome> {
    run_top_level(store, executor, session_id, &ast.body, 0, None, sink).await
}

/// Resume a flow suspended on a top-level `await` (see [`FlowOutcome::suspension`]). Binds `input` to
/// the `await` at index `at` (the suspended node) and continues from the *next* top-level statement —
/// the already-executed prefix and its side effects are not re-run, because the earlier symbols are
/// already durable in the store. The flow may suspend again on a later `await`.
pub async fn resume_flow(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    body: &[Node],
    at: crate::ast::NodeId,
    input: Value,
    sink: &mut dyn FlowSink,
) -> Result<FlowOutcome> {
    run_top_level(
        store,
        executor,
        session_id,
        body,
        at.0 as usize,
        Some(input),
        sink,
    )
    .await
}

/// The top-level statement driver shared by [`execute_flow`] (fresh, `start = 0`) and [`resume_flow`]
/// (`start` = the suspended `await`'s index, with `resume` = the value to bind there). It walks the
/// flow's top-level statements: a top-level `await` **suspends** the flow (records `Awaiting`, returns
/// `FlowOutcome.suspension`); every other node runs through [`exec_body`] exactly as before. Because
/// `await` is a top-level-only statement (the analyzer enforces it), a suspend can only originate here,
/// at a known index — nested bodies never produce one.
async fn run_top_level(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    body: &[Node],
    start: usize,
    resume: Option<Value>,
    sink: &mut dyn FlowSink,
) -> Result<FlowOutcome> {
    let mut steps = 0usize;
    let mut transcript: Vec<String> = Vec::new();
    let mut last = String::new();
    let mut i = start;

    // Resuming: bind the awaited value to the suspended `await`'s binding, then advance past it.
    if let Some(value) = resume {
        let Some(Node::Await {
            binding, as_type, ..
        }) = body.get(start)
        else {
            return Err(FlowError::Runtime(
                "resume cursor does not point at an `await` node".to_string(),
            ));
        };
        let coerced = coerce_await_input(value, as_type);
        let vid = store.put_value(session_id, &coerced)?;
        let text = value_text(&coerced);
        if let Some(b) = binding {
            let ty_label = as_type.as_ref().map(TypeRef::label);
            store.bind(
                session_id,
                b,
                &vid,
                ty_label.as_deref(),
                &summarize(&text),
                Visibility::Visible,
            )?;
        }
        last = text;
        i = start + 1;
    }

    while i < body.len() {
        if let Node::Await { source, .. } = &body[i] {
            let node = crate::ast::NodeId(i as u32);
            store.append_event(
                session_id,
                &RunEvent::Awaiting {
                    run: crate::ast::RunId(format!("{session_id}:{i}")),
                    node,
                },
            )?;
            return Ok(FlowOutcome {
                returned: None,
                result: last,
                transcript: transcript.join("\n\n"),
                steps,
                suspension: Some(Suspension {
                    node,
                    source: source.clone(),
                }),
            });
        }
        let (blast, _bvid, step) = exec_body(
            store,
            executor,
            session_id,
            std::slice::from_ref(&body[i]),
            sink,
            &mut steps,
            &mut transcript,
        )
        .await?;
        if !blast.is_empty() {
            last = blast;
        }
        if let Step::Return(vid) = step {
            if let Some(v) = &vid {
                store.append_event(session_id, &RunEvent::FlowReturned { value: v.clone() })?;
            }
            return Ok(FlowOutcome {
                returned: vid,
                result: last,
                transcript: transcript.join("\n\n"),
                steps,
                suspension: None,
            });
        }
        i += 1;
    }

    Ok(FlowOutcome {
        returned: None,
        result: last,
        transcript: transcript.join("\n\n"),
        steps,
        suspension: None,
    })
}

/// Coerce a resume `input` against the `await`'s declared `as_type` — lenient, like the type checker:
/// a string reply is parsed to a number/bool when the await asked for one; everything else is kept
/// verbatim (no hard failures — an un-coercible value flows through as-is).
fn coerce_await_input(input: Value, as_type: &Option<TypeRef>) -> Value {
    match (as_type, &input) {
        (Some(TypeRef::Number), Value::String(s)) => {
            s.trim().parse::<f64>().map(Value::Number).unwrap_or(input)
        }
        (Some(TypeRef::Bool), Value::String(s)) => match s.trim() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => input,
        },
        _ => input,
    }
}

/// Execute a [`PhysicalPlan`] (from [`crate::optimize::optimize`]) over a flow's top-level `body`,
/// reusing the interpreter per node. `Sequential`/`ApprovalFence` run one node in place;
/// `Parallel` runs its nodes concurrently — each on a buffering sink, replayed into the real sink in
/// stage order so output never interleaves. The result is equivalent to [`execute_flow`] for any plan
/// the optimizer emits (it only batches provably-independent read-only nodes).
pub async fn execute_plan(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    body: &[Node],
    plan: &PhysicalPlan,
    sink: &mut dyn FlowSink,
) -> Result<FlowOutcome> {
    let mut steps = 0usize;
    let mut transcript: Vec<String> = Vec::new();
    let mut last = String::new();
    let mut returned: Option<ValueId> = None;

    let node_at = |id: &crate::ast::NodeId| -> Result<&Node> {
        body.get(id.0 as usize).ok_or_else(|| {
            FlowError::Runtime(format!("plan references node {} out of range", id.0))
        })
    };

    'stages: for stage in &plan.stages {
        match stage {
            Stage::Sequential(id) | Stage::ApprovalFence(id) => {
                let node = node_at(id)?;
                let (text, _lv, step) = exec_body(
                    store,
                    executor,
                    session_id,
                    std::slice::from_ref(node),
                    sink,
                    &mut steps,
                    &mut transcript,
                )
                .await?;
                last = text;
                if let Step::Return(vid) = step {
                    returned = vid;
                    break 'stages;
                }
            }
            Stage::Parallel(ids) => {
                let futs = ids.iter().map(|id| async move {
                    let node = node_at(id)?;
                    let mut buf = BufferSink::default();
                    let mut s = 0usize;
                    let mut tr: Vec<String> = Vec::new();
                    let (text, _lv, step) = exec_body(
                        store,
                        executor,
                        session_id,
                        std::slice::from_ref(node),
                        &mut buf,
                        &mut s,
                        &mut tr,
                    )
                    .await?;
                    Ok::<_, FlowError>((buf, s, tr, text, step))
                });
                let results = futures::future::try_join_all(futs).await?;
                for (buf, s, tr, text, step) in results {
                    if let Step::Return(_) = step {
                        return Err(FlowError::Runtime(
                            "`return` is not allowed inside a parallel stage".to_string(),
                        ));
                    }
                    buf.replay(&mut *sink);
                    steps += s;
                    transcript.extend(tr);
                    last = text;
                }
            }
            Stage::Branch(_) | Stage::Repeat(_) | Stage::Await(_) => {
                return Err(FlowError::Runtime(
                    "execute_plan v1 supports only sequential/parallel/approval_fence stages"
                        .to_string(),
                ));
            }
        }
    }

    if let Some(vid) = &returned {
        store.append_event(session_id, &RunEvent::FlowReturned { value: vid.clone() })?;
    }
    Ok(FlowOutcome {
        returned,
        result: last,
        transcript: transcript.join("\n\n"),
        steps,
        // The optimized plan path does not suspend: the optimizer never emits `Stage::Await`, and a
        // top-level `await` reaching `exec_body` errors. Cross-turn suspend goes through `execute_flow`.
        suspension: None,
    })
}

/// Execute a sequence of nodes, returning the last produced text and whether a `return` unwound the
/// flow. Boxed because `when`/`repeat` recurse into nested bodies (async recursion needs indirection).
fn exec_body<'a>(
    store: &'a dyn ValueStore,
    executor: &'a dyn OpHost,
    session_id: &'a str,
    body: &'a [Node],
    sink: &'a mut dyn FlowSink,
    steps: &'a mut usize,
    transcript: &'a mut Vec<String>,
) -> BodyFuture<'a> {
    Box::pin(async move {
        let mut last = String::new();
        // The value id the body's most recent op produced — so `seq`/`each`/`parallel`/`pipe` can
        // bind a block's result without the caller threading a separate accumulator.
        let mut last_value: Option<ValueId> = None;
        for node in body {
            match node {
                Node::Bind {
                    name, value, ty, ..
                } => {
                    // Pure nodes (expr/fmt/jq) may appear as a bind value without going through
                    // execute_call — they are side-effect-free and need no dispatch envelope.
                    match value.as_ref() {
                        Node::Expr { formula, vars } => {
                            let resolved: std::collections::BTreeMap<String, f64> = vars
                                .iter()
                                .map(|(k, v)| {
                                    let jv = eval_arg(v, store, session_id)?;
                                    let n = match &jv {
                                        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
                                        serde_json::Value::String(s) => {
                                            s.parse::<f64>().unwrap_or(0.0)
                                        }
                                        _ => 0.0,
                                    };
                                    Ok::<_, crate::FlowError>((k.clone(), n))
                                })
                                .collect::<Result<_>>()?;
                            let result = eval_expr_formula(formula, &resolved)?;
                            let text = format_number(result);
                            let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                            let ty_label = ty.as_ref().map(TypeRef::label);
                            store.bind(
                                session_id,
                                name,
                                &vid,
                                ty_label.as_deref(),
                                &summarize(&text),
                                Visibility::Visible,
                            )?;
                            transcript.push(format!("[${} = expr {formula}]\n{text}", name.0));
                            last = text;
                            last_value = Some(vid);
                            continue;
                        }
                        Node::Fmt { template } => {
                            let text = interpolate_str(template, store, session_id);
                            let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                            let ty_label = ty.as_ref().map(TypeRef::label);
                            store.bind(
                                session_id,
                                name,
                                &vid,
                                ty_label.as_deref(),
                                &summarize(&text),
                                Visibility::Visible,
                            )?;
                            transcript.push(format!("[${} = fmt]\n{text}", name.0));
                            last = text;
                            last_value = Some(vid);
                            continue;
                        }
                        Node::Jq { path, input } => {
                            let jv = eval_arg(input, store, session_id)?;
                            let result = eval_jq_path(path, &jv)?;
                            let text = match &result {
                                serde_json::Value::String(s) => s.clone(),
                                other => serde_json::to_string(other).unwrap_or_default(),
                            };
                            let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                            let ty_label = ty.as_ref().map(TypeRef::label);
                            store.bind(
                                session_id,
                                name,
                                &vid,
                                ty_label.as_deref(),
                                &summarize(&text),
                                Visibility::Visible,
                            )?;
                            transcript.push(format!("[${} = jq {path}]\n{text}", name.0));
                            last = text;
                            last_value = Some(vid);
                            continue;
                        }
                        Node::Parse {
                            value: inner,
                            as_type,
                        } => {
                            let jv = eval_arg(inner, store, session_id)?;
                            let text = coerce_parse(&jv, as_type)?;
                            let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                            let ty_label = ty.as_ref().map(TypeRef::label);
                            store.bind(
                                session_id,
                                name,
                                &vid,
                                ty_label.as_deref(),
                                &summarize(&text),
                                Visibility::Visible,
                            )?;
                            transcript.push(format!("[${} = parse {as_type}]\n{text}", name.0));
                            last = text;
                            last_value = Some(vid);
                            continue;
                        }
                        _ => {}
                    }
                    let Node::Call { op, args } = value.as_ref() else {
                        return Err(crate::FlowError::Runtime(
                            "execution can only bind the result of a `call`, `expr`, `fmt`, `jq`, or `parse`".to_string(),
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
                        return Err(crate::FlowError::Runtime(format!(
                            "step `{op}` failed: {}",
                            outcome.content
                        )));
                    }
                    // The model reasons over intermediate results → feed the model-facing VIEW
                    // (line-numbered read, diff, …). Control flow (`when`/`return`) stays canonical.
                    // Record EVERY node's view in the transcript so the round feedback surfaces all of
                    // a plan's reads, not just the last one. Oversized views are trimmed so one huge
                    // result can't blow the round's context budget (the canonical value is untouched).
                    let view = executor.trim_output(outcome.view.clone(), op);
                    transcript.push(format!("[${} = {op}]\n{view}", name.0));
                    last = outcome.view;
                    last_value = outcome.value_id;
                }
                Node::Call { op, args } => {
                    let outcome =
                        run_call(store, executor, session_id, op, args, None, sink).await?;
                    *steps += 1;
                    if outcome.is_error {
                        return Err(crate::FlowError::Runtime(format!(
                            "step `{op}` failed: {}",
                            outcome.content
                        )));
                    }
                    // The model reasons over intermediate results → feed the model-facing VIEW
                    // (line-numbered read, diff, …). Control flow (`when`/`return`) stays canonical.
                    // Oversized views are trimmed (canonical value untouched).
                    let view = executor.trim_output(outcome.view.clone(), op);
                    transcript.push(format!("[{op}]\n{view}"));
                    last = outcome.view;
                    last_value = outcome.value_id;
                }
                Node::Ctx {
                    name,
                    purpose,
                    include,
                    exclude,
                    budget,
                } => {
                    let members: Vec<SymbolName> = include
                        .iter()
                        .filter(|s| !exclude.contains(s))
                        .cloned()
                        .collect();
                    let vid = build_ctx(
                        store, session_id, name, purpose, &members, *budget, transcript,
                    )?;
                    last = format!("ctx {}", name.0);
                    last_value = Some(vid);
                }
                Node::CtxAppend { ctx, add } => {
                    let vid = append_ctx(store, session_id, ctx, add, transcript)?;
                    last = format!("ctx += {}", ctx.0);
                    last_value = Some(vid);
                }
                Node::Return { value } => {
                    let (content, vid) =
                        eval_return(store, executor, session_id, value, sink, steps).await?;
                    return Ok((content, vid.clone(), Step::Return(vid)));
                }
                Node::When {
                    cond,
                    then,
                    otherwise,
                } => {
                    let take = eval_cond(store, executor, session_id, cond, sink, steps).await?;
                    let branch = if take { then } else { otherwise };
                    let (blast, bvid, step) = exec_body(
                        store,
                        executor,
                        session_id,
                        branch,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await?;
                    if !blast.is_empty() {
                        last = blast;
                        last_value = bvid;
                    }
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Repeat {
                    max,
                    until,
                    body: rbody,
                    collect,
                } => {
                    let mut repeat_collected: Vec<ValueId> = Vec::new();
                    for _ in 0..*max {
                        let (blast, bvid, step) = exec_body(
                            store,
                            executor,
                            session_id,
                            rbody,
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await?;
                        if !blast.is_empty() {
                            last = blast;
                        }
                        if let Some(v) = bvid {
                            if collect.is_some() {
                                repeat_collected.push(v.clone());
                            }
                            last_value = Some(v);
                        }
                        if let Step::Return(v) = step {
                            return Ok((last, v.clone(), Step::Return(v)));
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
                    if let Some(cname) = collect {
                        let items: Vec<Value> = repeat_collected
                            .iter()
                            .filter_map(|vid| store.get_value(vid).ok().flatten())
                            .collect();
                        let list_val = Value::List(items);
                        let list_vid = store.put_value(session_id, &list_val)?;
                        bind_existing(store, session_id, cname, &list_vid)?;
                        last_value = Some(list_vid);
                    }
                }
                Node::Each {
                    source,
                    item,
                    body: ebody,
                    collect,
                    flat,
                } => {
                    let list = eval_arg(source, store, session_id)?;
                    let serde_json::Value::Array(elems) = list else {
                        return Err(crate::FlowError::Runtime(
                            "`each` source must evaluate to a list".to_string(),
                        ));
                    };
                    let mut collected: Vec<ValueId> = Vec::new();
                    for elem in &elems {
                        let vid = store.put_value(session_id, &Value::from_json(elem))?;
                        bind_existing(store, session_id, item, &vid)?;
                        let (blast, bvid, step) = exec_body(
                            store,
                            executor,
                            session_id,
                            ebody,
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await?;
                        if !blast.is_empty() {
                            last = blast;
                        }
                        if let Some(v) = bvid {
                            collected.push(v.clone());
                            last_value = Some(v);
                        }
                        if let Step::Return(v) = step {
                            return Ok((last, v.clone(), Step::Return(v)));
                        }
                    }
                    if let Some(cname) = collect {
                        let items: Vec<Value> = collected
                            .iter()
                            .filter_map(|vid| store.get_value(vid).ok().flatten())
                            .collect();
                        let list_val = if *flat {
                            // Flatten one level: each item must be a Value::List;
                            // non-list items are included as-is (graceful).
                            let mut flat_items: Vec<Value> = Vec::new();
                            for item in items {
                                match item {
                                    Value::List(inner) => flat_items.extend(inner),
                                    other => flat_items.push(other),
                                }
                            }
                            Value::List(flat_items)
                        } else {
                            Value::List(items)
                        };
                        let list_vid = store.put_value(session_id, &list_val)?;
                        bind_existing(store, session_id, cname, &list_vid)?;
                        last_value = Some(list_vid);
                    }
                }
                Node::Assert { cond, message } => {
                    let ok = eval_cond(store, executor, session_id, cond, &mut *sink, &mut *steps)
                        .await?;
                    if !ok {
                        let detail = message
                            .clone()
                            .unwrap_or_else(|| "condition is false".to_string());
                        return Err(FlowError::Core(Error::AssertFailed(detail)));
                    }
                }
                Node::Pipe {
                    steps: psteps,
                    bind,
                } => {
                    let mut prev: Option<ValueId> = None;
                    for step in psteps {
                        let Node::Call { op, args } = step else {
                            return Err(FlowError::Runtime(
                                "`pipe` steps must be `call` nodes".to_string(),
                            ));
                        };
                        // Splice the previous step's result as this step's first argument.
                        let synth_args: Vec<Node> = match &prev {
                            Some(pvid) => {
                                let pjson = store
                                    .get_value(pvid)?
                                    .ok_or_else(|| {
                                        Error::Other("dangling value in `pipe`".to_string())
                                    })?
                                    .to_json();
                                let mut a = Vec::with_capacity(args.len() + 1);
                                a.push(Node::Lit { value: pjson });
                                a.extend(args.iter().cloned());
                                a
                            }
                            None => args.clone(),
                        };
                        let outcome = run_call(
                            store,
                            executor,
                            session_id,
                            op,
                            &synth_args,
                            None,
                            &mut *sink,
                        )
                        .await?;
                        *steps += 1;
                        if outcome.is_error {
                            return Err(FlowError::Runtime(format!(
                                "pipe step `{op}` failed: {}",
                                outcome.content
                            )));
                        }
                        transcript.push(format!("[pipe {op}]\n{}", outcome.view));
                        last = outcome.view;
                        prev = outcome.value_id;
                    }
                    if let Some(name) = bind {
                        if let Some(vid) = &prev {
                            bind_existing(store, session_id, name, vid)?;
                        }
                    }
                    last_value = prev;
                }
                Node::Seq { body: sbody, bind } => {
                    let (blast, bvid, step) = exec_body(
                        store,
                        executor,
                        session_id,
                        sbody,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await?;
                    if !blast.is_empty() {
                        last = blast;
                    }
                    last_value = bvid.clone();
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                    if let (Some(name), Some(vid)) = (bind, &bvid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                }
                Node::Memo {
                    name, value, ty, ..
                } => {
                    let Node::Call { op, args } = value.as_ref() else {
                        return Err(FlowError::Runtime(
                            "`memo` can only bind the result of a `call`".to_string(),
                        ));
                    };
                    // Pinned across turns: if the symbol is already resolved for this session, reuse
                    // the cached value and skip execution (compute-once-per-session, keyed on name).
                    if let Some(existing) = store.resolve(session_id, name)? {
                        let text = store
                            .get_value(&existing)?
                            .map(|v| value_text(&v))
                            .unwrap_or_default();
                        transcript.push(format!("[${} = memo {op} (cached)]\n{text}", name.0));
                        if !text.is_empty() {
                            last = text;
                        }
                        last_value = Some(existing);
                        continue;
                    }
                    let ty_label = ty.as_ref().map(TypeRef::label);
                    let bspec = BindSpec {
                        name,
                        ty: ty_label.as_deref(),
                        visibility: Visibility::Visible,
                    };
                    let outcome =
                        run_call(store, executor, session_id, op, args, Some(bspec), sink).await?;
                    *steps += 1;
                    if outcome.is_error {
                        return Err(FlowError::Runtime(format!(
                            "step `{op}` failed: {}",
                            outcome.content
                        )));
                    }
                    transcript.push(format!("[${} = memo {op}]\n{}", name.0, outcome.view));
                    last = outcome.view;
                    last_value = outcome.value_id;
                }
                Node::Parallel { branches } => {
                    // Run each branch concurrently, each writing to its own buffering sink; after the
                    // join, replay the buffers into the real sink in branch order so concurrent output
                    // doesn't interleave. Every op still dispatches through the same envelope.
                    let futs = branches.iter().map(|b| async move {
                        let mut buf = BufferSink::default();
                        let mut s = 0usize;
                        let mut tr: Vec<String> = Vec::new();
                        let (text, lv, step) = exec_body(
                            store, executor, session_id, &b.body, &mut buf, &mut s, &mut tr,
                        )
                        .await?;
                        Ok::<_, FlowError>((b, buf, s, tr, text, lv, step))
                    });
                    let results = futures::future::try_join_all(futs).await?;
                    for (b, buf, s, tr, text, lv, step) in results {
                        if let Step::Return(_) = step {
                            return Err(FlowError::Runtime(
                                "`return` is not allowed inside a `parallel` branch".to_string(),
                            ));
                        }
                        buf.replay(&mut *sink);
                        *steps += s;
                        transcript.extend(tr);
                        if let Some(vid) = lv {
                            bind_existing(store, session_id, &b.name, &vid)?;
                            last = text;
                            last_value = Some(vid);
                        }
                    }
                }
                Node::Retry {
                    max,
                    backoff,
                    delay_ms,
                    body: rbody,
                    bind,
                } => {
                    let base_ms = delay_ms.unwrap_or(500);
                    let backoff_kind = backoff.as_deref().unwrap_or("none");
                    let mut last_err = String::new();
                    let mut succeeded = false;
                    let mut last_vid: Option<ValueId> = None;
                    for attempt in 0..*max {
                        if attempt > 0 {
                            let wait = match backoff_kind {
                                "linear" => base_ms * attempt as u64,
                                "exponential" => base_ms * (1u64 << (attempt - 1).min(10)),
                                _ => base_ms,
                            };
                            if wait > 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                            }
                        }
                        match exec_body(
                            store,
                            executor,
                            session_id,
                            rbody,
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await
                        {
                            Ok((blast, bvid, step)) => {
                                if !blast.is_empty() {
                                    last = blast;
                                }
                                last_vid = bvid;
                                if let Step::Return(v) = step {
                                    return Ok((last, v.clone(), Step::Return(v)));
                                }
                                succeeded = true;
                                break;
                            }
                            Err(e) => {
                                // Fatal errors must not be retried — propagate immediately.
                                if matches!(
                                    e,
                                    FlowError::Core(Error::AssertFailed(_))
                                        | FlowError::Core(Error::ConfirmDenied(_))
                                ) {
                                    return Err(e);
                                }
                                last_err = e.to_string();
                            }
                        }
                    }
                    if !succeeded {
                        return Err(FlowError::Runtime(format!(
                            "`retry` exhausted {} attempt(s): {}",
                            max, last_err
                        )));
                    }
                    if let (Some(name), Some(vid)) = (bind, &last_vid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                    last_value = last_vid;
                }
                Node::Try {
                    body: tbody,
                    catch,
                    handler,
                } => {
                    match exec_body(
                        store,
                        executor,
                        session_id,
                        tbody,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await
                    {
                        Ok((blast, bvid, step)) => {
                            if !blast.is_empty() {
                                last = blast;
                            }
                            last_value = bvid;
                            if let Step::Return(v) = step {
                                return Ok((last, v.clone(), Step::Return(v)));
                            }
                        }
                        Err(e) => {
                            if let Some(cname) = catch {
                                let err_vid =
                                    store.put_value(session_id, &Value::String(e.to_string()))?;
                                bind_existing(store, session_id, cname, &err_vid)?;
                            }
                            let (hblast, hvid, hstep) = exec_body(
                                store,
                                executor,
                                session_id,
                                handler,
                                &mut *sink,
                                &mut *steps,
                                &mut *transcript,
                            )
                            .await?;
                            if !hblast.is_empty() {
                                last = hblast;
                            }
                            last_value = hvid;
                            if let Step::Return(v) = hstep {
                                return Ok((last, v.clone(), Step::Return(v)));
                            }
                        }
                    }
                }
                Node::Confirm {
                    message,
                    risk,
                    body: cbody,
                } => {
                    let intents = IntentSet::new();
                    let risk_tag = risk.as_deref().unwrap_or("medium");
                    let labelled = format!("[{risk_tag}] {message}");
                    let choice = executor.request_approval(&labelled, &intents).await;
                    if !matches!(choice, ApprovalChoice::Allow) {
                        return Err(FlowError::Core(Error::ConfirmDenied(message.clone())));
                    }
                    let (blast, bvid, step) = exec_body(
                        store,
                        executor,
                        session_id,
                        cbody,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await?;
                    if !blast.is_empty() {
                        last = blast;
                    }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Loop {
                    for_ms,
                    every_ms,
                    until,
                    body: lbody,
                    bind,
                } => {
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_millis(*for_ms);
                    let mut last_vid: Option<ValueId> = None;
                    loop {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        match exec_body(
                            store,
                            executor,
                            session_id,
                            lbody,
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await
                        {
                            Ok((blast, bvid, step)) => {
                                if !blast.is_empty() {
                                    last = blast;
                                }
                                last_vid = bvid;
                                if let Step::Return(v) = step {
                                    return Ok((last, v.clone(), Step::Return(v)));
                                }
                            }
                            Err(e) => {
                                return Err(FlowError::Runtime(format!("`loop` body failed: {e}")));
                            }
                        }
                        if let Some(u) = until {
                            if eval_cond(store, executor, session_id, u, &mut *sink, &mut *steps)
                                .await?
                            {
                                break;
                            }
                        }
                        if *every_ms > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(*every_ms)).await;
                        }
                    }
                    if let (Some(name), Some(vid)) = (bind, &last_vid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                    last_value = last_vid;
                }
                Node::Race {
                    timeout_ms,
                    branches,
                    bind,
                } => {
                    // True first-wins concurrency: spawn each branch, drive with tokio::select!,
                    // cancel losers. Same BufferSink pattern as Node::Parallel.
                    // A zero deadline can accommodate no work: `tokio::time::timeout` polls the
                    // inner future before the (already-elapsed) timer, so an immediately-ready
                    // branch would otherwise win a 0ms race. Short-circuit so the deadline holds.
                    if *timeout_ms == 0 {
                        return Err(FlowError::Runtime(format!(
                            "`race` timed out after {timeout_ms}ms with no successful branch"
                        )));
                    }
                    let remaining = std::time::Duration::from_millis(*timeout_ms);
                    let race_result: Option<(String, Option<ValueId>, Step)> =
                        tokio::time::timeout(remaining, async {
                            // We can't use macro select! over a dynamic list, so we poll branches
                            // as ordered futures but with a shared deadline enforced by the outer
                            // timeout — meaning we truly give each branch a chance concurrently
                            // by joining them and taking the first Ok.
                            let futs: Vec<_> = branches
                                .iter()
                                .map(|b| {
                                    let body = &b.body;
                                    Box::pin(async move {
                                        let mut buf = BufferSink::default();
                                        let mut s = 0usize;
                                        let mut tr: Vec<String> = Vec::new();
                                        exec_body(
                                            store, executor, session_id, body, &mut buf, &mut s,
                                            &mut tr,
                                        )
                                        .await
                                        .map(|(text, lv, step)| (text, lv, step, buf, s, tr))
                                    })
                                })
                                .collect();
                            // Race: futures::future::select_ok returns first success
                            futures::future::select_ok(futs).await.ok()
                        })
                        .await
                        .ok()
                        .flatten()
                        .map(|((text, lv, step, buf, s, tr), _rest)| {
                            buf.replay(&mut *sink);
                            *steps += s;
                            transcript.extend(tr);
                            (text, lv, step)
                        });
                    let (blast, bvid, step) = race_result.ok_or_else(|| {
                        FlowError::Runtime(format!(
                            "`race` timed out after {timeout_ms}ms with no successful branch"
                        ))
                    })?;
                    if !blast.is_empty() {
                        last = blast;
                    }
                    if let (Some(name), Some(vid)) = (bind, &bvid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Throttle {
                    name: tname,
                    max,
                    window_ms,
                    body: tbody,
                } => {
                    // Token-bucket: track call timestamps in the session store keyed by `name`.
                    // Keying on `name` means the bucket persists correctly across turns and
                    // different throttle nodes with different names never share a bucket.
                    let bucket_key = SymbolName(format!("__throttle_bucket_{tname}"));
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let window_start = now_ms.saturating_sub(*window_ms);
                    // Load existing timestamps.
                    let mut times: Vec<u64> =
                        if let Some(vid) = store.resolve(session_id, &bucket_key).ok().flatten() {
                            if let Some(Value::String(s)) = store.get_value(&vid).ok().flatten() {
                                serde_json::from_str::<Vec<u64>>(&s).unwrap_or_default()
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        };
                    // Evict expired entries.
                    times.retain(|&t| t >= window_start);
                    if times.len() >= *max as usize {
                        return Err(FlowError::Runtime(format!(
                            "`throttle` limit of {max} per {window_ms}ms exceeded"
                        )));
                    }
                    times.push(now_ms);
                    let times_json = serde_json::to_string(&times).unwrap_or_default();
                    let vid = store.put_value(session_id, &Value::String(times_json))?;
                    store.bind(session_id, &bucket_key, &vid, None, "", Visibility::Hidden)?;
                    let (blast, bvid, step) =
                        exec_body(store, executor, session_id, tbody, sink, steps, transcript)
                            .await?;
                    if !blast.is_empty() {
                        last = blast;
                    }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Debounce {
                    name: _dname,
                    wait_ms,
                    body: dbody,
                } => {
                    // Debounce: sleep for wait_ms then run body once.
                    // `name` is a stable key (used for future cross-turn debounce state);
                    // currently the settling delay is implemented as a fixed sleep.
                    tokio::time::sleep(std::time::Duration::from_millis(*wait_ms)).await;
                    let (blast, bvid, step) =
                        exec_body(store, executor, session_id, dbody, sink, steps, transcript)
                            .await?;
                    if !blast.is_empty() {
                        last = blast;
                    }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Unless { cond, body: ubody } => {
                    // Sugar for `when !cond`: run body only when condition is falsey.
                    let take =
                        !eval_cond(store, executor, session_id, cond, &mut *sink, &mut *steps)
                            .await?;
                    if take {
                        let (blast, bvid, step) = exec_body(
                            store,
                            executor,
                            session_id,
                            ubody,
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await?;
                        if !blast.is_empty() {
                            last = blast;
                            last_value = bvid;
                        }
                        if let Step::Return(v) = step {
                            return Ok((last, v.clone(), Step::Return(v)));
                        }
                    }
                }
                Node::Verify {
                    cmd,
                    expect,
                    message,
                } => {
                    // Run `cmd`, check output contains/matches `expect`; abort with `message` if not.
                    let (cmd_text, _) =
                        eval_return(store, executor, session_id, cmd, &mut *sink, &mut *steps)
                            .await?;
                    let expect_val = eval_arg(expect, store, session_id)?;
                    let pattern = match &expect_val {
                        serde_json::Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    let ok = cmd_text.contains(pattern.as_str());
                    if !ok {
                        let detail = message
                            .clone()
                            .unwrap_or_else(|| format!("output did not contain {:?}", pattern));
                        return Err(FlowError::Runtime(format!("verify failed: {detail}")));
                    }
                    transcript.push(format!("[verify ok] {pattern}"));
                    last = format!("verify ok: {pattern}");
                }
                Node::Peek { name } => {
                    // Read the current in-session value of a named symbol — zero IO.
                    let text = match store.resolve(session_id, name)? {
                        Some(vid) => store
                            .get_value(&vid)?
                            .map(|v| value_text(&v))
                            .unwrap_or_default(),
                        None => String::new(),
                    };
                    transcript.push(format!("[peek ${}]\n{text}", name.0));
                    last = text;
                }
                Node::Expr { formula, vars } => {
                    // Pure arithmetic — no IO, no approval gate.
                    let resolved: std::collections::BTreeMap<String, f64> = vars
                        .iter()
                        .map(|(k, v)| {
                            let jv = eval_arg(v, store, session_id)?;
                            let n = match &jv {
                                serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
                                serde_json::Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
                                _ => 0.0,
                            };
                            Ok::<_, crate::FlowError>((k.clone(), n))
                        })
                        .collect::<Result<_>>()?;
                    let result = eval_expr_formula(formula, &resolved)?;
                    let text = format_number(result);
                    let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                    transcript.push(format!("[expr {formula}]\n{text}"));
                    last = text;
                    last_value = Some(vid);
                }
                Node::Fmt { template } => {
                    // Pure string interpolation — substitutes {sym} from session symbols.
                    let text = interpolate_str(template, store, session_id);
                    let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                    transcript.push(format!("[fmt]\n{text}"));
                    last = text;
                    last_value = Some(vid);
                }
                Node::Jq { path, input } => {
                    // Pure JSON path extraction — no IO.
                    let jv = eval_arg(input, store, session_id)?;
                    let result = eval_jq_path(path, &jv)?;
                    let text = match &result {
                        serde_json::Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                    transcript.push(format!("[jq {path}]\n{text}"));
                    last = text;
                    last_value = Some(vid);
                }
                Node::Parse {
                    value: inner,
                    as_type,
                } => {
                    // Pure type coercion — no IO.
                    let jv = eval_arg(inner, store, session_id)?;
                    let text = coerce_parse(&jv, as_type)?;
                    let vid = store.put_value(session_id, &Value::String(text.clone()))?;
                    transcript.push(format!("[parse {as_type}]\n{text}"));
                    last = text;
                    last_value = Some(vid);
                }
                Node::Match {
                    subject,
                    cases,
                    default,
                } => {
                    let subj = eval_arg(subject, store, session_id)?;
                    let mut branch: Option<&[Node]> = None;
                    for case in cases {
                        if eval_arg(&case.value, store, session_id)? == subj {
                            branch = Some(&case.body);
                            break;
                        }
                    }
                    let branch = match branch {
                        Some(b) => b,
                        None if !default.is_empty() => default.as_slice(),
                        None => {
                            return Err(FlowError::Runtime(format!(
                                "`match` had no case for `{}` and no default",
                                serde_json::to_string(&subj).unwrap_or_default()
                            )));
                        }
                    };
                    let (blast, bvid, step) = exec_body(
                        store,
                        executor,
                        session_id,
                        branch,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await?;
                    if !blast.is_empty() {
                        last = blast;
                        last_value = bvid;
                    }
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Route {
                    selector,
                    cases,
                    default,
                } => {
                    // Resolve the selector to a label. A `call` selector dispatches (the `!model` op);
                    // a var/lit resolves without dispatch. The model picks *which* declared case runs.
                    let label = match selector.as_ref() {
                        Node::Call { op, args } => {
                            let outcome =
                                run_call(store, executor, session_id, op, args, None, sink).await?;
                            *steps += 1;
                            if outcome.is_error {
                                return Err(FlowError::Runtime(format!(
                                    "`route` selector `{op}` failed: {}",
                                    outcome.content
                                )));
                            }
                            outcome.content.trim().to_string()
                        }
                        other => match eval_arg(other, store, session_id)? {
                            serde_json::Value::String(s) => s,
                            v => serde_json::to_string(&v).unwrap_or_default(),
                        },
                    };
                    let branch = cases
                        .iter()
                        .find(|c| c.label == label)
                        .map(|c| c.body.as_slice());
                    let branch = match branch {
                        Some(b) => b,
                        None if !default.is_empty() => default.as_slice(),
                        None => {
                            return Err(FlowError::Runtime(format!(
                                "`route` selector returned `{label}`, which matches no case and there is no default"
                            )));
                        }
                    };
                    let (blast, bvid, step) = exec_body(
                        store,
                        executor,
                        session_id,
                        branch,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await?;
                    if !blast.is_empty() {
                        last = blast;
                        last_value = bvid;
                    }
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Fallback { branches, bind } => {
                    // Try each branch in order; the first non-empty success wins. An empty success is
                    // kept only as a last resort; if every branch errors, the last error propagates.
                    let mut win: Option<(String, Option<ValueId>)> = None;
                    let mut last_err: Option<FlowError> = None;
                    for b in branches {
                        match exec_body(
                            store,
                            executor,
                            session_id,
                            &b.body,
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await
                        {
                            Ok((blast, bvid, step)) => {
                                if let Step::Return(v) = step {
                                    let l = if blast.is_empty() {
                                        last.clone()
                                    } else {
                                        blast
                                    };
                                    return Ok((l, v.clone(), Step::Return(v)));
                                }
                                if !blast.is_empty() {
                                    win = Some((blast, bvid));
                                    break;
                                }
                                if win.is_none() {
                                    win = Some((blast, bvid));
                                }
                            }
                            Err(e) => last_err = Some(e),
                        }
                    }
                    match win {
                        Some((blast, bvid)) => {
                            if !blast.is_empty() {
                                last = blast;
                            }
                            if let (Some(name), Some(vid)) = (bind, &bvid) {
                                bind_existing(store, session_id, name, vid)?;
                            }
                            last_value = bvid;
                        }
                        None => {
                            if let Some(e) = last_err {
                                return Err(e);
                            }
                        }
                    }
                }
                Node::Timeout {
                    ms,
                    body: tbody,
                    bind,
                } => {
                    if *ms == 0 {
                        return Err(FlowError::Runtime(
                            "`timeout` of 0ms cannot complete any work".to_string(),
                        ));
                    }
                    let dur = std::time::Duration::from_millis(*ms);
                    // Buffer only the *sink* so a partial run discarded on timeout doesn't tear the
                    // live output. Thread the real `steps`/`transcript` in, so dispatches that DID
                    // complete before the deadline stay counted (an enclosing `budget` must see them)
                    // and audited (the run trace must not silently omit side effects that happened).
                    let res = tokio::time::timeout(dur, async {
                        let mut buf = BufferSink::default();
                        exec_body(
                            store,
                            executor,
                            session_id,
                            tbody,
                            &mut buf,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await
                        .map(|(text, lv, step)| (text, lv, step, buf))
                    })
                    .await;
                    match res {
                        Ok(Ok((text, lv, step, buf))) => {
                            buf.replay(&mut *sink);
                            if !text.is_empty() {
                                last = text;
                            }
                            if let (Some(name), Some(vid)) = (bind, &lv) {
                                bind_existing(store, session_id, name, vid)?;
                            }
                            last_value = lv;
                            if let Step::Return(v) = step {
                                return Ok((last, v.clone(), Step::Return(v)));
                            }
                        }
                        Ok(Err(e)) => return Err(e),
                        Err(_) => {
                            return Err(FlowError::Runtime(format!("`timeout` exceeded {ms}ms")));
                        }
                    }
                }
                Node::Budget {
                    limit,
                    body: bbody,
                    bind,
                } => {
                    // Cost cap: allow at most `limit` op dispatches in this scope, checked at each
                    // statement boundary (a single statement may still overshoot — documented v1).
                    let start = *steps;
                    let cap = *limit as usize;
                    let mut bvid: Option<ValueId> = None;
                    for stmt in bbody {
                        if (*steps).saturating_sub(start) >= cap {
                            return Err(FlowError::Runtime(format!(
                                "`budget` exceeded: at most {cap} op dispatch(es) allowed in this scope"
                            )));
                        }
                        let (blast, svid, step) = exec_body(
                            store,
                            executor,
                            session_id,
                            std::slice::from_ref(stmt),
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await?;
                        if !blast.is_empty() {
                            last = blast;
                        }
                        if let Some(v) = svid {
                            last_value = Some(v.clone());
                            bvid = Some(v);
                        }
                        if let Step::Return(v) = step {
                            return Ok((last, v.clone(), Step::Return(v)));
                        }
                    }
                    if let (Some(name), Some(vid)) = (bind, &bvid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                }
                Node::Await { .. } => {
                    // Top-level awaits are intercepted by `run_top_level` (which suspends the flow);
                    // reaching here means an `await` was nested or run via the optimized plan path,
                    // neither of which can suspend in v1. The analyzer rejects the nested case.
                    return Err(FlowError::Runtime(
                        "`await` must be a top-level flow statement (it suspends the whole flow for cross-turn resume; it cannot be nested or run in the optimized plan path)"
                            .to_string(),
                    ));
                }
                Node::Var { .. } | Node::Lit { .. } | Node::Thing { .. } => {
                    return Err(FlowError::Runtime(
                        "a bare value is not an executable statement".to_string(),
                    ));
                }
            }
        }
        Ok((last, last_value, Step::Next))
    })
}

/// Evaluate a `when` / `repeat-until` condition to a boolean. `lit`/`var` are resolved without side
/// effects; a `call` executes (its content's truthiness is the result). An errored call is falsey.
async fn eval_cond(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    node: &Node,
    sink: &mut dyn FlowSink,
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
        other => Err(FlowError::Runtime(format!(
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
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    op: &str,
    args: &[Node],
    bind: Option<BindSpec<'_>>,
    sink: &mut dyn FlowSink,
) -> Result<CallOutcome> {
    let arg_values = args
        .iter()
        .map(|a| eval_arg(a, store, session_id))
        .collect::<Result<Vec<_>>>()?;
    let input = map_args_to_input(op, arg_values, executor.catalog())?;
    sink.tool_call(op, &input);
    let outcome = execute_call(store, executor, session_id, op, input, bind).await?;
    // Surface the model-facing VIEW (numbered read, diff, …) to the sink — what the model/user sees.
    // The canonical `outcome.content` remains what control flow and interpolation use.
    sink.tool_result(
        op,
        &OpOutcome {
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
fn eval_arg(node: &Node, store: &dyn ValueStore, session_id: &str) -> Result<serde_json::Value> {
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
        other => Err(FlowError::Runtime(format!(
            "unsupported call argument `{}` — only `lit` and `var` ($symbol) nodes are valid call \
             args. To use a computed string (fmt/expr/jq/parse), `bind` it to a symbol first, then \
             pass that symbol as a `var` arg.",
            node_kind(other)
        ))),
    }
}

/// Substitute `{{symbol}}` tokens inside string literals with the resolved symbol's text value, so the
/// model can embed a stored value into a larger string. Recurses through strings in arrays/objects;
/// non-string scalars pass through. A token whose symbol isn't bound is left verbatim.
fn interpolate(
    value: &serde_json::Value,
    store: &dyn ValueStore,
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
fn interpolate_str(s: &str, store: &dyn ValueStore, session_id: &str) -> String {
    // Expand a leading `~/` (or bare `~`) to the home directory so fmt
    // templates like `"~/.flux/foo"` work without shelling out.
    let expanded: std::borrow::Cow<str> = if let Some(rest) = s.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            let home = std::env::var("HOME").unwrap_or_default();
            std::borrow::Cow::Owned(format!("{home}{rest}"))
        } else {
            std::borrow::Cow::Borrowed(s)
        }
    } else {
        std::borrow::Cow::Borrowed(s)
    };
    let s = expanded.as_ref();
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
fn resolve_symbol_text(store: &dyn ValueStore, session_id: &str, name: &str) -> Option<String> {
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
/// `required ++ optional` order (from its [`OpSignature`](crate::opspec::OpSignature)). The AST stays
/// positional — this is the one place positional args become the named object a tool expects.
fn map_args_to_input(
    op: &str,
    args: Vec<serde_json::Value>,
    catalog: &dyn OpCatalog,
) -> Result<serde_json::Value> {
    let sig = catalog
        .lookup(op)
        .ok_or_else(|| Error::Other(format!("unknown op `{op}`")))?;

    // A lone object argument is already the named input map — pass it straight through.
    if let [serde_json::Value::Object(_)] = args.as_slice() {
        return Ok(args.into_iter().next().unwrap());
    }

    let order: Vec<String> = sig
        .required_params
        .into_iter()
        .chain(sig.optional_params)
        .collect();
    let mut input = serde_json::Map::new();
    for (i, val) in args.into_iter().enumerate() {
        match order.get(i) {
            Some(name) => {
                input.insert(name.clone(), val);
            }
            None => {
                return Err(FlowError::Runtime(format!(
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
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    value: &Node,
    sink: &mut dyn FlowSink,
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
                return Err(FlowError::Runtime(format!(
                    "return step `{op}` failed: {}",
                    outcome.content
                )));
            }
            Ok((outcome.content, outcome.value_id))
        }
        other => Err(FlowError::Runtime(format!(
            "unsupported return expression `{}`",
            node_kind(other)
        ))),
    }
}

/// Evaluate a safe arithmetic formula string with named variable bindings.
/// Supported: `+`, `-`, `*`, `/`, `round(x,n)`, `abs(x)`, `min(a,b)`, `max(a,b)`,
/// numeric literals, variable names, and parentheses. No side effects.
fn eval_expr_formula(formula: &str, vars: &std::collections::BTreeMap<String, f64>) -> Result<f64> {
    eval_expr_tokens(&mut tokenize_expr(formula), vars)
        .ok_or_else(|| FlowError::Runtime(format!("invalid `expr` formula: {formula}")))
}

fn tokenize_expr(s: &str) -> std::collections::VecDeque<String> {
    let mut tokens = std::collections::VecDeque::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '0'..='9' | '.' => {
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' {
                        num.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push_back(num);
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let mut ident = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        ident.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push_back(ident);
            }
            c => {
                tokens.push_back(c.to_string());
                chars.next();
            }
        }
    }
    tokens
}

fn eval_expr_tokens(
    tokens: &mut std::collections::VecDeque<String>,
    vars: &std::collections::BTreeMap<String, f64>,
) -> Option<f64> {
    expr_add(tokens, vars)
}

fn expr_add(
    t: &mut std::collections::VecDeque<String>,
    v: &std::collections::BTreeMap<String, f64>,
) -> Option<f64> {
    let mut lhs = expr_mul(t, v)?;
    loop {
        match t.front().map(|s| s.as_str()) {
            Some("+") => {
                t.pop_front();
                lhs += expr_mul(t, v)?;
            }
            Some("-") => {
                t.pop_front();
                lhs -= expr_mul(t, v)?;
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn expr_mul(
    t: &mut std::collections::VecDeque<String>,
    v: &std::collections::BTreeMap<String, f64>,
) -> Option<f64> {
    let mut lhs = expr_unary(t, v)?;
    loop {
        match t.front().map(|s| s.as_str()) {
            Some("*") => {
                t.pop_front();
                lhs *= expr_unary(t, v)?;
            }
            Some("/") => {
                t.pop_front();
                let r = expr_unary(t, v)?;
                if r == 0.0 {
                    return None;
                }
                lhs /= r;
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn expr_unary(
    t: &mut std::collections::VecDeque<String>,
    v: &std::collections::BTreeMap<String, f64>,
) -> Option<f64> {
    if t.front().map(|s| s.as_str()) == Some("-") {
        t.pop_front();
        return Some(-expr_atom(t, v)?);
    }
    expr_atom(t, v)
}

fn expr_atom(
    t: &mut std::collections::VecDeque<String>,
    v: &std::collections::BTreeMap<String, f64>,
) -> Option<f64> {
    let tok = t.pop_front()?;
    match tok.as_str() {
        "(" => {
            let val = expr_add(t, v)?;
            if t.pop_front().as_deref() != Some(")") {
                return None;
            }
            Some(val)
        }
        "round" => {
            if t.pop_front().as_deref() != Some("(") {
                return None;
            }
            let x = expr_add(t, v)?;
            let n = if t.front().map(|s| s.as_str()) == Some(",") {
                t.pop_front();
                expr_add(t, v)?.round() as i32
            } else {
                0
            };
            if t.pop_front().as_deref() != Some(")") {
                return None;
            }
            let factor = 10f64.powi(n);
            Some((x * factor).round() / factor)
        }
        "abs" => {
            if t.pop_front().as_deref() != Some("(") {
                return None;
            }
            let x = expr_add(t, v)?;
            if t.pop_front().as_deref() != Some(")") {
                return None;
            }
            Some(x.abs())
        }
        "min" => {
            if t.pop_front().as_deref() != Some("(") {
                return None;
            }
            let a = expr_add(t, v)?;
            if t.pop_front().as_deref() != Some(",") {
                return None;
            }
            let b = expr_add(t, v)?;
            if t.pop_front().as_deref() != Some(")") {
                return None;
            }
            Some(a.min(b))
        }
        "max" => {
            if t.pop_front().as_deref() != Some("(") {
                return None;
            }
            let a = expr_add(t, v)?;
            if t.pop_front().as_deref() != Some(",") {
                return None;
            }
            let b = expr_add(t, v)?;
            if t.pop_front().as_deref() != Some(")") {
                return None;
            }
            Some(a.max(b))
        }
        s => {
            if let Ok(n) = s.parse::<f64>() {
                return Some(n);
            }
            v.get(s).copied()
        }
    }
}

/// Format a float cleanly: integer results drop the decimal, fractional keep up to 2 places.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        // Up to 2 significant decimal places, strip trailing zeros.
        let s = format!("{:.2}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Walk a dot-path (e.g. `".bitcoin.usd"` or `"results[0].price"`) into a JSON value.
/// Path segments: `.key` (object field), `[n]` (array index). Leading `.` is optional.
fn coerce_parse(value: &serde_json::Value, as_type: &str) -> Result<String> {
    let s = match value {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    match as_type {
        "f64" => {
            let n: f64 = s
                .trim()
                .parse()
                .map_err(|_| FlowError::Runtime(format!("parse: cannot coerce {:?} to f64", s)))?;
            Ok(format_number(n))
        }
        "i64" => {
            let n: i64 = s
                .trim()
                .parse()
                .map_err(|_| FlowError::Runtime(format!("parse: cannot coerce {:?} to i64", s)))?;
            Ok(n.to_string())
        }
        "bool" => {
            let t = s.trim();
            Ok((t == "true" || t == "1").to_string())
        }
        "json" => {
            // validate it parses as JSON, return canonical form
            let v: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| FlowError::Runtime(format!("parse: invalid JSON: {e}")))?;
            Ok(serde_json::to_string(&v).unwrap_or_default())
        }
        _ => Ok(s), // "string" or unknown — pass through
    }
}

fn eval_jq_path(path: &str, value: &serde_json::Value) -> Result<serde_json::Value> {
    let path = path.trim().trim_start_matches('.');
    if path.is_empty() {
        return Ok(value.clone());
    }
    let mut cur = value;
    // Split on `.` and handle `[n]` inside each segment.
    for raw_seg in path.split('.') {
        let seg = raw_seg.trim();
        if seg.is_empty() {
            continue;
        }
        // Segment may be `key[0][1]` — split on `[`.
        let mut parts = seg.splitn(2, '[');
        let key = parts.next().unwrap_or("");
        if !key.is_empty() {
            cur = cur
                .get(key)
                .ok_or_else(|| Error::Other(format!("`jq` path: key `{key}` not found")))?;
        }
        if let Some(rest) = parts.next() {
            // rest is like `0]` or `0][1]`
            let mut bracket = format!("[{rest}");
            while bracket.contains('[') {
                let end = bracket
                    .find(']')
                    .ok_or_else(|| Error::Other("`jq` path: unmatched `[`".to_string()))?;
                let idx_str = bracket[1..end].trim();
                let idx: usize = idx_str
                    .parse()
                    .map_err(|_| Error::Other(format!("`jq` path: invalid index `{idx_str}`")))?;
                cur = cur
                    .get(idx)
                    .ok_or_else(|| Error::Other(format!("`jq` path: index {idx} out of bounds")))?;
                bracket = bracket[end + 1..].to_string();
            }
        }
    }
    Ok(cur.clone())
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

/// The visibility keep-priority: pinned context is retained over plain/hidden when a pack is budgeted.
fn vis_keep_rank(v: Visibility) -> u8 {
    match v {
        Visibility::Pinned => 4,
        Visibility::Visible => 3,
        Visibility::Hidden => 2,
        Visibility::Expired => 1,
        Visibility::Private => 0,
    }
}

/// Resolve a context pack's `members` (already exclude-filtered, in declared order) to a budgeted
/// `Ctx` value bound to `name`. When `budget` is set the pack is shrunk **at evaluation**: members are
/// kept in priority order (visibility tier, then declared order) while their cumulative char size fits
/// the budget; the rest are dropped and recorded as a [`RunEvent::CtxShrunk`]. The interpreter stays
/// op-agnostic — consuming ops just read the already-bounded member list.
fn build_ctx(
    store: &dyn ValueStore,
    session_id: &str,
    name: &SymbolName,
    purpose: &Option<String>,
    members: &[SymbolName],
    budget: Option<u64>,
    transcript: &mut Vec<String>,
) -> Result<ValueId> {
    // Dedup members (first-seen order): a symbol listed twice must not double-charge the budget.
    let members: Vec<SymbolName> = {
        let mut seen = std::collections::HashSet::new();
        members
            .iter()
            .filter(|s| seen.insert(s.0.clone()))
            .cloned()
            .collect()
    };

    // Char size + visibility rank per member. An unbound member contributes nothing (a pack tolerates
    // a not-yet-resolved reference rather than erroring).
    let sizes: Vec<usize> = members
        .iter()
        .map(|sym| -> Result<usize> {
            Ok(match store.resolve(session_id, sym)? {
                Some(vid) => store
                    .get_value(&vid)?
                    .map(|v| v.to_json().to_string().chars().count())
                    .unwrap_or(0),
                None => 0,
            })
        })
        .collect::<Result<_>>()?;
    let ranks: Vec<u8> = members
        .iter()
        .map(|sym| -> Result<u8> {
            Ok(store
                .binding(session_id, sym)?
                .map(|b| vis_keep_rank(b.visibility))
                .unwrap_or(vis_keep_rank(Visibility::Visible)))
        })
        .collect::<Result<_>>()?;

    let mut keep = vec![true; members.len()];
    if let Some(b) = budget {
        // Shrink by visibility tier then declared order, dropping the lowest-priority tail: keep a
        // priority-ordered *prefix* so a pinned member is never dropped to make room for a plainer one
        // (stable sort preserves declared order within a tier; the first member that doesn't fit stops
        // the pack — no rank inversion).
        let mut order: Vec<usize> = (0..members.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(ranks[i]));
        keep = vec![false; members.len()];
        let mut running = 0usize;
        for &i in &order {
            if running + sizes[i] <= b as usize {
                running += sizes[i];
                keep[i] = true;
            } else {
                break;
            }
        }
    }

    // Kept/dropped reported in original declared order.
    let mut kept: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for (i, sym) in members.iter().enumerate() {
        if keep[i] {
            kept.push(sym.0.clone());
        } else {
            dropped.push(sym.0.clone());
        }
    }

    if let Some(b) = budget {
        store.append_event(
            session_id,
            &RunEvent::CtxShrunk {
                ctx: name.0.clone(),
                kept: kept.clone(),
                dropped: dropped.clone(),
                budget: b,
            },
        )?;
        if !dropped.is_empty() {
            transcript.push(format!(
                "[ctx {}] budget {b} chars — kept {:?}, dropped {:?}",
                name.0, kept, dropped
            ));
        }
    }

    let mut fields: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    fields.insert("name".to_string(), Value::String(name.0.clone()));
    if let Some(p) = purpose {
        fields.insert("purpose".to_string(), Value::String(p.clone()));
    }
    fields.insert(
        "members".to_string(),
        Value::List(kept.iter().cloned().map(Value::String).collect()),
    );
    if let Some(b) = budget {
        fields.insert("budget".to_string(), Value::Number(b as f64));
    }
    let vid = store.put_value(session_id, &Value::Struct(fields))?;
    let summary = match budget {
        Some(b) => format!("Ctx({} members, budget {b})", kept.len()),
        None => format!("Ctx({} members)", kept.len()),
    };
    store.bind(
        session_id,
        name,
        &vid,
        Some("Ctx"),
        &summary,
        Visibility::Visible,
    )?;
    Ok(vid)
}

/// Accrete `add` into the existing pack bound to `ctx`, immutably rebinding it to a new `Ctx` value
/// (the prior value stays addressable — the audit chain) with the budget re-applied.
fn append_ctx(
    store: &dyn ValueStore,
    session_id: &str,
    ctx: &SymbolName,
    add: &[SymbolName],
    transcript: &mut Vec<String>,
) -> Result<ValueId> {
    let vid = store.resolve(session_id, ctx)?.ok_or_else(|| {
        crate::FlowError::Runtime(format!("ctx_append: unknown context pack `{}`", ctx.0))
    })?;
    let Some(Value::Struct(fields)) = store.get_value(&vid)? else {
        return Err(crate::FlowError::Runtime(format!(
            "ctx_append: `{}` is not a context pack",
            ctx.0
        )));
    };
    let mut members: Vec<SymbolName> = match fields.get("members") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(SymbolName(s.clone())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let purpose = match fields.get("purpose") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    let budget = match fields.get("budget") {
        Some(Value::Number(n)) => Some(*n as u64),
        _ => None,
    };
    for a in add {
        if !members.contains(a) {
            members.push(a.clone());
        }
    }
    build_ctx(
        store, session_id, ctx, &purpose, &members, budget, transcript,
    )
}

/// The node-kind tag (for error messages).
fn node_kind(node: &Node) -> &'static str {
    match node {
        Node::Call { .. } => "call",
        Node::Bind { .. } => "bind",
        Node::When { .. } => "when",
        Node::Repeat { .. } => "repeat",
        Node::Each { .. } => "each",
        Node::Assert { .. } => "assert",
        Node::Pipe { .. } => "pipe",
        Node::Seq { .. } => "seq",
        Node::Memo { .. } => "memo",
        Node::Parallel { .. } => "parallel",
        Node::Await { .. } => "await",
        Node::Retry { .. } => "retry",
        Node::Try { .. } => "try",
        Node::Confirm { .. } => "confirm",
        Node::Loop { .. } => "loop",
        Node::Race { .. } => "race",
        Node::Throttle { .. } => "throttle",
        Node::Debounce { .. } => "debounce",
        Node::Unless { .. } => "unless",
        Node::Verify { .. } => "verify",
        Node::Peek { .. } => "peek",
        Node::Expr { .. } => "expr",
        Node::Fmt { .. } => "fmt",
        Node::Jq { .. } => "jq",
        Node::Return { .. } => "return",
        Node::Var { .. } => "var",
        Node::Lit { .. } => "lit",
        Node::Thing { .. } => "thing",
        Node::Parse { .. } => "parse",
        Node::Ctx { .. } => "ctx",
        Node::CtxAppend { .. } => "ctx_append",
        Node::Match { .. } => "match",
        Node::Route { .. } => "route",
        Node::Fallback { .. } => "fallback",
        Node::Timeout { .. } => "timeout",
        Node::Budget { .. } => "budget",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::ast::{Node, RunEvent, SymbolName, Value, Visibility};
    use crate::opspec::{OpCatalog, OpSignature};
    use crate::store::MemStore;

    /// A minimal in-memory catalog for arg-mapping tests (no runtime/tools dependency).
    struct MockCatalog(Vec<OpSignature>);
    impl OpCatalog for MockCatalog {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            self.0.iter().find(|s| s.name == name).cloned()
        }
    }
    fn sig(name: &str, required: &[&str], optional: &[&str]) -> OpSignature {
        OpSignature {
            name: name.into(),
            description: String::new(),
            effects: Vec::new(),
            risk: flux_spec::Risk::Low,
            idempotency: flux_spec::Idempotency::Idempotent,
            required_params: required.iter().map(|s| s.to_string()).collect(),
            optional_params: optional.iter().map(|s| s.to_string()).collect(),
            param_types: Default::default(),
        }
    }
    fn catalog() -> MockCatalog {
        MockCatalog(vec![
            sig("read", &["path"], &["offset", "limit"]),
            sig("write", &["path", "content"], &[]),
            sig("edit", &["path", "old_string", "new_string"], &[]),
        ])
    }
    fn flow_lit(v: serde_json::Value) -> Node {
        Node::Lit { value: v }
    }
    fn flow_var(name: &str) -> Node {
        Node::Var {
            name: SymbolName(name.into()),
        }
    }

    /// `ctx` budgets a pack at evaluation: it shrinks by visibility then declared order, keeps pinned
    /// context, records the drop as a `CtxShrunk` event, and `ctx_append` rebinds immutably.
    #[test]
    fn ctx_budgets_by_visibility_and_appends_immutably() {
        let store = MemStore::new();
        let sid = "s";
        let put = |name: &str, val: String, vis: Visibility| {
            let vid = store.put_value(sid, &Value::String(val.clone())).unwrap();
            store
                .bind(sid, &SymbolName(name.into()), &vid, None, &val, vis)
                .unwrap();
        };
        // Each value serializes to ~42 chars ("xxxx…" + quotes); budget 100 fits exactly two.
        put("a", "x".repeat(40), Visibility::Visible);
        put("b", "y".repeat(40), Visibility::Visible);
        put("c", "z".repeat(40), Visibility::Pinned);

        let mut transcript = Vec::new();
        let members = vec![
            SymbolName("a".into()),
            SymbolName("b".into()),
            SymbolName("c".into()),
        ];
        let vid = build_ctx(
            &store,
            sid,
            &SymbolName("pack".into()),
            &Some("debug".into()),
            &members,
            Some(100),
            &mut transcript,
        )
        .unwrap();

        let Some(Value::Struct(fields)) = store.get_value(&vid).unwrap() else {
            panic!("ctx produced a struct value")
        };
        let Value::List(kept_vals) = &fields["members"] else {
            panic!("members is a list")
        };
        let kept: Vec<String> = kept_vals
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(kept.len(), 2, "budget kept exactly two of three members");
        assert!(kept.contains(&"c".to_string()), "pinned member is retained");

        // The shrink is recorded in the run trace with the dropped member.
        let dropped = store
            .events(sid)
            .into_iter()
            .find_map(|e| match e {
                RunEvent::CtxShrunk { dropped, .. } => Some(dropped),
                _ => None,
            })
            .expect("a CtxShrunk event was recorded");
        assert_eq!(
            dropped,
            vec!["b".to_string()],
            "the lowest-priority member dropped"
        );

        // ctx_append accretes a new (small) member and rebinds to a fresh value id (immutable chain).
        put("d", "small".into(), Visibility::Visible);
        let vid2 = append_ctx(
            &store,
            sid,
            &SymbolName("pack".into()),
            &[SymbolName("d".into())],
            &mut transcript,
        )
        .unwrap();
        assert_ne!(vid, vid2, "append rebinds to a new value id");
        let Some(Value::Struct(f2)) = store.get_value(&vid2).unwrap() else {
            panic!("appended ctx is a struct")
        };
        let Value::List(m2) = &f2["members"] else {
            panic!("members list")
        };
        assert!(
            m2.iter().any(|v| matches!(v, Value::String(s) if s == "d")),
            "the appended member is present"
        );
    }

    /// With no budget a pack keeps every member (an unbound one too — tolerated, sized 0) and records
    /// neither a shrink event nor a `budget` field.
    #[test]
    fn ctx_without_budget_keeps_all_including_unbound() {
        let store = MemStore::new();
        let sid = "s";
        let vid = store
            .put_value(sid, &Value::String("hello".into()))
            .unwrap();
        store
            .bind(
                sid,
                &SymbolName("a".into()),
                &vid,
                None,
                "hello",
                Visibility::Visible,
            )
            .unwrap();
        let mut transcript = Vec::new();
        // `b` is never bound — must be tolerated, not error.
        let members = vec![SymbolName("a".into()), SymbolName("b".into())];
        let cvid = build_ctx(
            &store,
            sid,
            &SymbolName("pack".into()),
            &None,
            &members,
            None,
            &mut transcript,
        )
        .unwrap();
        let Some(Value::Struct(fields)) = store.get_value(&cvid).unwrap() else {
            panic!("ctx is a struct")
        };
        let Value::List(kept) = &fields["members"] else {
            panic!("members list")
        };
        assert_eq!(
            kept.len(),
            2,
            "no budget keeps every member, unbound included"
        );
        assert!(!fields.contains_key("budget"), "no budget field when None");
        assert!(
            !store
                .events(sid)
                .iter()
                .any(|e| matches!(e, RunEvent::CtxShrunk { .. })),
            "no shrink event without a budget"
        );
    }

    /// Appending a higher-priority member re-budgets the pack and can evict a previously-kept,
    /// lower-priority member (priority-prefix semantics — no rank inversion).
    #[test]
    fn ctx_append_re_budgets_and_can_evict() {
        let store = MemStore::new();
        let sid = "s";
        let put = |name: &str, val: String, vis: Visibility| {
            let vid = store.put_value(sid, &Value::String(val.clone())).unwrap();
            store
                .bind(sid, &SymbolName(name.into()), &vid, None, &val, vis)
                .unwrap();
        };
        put("keep", "z".repeat(40), Visibility::Pinned); // ~42 chars
        put("low", "y".repeat(40), Visibility::Visible); // ~42 chars
        let mut t = Vec::new();
        let members = vec![SymbolName("keep".into()), SymbolName("low".into())];
        build_ctx(
            &store,
            sid,
            &SymbolName("p".into()),
            &None,
            &members,
            Some(100),
            &mut t,
        )
        .unwrap();

        // A higher-priority (pinned) member that overflows the budget evicts the lower-priority `low`.
        put("big", "q".repeat(50), Visibility::Pinned); // ~52 chars
        let v2 = append_ctx(
            &store,
            sid,
            &SymbolName("p".into()),
            &[SymbolName("big".into())],
            &mut t,
        )
        .unwrap();
        let Some(Value::Struct(f2)) = store.get_value(&v2).unwrap() else {
            panic!("ctx is a struct")
        };
        let Value::List(m2) = &f2["members"] else {
            panic!("members list")
        };
        let kept: Vec<String> = m2
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(kept.contains(&"keep".to_string()), "pinned member retained");
        assert!(
            kept.contains(&"big".to_string()),
            "appended pinned member kept"
        );
        assert!(
            !kept.contains(&"low".to_string()),
            "lower-priority member evicted on re-budget (no rank inversion)"
        );
    }

    /// A `PhysicalPlan` (Parallel independent reads, then a dependent read) executes to the **same**
    /// store state as running the flow linearly through `execute_flow`.
    #[tokio::test]
    async fn execute_plan_matches_execute_flow() {
        use crate::ast::{NodeId, PhysicalPlan, Stage};

        struct EchoCat;
        impl OpCatalog for EchoCat {
            fn lookup(&self, name: &str) -> Option<OpSignature> {
                (name == "read").then(|| OpSignature {
                    name: "read".into(),
                    description: String::new(),
                    effects: vec![flux_spec::Effect::Read],
                    risk: flux_spec::Risk::Low,
                    idempotency: flux_spec::Idempotency::Idempotent,
                    required_params: vec!["path".into()],
                    optional_params: Vec::new(),
                    param_types: Default::default(),
                })
            }
        }
        struct EchoHost(EchoCat);
        #[async_trait::async_trait]
        impl OpHost for EchoHost {
            async fn dispatch(&self, op: &str, input: serde_json::Value) -> OpOutcome {
                OpOutcome::ok(format!("{op}({input})"))
            }
            fn catalog(&self) -> &dyn OpCatalog {
                &self.0
            }
            async fn request_approval(
                &self,
                _label: &str,
                _intents: &flux_spec::IntentSet,
            ) -> ApprovalChoice {
                ApprovalChoice::Allow
            }
            fn trim_output(&self, view: String, _op: &str) -> String {
                view
            }
        }

        let bind_read = |name: &str, arg: Node| Node::Bind {
            name: SymbolName(name.into()),
            value: Box::new(Node::Call {
                op: "read".into(),
                args: vec![arg],
            }),
            ty: None,
            effect: None,
        };
        // $a = read "x"; $b = read "y" (independent); $c = read $a (depends on a).
        let body = vec![
            bind_read("a", flow_lit(json!("x"))),
            bind_read("b", flow_lit(json!("y"))),
            bind_read("c", flow_var("a")),
        ];
        let plan = PhysicalPlan {
            stages: vec![
                Stage::Parallel(vec![NodeId(0), NodeId(1)]),
                Stage::Sequential(NodeId(2)),
            ],
        };

        let host = EchoHost(EchoCat);

        let store_plan = MemStore::new();
        let mut sink = BufferSink::default();
        execute_plan(&store_plan, &host, "s", &body, &plan, &mut sink)
            .await
            .unwrap();

        let store_flow = MemStore::new();
        let ast = DraftAst {
            body: body.clone(),
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        execute_flow(&store_flow, &host, "s", &ast, &mut sink2)
            .await
            .unwrap();

        for sym in ["a", "b", "c"] {
            let vp = store_plan
                .resolve("s", &SymbolName(sym.into()))
                .unwrap()
                .and_then(|id| store_plan.get_value(&id).unwrap());
            let vf = store_flow
                .resolve("s", &SymbolName(sym.into()))
                .unwrap()
                .and_then(|id| store_flow.get_value(&id).unwrap());
            assert!(vp.is_some(), "symbol ${sym} should be bound by the plan");
            assert_eq!(vp, vf, "symbol ${sym} differs: plan vs linear execution");
        }
    }

    // ---- P6b: Tier-1 control-flow primitives (match/route/fallback/timeout/budget) ----

    use crate::ast::{FallbackBranch, MatchCase, RouteCase};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct CfCat;
    impl OpCatalog for CfCat {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            let mk = |req: &[&str]| {
                Some(OpSignature {
                    name: name.into(),
                    description: String::new(),
                    effects: vec![flux_spec::Effect::Read],
                    risk: flux_spec::Risk::Low,
                    idempotency: flux_spec::Idempotency::Idempotent,
                    required_params: req.iter().map(|s| s.to_string()).collect(),
                    optional_params: Vec::new(),
                    param_types: Default::default(),
                })
            };
            match name {
                "pick" => mk(&["label"]),
                "echo" => mk(&["v"]),
                "boom" | "slow" => mk(&[]),
                _ => None,
            }
        }
    }

    /// A host that records the (op, first-arg) of each dispatch so a test can assert *which* branch
    /// ran. `pick` echoes its `label` (a `route` selector), `echo` echoes its `v`, `boom` errors, and
    /// `slow` sleeps before succeeding (to exercise `timeout`).
    struct CfHost {
        cat: CfCat,
        log: Mutex<Vec<String>>,
        calls: AtomicUsize,
    }
    impl CfHost {
        fn new() -> Self {
            CfHost {
                cat: CfCat,
                log: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            }
        }
        fn marks(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }
    #[async_trait::async_trait]
    impl OpHost for CfHost {
        async fn dispatch(&self, op: &str, input: serde_json::Value) -> OpOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let arg = |k: &str| {
                input
                    .get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let (mark, out) = match op {
                "pick" => (
                    format!("pick={}", arg("label")),
                    OpOutcome::ok(arg("label")),
                ),
                "echo" => (arg("v"), OpOutcome::ok(arg("v"))),
                "boom" => ("boom".to_string(), OpOutcome::error("boom failed")),
                "slow" => {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    ("slow".to_string(), OpOutcome::ok("slow done"))
                }
                other => (other.to_string(), OpOutcome::ok(other.to_string())),
            };
            self.log.lock().unwrap().push(mark);
            out
        }
        fn catalog(&self) -> &dyn OpCatalog {
            &self.cat
        }
        async fn request_approval(
            &self,
            _label: &str,
            _intents: &flux_spec::IntentSet,
        ) -> ApprovalChoice {
            ApprovalChoice::Allow
        }
        fn trim_output(&self, view: String, _op: &str) -> String {
            view
        }
    }

    fn call(op: &str, args: Vec<Node>) -> Node {
        Node::Call {
            op: op.into(),
            args,
        }
    }
    fn echo(v: &str) -> Node {
        call("echo", vec![flow_lit(json!(v))])
    }
    async fn run(host: &CfHost, body: Vec<Node>) -> Result<FlowOutcome> {
        let store = MemStore::new();
        let ast = DraftAst {
            body,
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, host, "s", &ast, &mut sink).await
    }

    #[tokio::test]
    async fn match_runs_first_equal_case_then_default_then_errors() {
        // subject "b" runs case "b".
        let host = CfHost::new();
        let body = vec![Node::Match {
            subject: Box::new(flow_lit(json!("b"))),
            cases: vec![
                MatchCase {
                    value: flow_lit(json!("a")),
                    body: vec![echo("A")],
                },
                MatchCase {
                    value: flow_lit(json!("b")),
                    body: vec![echo("B")],
                },
            ],
            default: vec![echo("D")],
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["B"], "the equal case runs, nothing else");

        // unmatched subject falls to default.
        let host = CfHost::new();
        let body = vec![Node::Match {
            subject: Box::new(flow_lit(json!("z"))),
            cases: vec![MatchCase {
                value: flow_lit(json!("a")),
                body: vec![echo("A")],
            }],
            default: vec![echo("D")],
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["D"], "no case matched → default");

        // unmatched with no default is an error (the exhaustiveness guard-rail).
        let host = CfHost::new();
        let body = vec![Node::Match {
            subject: Box::new(flow_lit(json!("z"))),
            cases: vec![MatchCase {
                value: flow_lit(json!("a")),
                body: vec![echo("A")],
            }],
            default: vec![],
        }];
        assert!(run(&host, body).await.is_err());
        assert!(
            host.marks().is_empty(),
            "no branch ran on an unmatched match"
        );
    }

    #[tokio::test]
    async fn route_selector_label_picks_the_declared_case() {
        // selector pick("b") → "b" → case "b" runs.
        let host = CfHost::new();
        let body = vec![Node::Route {
            selector: Box::new(call("pick", vec![flow_lit(json!("b"))])),
            cases: vec![
                RouteCase {
                    label: "a".into(),
                    body: vec![echo("A")],
                },
                RouteCase {
                    label: "b".into(),
                    body: vec![echo("B")],
                },
            ],
            default: vec![],
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["pick=b", "B"]);

        // a label matching no case with no default is an error.
        let host = CfHost::new();
        let body = vec![Node::Route {
            selector: Box::new(call("pick", vec![flow_lit(json!("zzz"))])),
            cases: vec![RouteCase {
                label: "a".into(),
                body: vec![echo("A")],
            }],
            default: vec![],
        }];
        assert!(run(&host, body).await.is_err());
        assert_eq!(host.marks(), vec!["pick=zzz"], "no case body ran");
    }

    #[tokio::test]
    async fn fallback_takes_the_first_success_else_propagates() {
        // first branch errors (boom), second succeeds (echo) → second wins.
        let host = CfHost::new();
        let body = vec![Node::Fallback {
            branches: vec![
                FallbackBranch {
                    body: vec![call("boom", vec![])],
                },
                FallbackBranch {
                    body: vec![echo("ok")],
                },
            ],
            bind: Some(SymbolName("out".into())),
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["boom", "ok"]);

        // every branch errors → the last error propagates.
        let host = CfHost::new();
        let body = vec![Node::Fallback {
            branches: vec![
                FallbackBranch {
                    body: vec![call("boom", vec![])],
                },
                FallbackBranch {
                    body: vec![call("boom", vec![])],
                },
            ],
            bind: None,
        }];
        assert!(run(&host, body).await.is_err());
        assert_eq!(host.marks(), vec!["boom", "boom"]);
    }

    #[tokio::test]
    async fn timeout_bounds_the_wall_clock() {
        // fast body finishes inside the deadline; its dispatch is threaded into the real step count
        // and transcript (not a discarded local) so an enclosing `budget`/audit sees the work.
        let host = CfHost::new();
        let body = vec![Node::Timeout {
            ms: 1000,
            body: vec![echo("a"), echo("b")],
            bind: None,
        }];
        let out = run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["a", "b"]);
        assert_eq!(out.steps, 2, "timeout body's dispatches count toward steps");
        assert!(
            out.transcript.contains("echo"),
            "body work is in the transcript"
        );

        // a body slower than the deadline errors.
        let host = CfHost::new();
        let body = vec![Node::Timeout {
            ms: 20,
            body: vec![call("slow", vec![])],
            bind: None,
        }];
        assert!(run(&host, body).await.is_err());
    }

    #[tokio::test]
    async fn budget_caps_dispatches_at_statement_boundaries() {
        // limit 5 comfortably fits two dispatches.
        let host = CfHost::new();
        let body = vec![Node::Budget {
            limit: 5,
            body: vec![echo("one"), echo("two")],
            bind: None,
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["one", "two"]);

        // limit 1 allows the first statement, then rejects the second before it dispatches.
        let host = CfHost::new();
        let body = vec![Node::Budget {
            limit: 1,
            body: vec![echo("one"), echo("two")],
            bind: None,
        }];
        assert!(run(&host, body).await.is_err());
        assert_eq!(
            host.marks(),
            vec!["one"],
            "the over-budget statement never ran"
        );
    }

    // ---- P6a: await cross-turn suspend/resume ----

    fn await_node(binding: Option<&str>, source: &str, as_type: Option<TypeRef>) -> Node {
        Node::Await {
            binding: binding.map(|b| SymbolName(b.into())),
            source: source.into(),
            as_type,
        }
    }

    #[tokio::test]
    async fn await_suspends_then_resumes_without_rerunning_the_prefix() {
        let host = CfHost::new();
        let store = MemStore::new();
        let body = vec![
            echo("a"),
            await_node(Some("reply"), "user_input", None),
            echo("b"),
        ];
        let ast = DraftAst {
            body: body.clone(),
            ..Default::default()
        };

        // First turn: the prefix runs, then the flow suspends at the await — nothing past it runs.
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(host.marks(), vec!["a"], "only the prefix ran");
        let susp = out.suspension.expect("flow suspended on the await");
        assert_eq!(susp.node, crate::ast::NodeId(1));
        assert_eq!(susp.source, "user_input");
        assert!(out.returned.is_none());
        assert!(
            store
                .events("s")
                .iter()
                .any(|e| matches!(e, RunEvent::Awaiting { .. })),
            "an Awaiting event was recorded"
        );

        // Second turn: resume with the reply. The prefix is NOT re-run, the awaited value is bound,
        // and execution continues from the next statement to completion.
        let mut sink2 = BufferSink::default();
        let out2 = resume_flow(
            &store,
            &host,
            "s",
            &body,
            susp.node,
            Value::String("hi".into()),
            &mut sink2,
        )
        .await
        .unwrap();
        assert!(out2.suspension.is_none(), "the resumed flow completed");
        assert_eq!(
            host.marks(),
            vec!["a", "b"],
            "prefix not re-run; only echo b added"
        );
        let reply = store
            .resolve("s", &SymbolName("reply".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap());
        assert_eq!(
            reply,
            Some(Value::String("hi".into())),
            "awaited value bound"
        );
    }

    #[tokio::test]
    async fn resume_coerces_input_to_the_await_type() {
        let host = CfHost::new();
        let store = MemStore::new();
        let body = vec![await_node(Some("n"), "num", Some(TypeRef::Number))];
        let ast = DraftAst {
            body: body.clone(),
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let susp = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap()
            .suspension
            .unwrap();

        let mut sink2 = BufferSink::default();
        resume_flow(
            &store,
            &host,
            "s",
            &body,
            susp.node,
            Value::String("42".into()),
            &mut sink2,
        )
        .await
        .unwrap();
        let n = store
            .resolve("s", &SymbolName("n".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap());
        assert_eq!(n, Some(Value::Number(42.0)), "a numeric reply is coerced");
    }

    #[test]
    fn map_args_maps_positional_to_required_param_names() {
        let ops = catalog();

        // read("README.md") → {"path": "README.md"}
        let input = map_args_to_input("read", vec![json!("README.md")], &ops).unwrap();
        assert_eq!(input, json!({ "path": "README.md" }));

        // write("out.txt", "hi") → required order {path, content}
        let input = map_args_to_input("write", vec![json!("out.txt"), json!("hi")], &ops).unwrap();
        assert_eq!(input, json!({ "path": "out.txt", "content": "hi" }));

        // edit(path, old, new) → all three required params, in order
        let input =
            map_args_to_input("edit", vec![json!("f"), json!("a"), json!("b")], &ops).unwrap();
        assert_eq!(
            input,
            json!({ "path": "f", "old_string": "a", "new_string": "b" })
        );

        // A lone object argument passes straight through as the named input.
        let input =
            map_args_to_input("write", vec![json!({ "path": "x", "content": "y" })], &ops).unwrap();
        assert_eq!(input, json!({ "path": "x", "content": "y" }));

        // More args than the op has params (read takes 3: path, offset, limit) is a clear error.
        assert!(map_args_to_input(
            "read",
            vec![json!("a"), json!("b"), json!("c"), json!("d")],
            &ops
        )
        .is_err());
    }

    #[test]
    fn map_args_binds_through_a_lowered_opspec_schema() {
        use crate::ast::TypeRef;
        use crate::opspec::{OpSpec, Param};

        // End-to-end P0 path: a typed, *named* OpSpec lowers to a real input schema, and an
        // OpSignature derived from that lowered ToolSpec recovers the param names — so positional
        // call args bind to the right slots without any hand-written signature.
        let req = |name: &str| Param {
            name: name.into(),
            ty: TypeRef::String,
            optional: false,
        };
        let spec = OpSpec {
            name: "edit".into(),
            description: "edit a file".into(),
            inputs: vec![req("path"), req("old"), req("new")],
            output: TypeRef::Any,
            effects: Vec::new(),
            risk: flux_spec::Risk::Low,
            idempotency: flux_spec::Idempotency::Idempotent,
        };
        let ops = MockCatalog(vec![OpSignature::from_spec(&spec.lower())]);

        let input =
            map_args_to_input("edit", vec![json!("f"), json!("a"), json!("b")], &ops).unwrap();
        assert_eq!(input, json!({ "path": "f", "old": "a", "new": "b" }));
    }

    #[test]
    fn eval_arg_resolves_a_var_to_its_stored_value() {
        let store = MemStore::new();
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
        let store = MemStore::new();
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
        // An unbound token (either style, or unrelated `{…}` text) is left verbatim.
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
}

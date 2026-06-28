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

/// The durable identity of a flow for `checkpoint` scoping: its declared name if any, else a stable
/// content hash of its top-level body. Scopes a flow's durable resume state to the same logical flow
/// across re-runs without colliding with a different flow in the same session.
fn flow_key(name: Option<&str>, body: &[Node]) -> String {
    match name {
        Some(n) if !n.trim().is_empty() => n.to_string(),
        _ => format!(
            "h:{}",
            &sha256_hex(&serde_json::to_string(body).unwrap_or_default())[..16]
        ),
    }
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
    let fk = flow_key(ast.name.as_deref(), &ast.body);
    run_top_level(store, executor, session_id, &ast.body, 0, None, &fk, sink).await
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
    let fk = flow_key(None, body);
    run_top_level(
        store,
        executor,
        session_id,
        body,
        at.0 as usize,
        Some(input),
        &fk,
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
#[allow(clippy::too_many_arguments)]
async fn run_top_level(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    body: &[Node],
    start: usize,
    resume: Option<Value>,
    flow_key: &str,
    sink: &mut dyn FlowSink,
) -> Result<FlowOutcome> {
    let mut steps = 0usize;
    let mut transcript: Vec<String> = Vec::new();
    let mut last = String::new();
    let mut i = start;
    let is_fresh = resume.is_none();

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

    // Fresh run: if a prior run checkpointed this flow in this session, fast-forward past the
    // completed prefix. The prefix's symbols are already durably bound and its side effects are not
    // repeated — the durable resume point. (A resume from `await` keeps its own cursor.)
    if is_fresh {
        if let Some(d) = store.as_durable() {
            if let Some(cp) = d.checkpoint_resume(session_id, flow_key)? {
                i = i.max((cp.0 as usize + 1).min(body.len()));
            }
        }
    }

    while i < body.len() {
        if let Node::Checkpoint { label } = &body[i] {
            // Record the durable resume cursor, then continue past it.
            if let Some(d) = store.as_durable() {
                d.checkpoint_record(session_id, flow_key, label, crate::ast::NodeId(i as u32))?;
            }
            transcript.push(format!("[checkpoint {label}]"));
            i += 1;
            continue;
        }
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
        // A `return` exits the flow immediately. An explicit `return <expr>` yields that
        // expression's value — even when it renders empty (e.g. the agent loop's `$answer`, which
        // is "" once the loop exhausts its iteration budget). A bare `return` carries no value, so
        // fall back to the last non-empty expression value. (Previously the empty explicit return
        // was discarded and the stale `last` leaked out as the result — surfacing loop-machinery
        // text like `observed \`turn.iteration\`` as the turn's answer.) Checked BEFORE the
        // `last = blast` update below so `blast` is still available here.
        if let Step::Return(vid) = step {
            if let Some(v) = &vid {
                store.append_event(session_id, &RunEvent::FlowReturned { value: v.clone() })?;
            }
            let result = if vid.is_some() { blast } else { last };
            return Ok(FlowOutcome {
                returned: vid,
                result,
                transcript: transcript.join("\n\n"),
                steps,
                suspension: None,
            });
        }
        if !blast.is_empty() {
            last = blast;
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
            Stage::Alias { target, source } => {
                // CSE: the optimizer proved this node's op+args are identical to `source`'s, so reuse
                // that value instead of dispatching. No `steps += 1` — the saved dispatch is the point.
                let vid = store.resolve(session_id, source)?.ok_or_else(|| {
                    FlowError::Runtime(format!("CSE alias source `{}` is unbound", source.0))
                })?;
                bind_existing(store, session_id, target, &vid)?;
                last = store
                    .get_value(&vid)?
                    .map(|v| value_text(&v))
                    .unwrap_or_default();
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
                            let resolved = resolve_expr_vars(vars, store, session_id)?;
                            let text = eval_expr_value(formula, &resolved)?.as_text();
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
                            // Op results are stored as JSON *strings* (the canonical `content`), so a
                            // string input that is really JSON is parsed first — this is what lets a flow
                            // pull `.kind`/`.transcript` out of a `plan`/`run_plan` result.
                            let jv = jq_parse_input(eval_arg(input, store, session_id)?);
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
                        Node::Thing { thing } => {
                            // Resolve the external reference to an exact identity through the host's
                            // resolver (the deterministic default handles self-identifying selectors),
                            // record it in the run trace, and bind the resolved `Thing` value.
                            let resolved = executor
                                .resolve_thing(thing)
                                .await
                                .map_err(crate::FlowError::Runtime)?;
                            store.append_event(
                                session_id,
                                &RunEvent::ThingResolved {
                                    thing: thing.clone(),
                                    resolved: resolved.clone(),
                                },
                            )?;
                            let display = resolved.display.clone();
                            let vid = store.put_value(session_id, &Value::Thing(resolved))?;
                            let ty_label = ty.as_ref().map(TypeRef::label);
                            store.bind(
                                session_id,
                                name,
                                &vid,
                                ty_label.as_deref(),
                                &summarize(&display),
                                Visibility::Visible,
                            )?;
                            transcript.push(format!("[${} = thing]\n{display}", name.0));
                            last = display;
                            last_value = Some(vid);
                            continue;
                        }
                        _ => {}
                    }
                    let Node::Call { op, args } = value.as_ref() else {
                        return Err(crate::FlowError::Runtime(
                            "execution can only bind the result of a `call`, `expr`, `fmt`, `jq`, `parse`, or a `thing` reference".to_string(),
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
                    // Pure computation — no IO, no approval gate.
                    let resolved = resolve_expr_vars(vars, store, session_id)?;
                    let text = eval_expr_value(formula, &resolved)?.as_text();
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
                Node::Scope {
                    acquire,
                    bind,
                    body: sbody,
                    finally,
                } => {
                    // RAII: acquire (bind the resource) → run body → ALWAYS run finally → propagate.
                    // If `acquire` fails the resource was never taken, so `finally` does not run (the
                    // `?` propagates before we reach the body/finally).
                    if let Some(acq) = acquire {
                        let (_, avid, astep) = exec_body(
                            store,
                            executor,
                            session_id,
                            std::slice::from_ref(acq.as_ref()),
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await?;
                        if let Step::Return(v) = astep {
                            return Ok((last, v.clone(), Step::Return(v)));
                        }
                        if let (Some(name), Some(vid)) = (bind, &avid) {
                            bind_existing(store, session_id, name, vid)?;
                        }
                    }
                    // Run the body, capturing its outcome so `finally` runs no matter what.
                    let body_res = exec_body(
                        store,
                        executor,
                        session_id,
                        sbody,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await;
                    // Guaranteed cleanup: `finally` always runs — on success, `return`, or error.
                    let fin_res = exec_body(
                        store,
                        executor,
                        session_id,
                        finally,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await;
                    match body_res {
                        Ok((blast, bvid, step)) => {
                            // Body succeeded: a cleanup failure is now the operative error.
                            fin_res?;
                            if !blast.is_empty() {
                                last = blast;
                            }
                            last_value = bvid;
                            if let Step::Return(v) = step {
                                return Ok((last, v.clone(), Step::Return(v)));
                            }
                        }
                        // Body failed: cleanup already ran (best-effort); the body error is primary.
                        Err(be) => return Err(be),
                    }
                }
                Node::Saga { steps: ssteps } => {
                    // Run each step in order; after a step succeeds, register its `undo`. If a later
                    // step fails, run the registered undos in reverse (LIFO, best-effort) then propagate.
                    let mut comps: Vec<&[Node]> = Vec::new();
                    let mut saga_last = String::new();
                    let mut saga_vid: Option<ValueId> = None;
                    let mut failure: Option<FlowError> = None;
                    for step in ssteps {
                        match exec_body(
                            store,
                            executor,
                            session_id,
                            &step.body,
                            &mut *sink,
                            &mut *steps,
                            &mut *transcript,
                        )
                        .await
                        {
                            Ok((blast, bvid, stp)) => {
                                if !blast.is_empty() {
                                    saga_last = blast;
                                }
                                if bvid.is_some() {
                                    saga_vid = bvid;
                                }
                                // A `return` in a step is a successful early exit — no compensation.
                                if let Step::Return(v) = stp {
                                    return Ok((saga_last, v.clone(), Step::Return(v)));
                                }
                                if !step.undo.is_empty() {
                                    comps.push(&step.undo);
                                }
                            }
                            Err(be) => {
                                failure = Some(be);
                                break;
                            }
                        }
                    }
                    if let Some(be) = failure {
                        // Unwind: compensate completed steps in reverse order, best-effort.
                        for undo in comps.iter().rev() {
                            if let Err(ue) = exec_body(
                                store,
                                executor,
                                session_id,
                                undo,
                                &mut *sink,
                                &mut *steps,
                                &mut *transcript,
                            )
                            .await
                            {
                                transcript.push(format!("[saga compensation failed: {ue}]"));
                            }
                        }
                        return Err(be);
                    }
                    if !saga_last.is_empty() {
                        last = saga_last;
                    }
                    last_value = saga_vid;
                }
                Node::Once {
                    label,
                    body: obody,
                    bind,
                } => {
                    // Effect-level memo: if a prior run in this session recorded this label's success,
                    // skip the side effect and reuse the stored value. Keyed on (session, label).
                    if let Some(d) = store.as_durable() {
                        if let Some(rec) = d.once_lookup(session_id, label)? {
                            if let (Some(name), Some(vid)) = (bind, &rec.value) {
                                bind_existing(store, session_id, name, vid)?;
                            }
                            transcript.push(format!("[once {label} (skipped, already done)]"));
                            if !rec.summary.is_empty() {
                                last = rec.summary;
                            }
                            last_value = rec.value;
                            continue;
                        }
                    }
                    // First run (or no durable store): run the body. An error propagates via `?`
                    // *before* we record, so a failed `once` leaves no record and is retried.
                    let (blast, bvid, step) = exec_body(
                        store,
                        executor,
                        session_id,
                        obody,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await?;
                    if let (Some(name), Some(vid)) = (bind, &bvid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                    if let Some(d) = store.as_durable() {
                        d.once_complete(session_id, label, bvid.as_ref(), &blast)?;
                    }
                    if !blast.is_empty() {
                        last = blast;
                    }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
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
                Node::Checkpoint { .. } => {
                    // Like `await`, a checkpoint is intercepted at the top level by `run_top_level`;
                    // reaching it here means it was nested (the analyzer rejects that) or run via the
                    // optimized plan path, neither of which has a stable resume cursor in v1.
                    return Err(FlowError::Runtime(
                        "`checkpoint` must be a top-level flow statement (it is a durable resume cursor; it cannot be nested or run in the optimized plan path)"
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
        // A pure `expr` predicate (`x == 2`, `len(s) > 0 && done`) evaluates with no IO — this is what
        // lets `when`/`unless`/`until`/`assert` express boolean logic without shelling out to bash.
        Node::Expr { formula, vars } => {
            let resolved = resolve_expr_vars(vars, store, session_id)?;
            Ok(eval_expr_value(formula, &resolved)?.truthy())
        }
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

/// A value produced by the `expr` evaluator: number, string, or bool. Arithmetic, comparison,
/// boolean, and string functions all flow through this typed value, with lenient numeric coercion
/// for backward compatibility (a numeric string participates in arithmetic as the number it spells).
#[derive(Clone, Debug, PartialEq)]
enum ExprVal {
    Num(f64),
    Str(String),
    Bool(bool),
}

impl ExprVal {
    /// Coerce to a number where it makes sense: bools are 0/1, numeric strings parse. Returns `None`
    /// for non-numeric strings, so arithmetic on them is a clean error rather than a silent 0.
    fn as_num(&self) -> Option<f64> {
        match self {
            ExprVal::Num(n) => Some(*n),
            ExprVal::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            ExprVal::Str(s) => s.trim().parse::<f64>().ok(),
        }
    }

    /// Render to canonical text — the form stored and printed (numbers via [`format_number`]).
    fn as_text(&self) -> String {
        match self {
            ExprVal::Num(n) => format_number(*n),
            ExprVal::Str(s) => s.clone(),
            ExprVal::Bool(b) => b.to_string(),
        }
    }

    /// Truthiness, matching [`json_truthy`] for strings so `expr` conditions read consistently.
    fn truthy(&self) -> bool {
        match self {
            ExprVal::Num(n) => *n != 0.0,
            ExprVal::Bool(b) => *b,
            ExprVal::Str(s) => {
                let t = s.trim();
                !t.is_empty() && !t.eq_ignore_ascii_case("false") && t != "0"
            }
        }
    }

    fn from_json(v: &serde_json::Value) -> ExprVal {
        match v {
            serde_json::Value::Number(n) => ExprVal::Num(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::Bool(b) => ExprVal::Bool(*b),
            serde_json::Value::String(s) => ExprVal::Str(s.clone()),
            serde_json::Value::Null => ExprVal::Str(String::new()),
            other => ExprVal::Str(other.to_string()),
        }
    }
}

/// Resolve an `expr` node's `vars` map (only `Lit`/`Var` nodes) to typed [`ExprVal`]s.
fn resolve_expr_vars(
    vars: &std::collections::BTreeMap<String, Box<Node>>,
    store: &dyn ValueStore,
    session_id: &str,
) -> Result<std::collections::BTreeMap<String, ExprVal>> {
    vars.iter()
        .map(|(k, v)| {
            Ok((
                k.clone(),
                ExprVal::from_json(&eval_arg(v, store, session_id)?),
            ))
        })
        .collect()
}

/// Evaluate a safe `expr` formula to a typed value. Supports arithmetic (`+ - * /`, with
/// `round(x,n)`/`abs(x)`/`min(a,b)`/`max(a,b)`), comparison (`== != < <= > >=`), boolean
/// (`&& || !`, `true`/`false`), string functions (`len/lower/upper/trim/replace/repeat/reverse/
/// contains/concat`), single/double-quoted string literals, parentheses, and named variables.
/// `+` adds when both sides are numeric and concatenates otherwise. No side effects.
fn eval_expr_value(
    formula: &str,
    vars: &std::collections::BTreeMap<String, ExprVal>,
) -> Result<ExprVal> {
    let mut toks = tokenize_expr(formula);
    match expr_or(&mut toks, vars) {
        Some(v) if toks.is_empty() => Ok(v),
        _ => Err(FlowError::Runtime(format!(
            "invalid `expr` formula: {formula}"
        ))),
    }
}

/// A lexical token of an `expr` formula.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(String),
}

fn tokenize_expr(s: &str) -> std::collections::VecDeque<Tok> {
    let mut tokens = std::collections::VecDeque::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
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
                match num.parse::<f64>() {
                    Ok(n) => tokens.push_back(Tok::Num(n)),
                    Err(_) => tokens.push_back(Tok::Op(num)), // malformed → fails to parse later
                }
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
                tokens.push_back(Tok::Ident(ident));
            }
            '\'' | '"' => {
                let quote = c;
                chars.next();
                let mut lit = String::new();
                while let Some(d) = chars.next() {
                    if d == quote {
                        break;
                    }
                    if d == '\\' {
                        if let Some(e) = chars.next() {
                            lit.push(match e {
                                'n' => '\n',
                                't' => '\t',
                                other => other,
                            });
                        }
                    } else {
                        lit.push(d);
                    }
                }
                tokens.push_back(Tok::Str(lit));
            }
            '=' | '!' | '<' | '>' | '&' | '|' => {
                chars.next();
                let mut op = c.to_string();
                if let Some(&d) = chars.peek() {
                    let two = matches!(
                        (c, d),
                        ('=', '=') | ('!', '=') | ('<', '=') | ('>', '=') | ('&', '&') | ('|', '|')
                    );
                    if two {
                        op.push(d);
                        chars.next();
                    }
                }
                tokens.push_back(Tok::Op(op));
            }
            _ => {
                tokens.push_back(Tok::Op(c.to_string()));
                chars.next();
            }
        }
    }
    tokens
}

fn peek_op(t: &std::collections::VecDeque<Tok>) -> Option<&str> {
    match t.front() {
        Some(Tok::Op(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn expr_or(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<ExprVal> {
    let mut lhs = expr_and(t, v)?;
    while peek_op(t) == Some("||") {
        t.pop_front();
        let rhs = expr_and(t, v)?;
        lhs = ExprVal::Bool(lhs.truthy() || rhs.truthy());
    }
    Some(lhs)
}

fn expr_and(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<ExprVal> {
    let mut lhs = expr_cmp(t, v)?;
    while peek_op(t) == Some("&&") {
        t.pop_front();
        let rhs = expr_cmp(t, v)?;
        lhs = ExprVal::Bool(lhs.truthy() && rhs.truthy());
    }
    Some(lhs)
}

fn expr_cmp(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<ExprVal> {
    let lhs = expr_add(t, v)?;
    if let Some(op) = peek_op(t) {
        if matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=") {
            let op = op.to_string();
            t.pop_front();
            let rhs = expr_add(t, v)?;
            return Some(ExprVal::Bool(expr_compare(&lhs, &rhs, &op)?));
        }
    }
    Some(lhs)
}

/// Compare two values: numerically when both coerce to numbers, else lexicographically by text.
fn expr_compare(a: &ExprVal, b: &ExprVal, op: &str) -> Option<bool> {
    let res = match (a.as_num(), b.as_num()) {
        (Some(x), Some(y)) => match op {
            "==" => x == y,
            "!=" => x != y,
            "<" => x < y,
            "<=" => x <= y,
            ">" => x > y,
            ">=" => x >= y,
            _ => return None,
        },
        _ => {
            let (x, y) = (a.as_text(), b.as_text());
            match op {
                "==" => x == y,
                "!=" => x != y,
                "<" => x < y,
                "<=" => x <= y,
                ">" => x > y,
                ">=" => x >= y,
                _ => return None,
            }
        }
    };
    Some(res)
}

fn expr_add(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<ExprVal> {
    let mut lhs = expr_mul(t, v)?;
    loop {
        match peek_op(t) {
            Some("+") => {
                t.pop_front();
                let rhs = expr_mul(t, v)?;
                lhs = match (lhs.as_num(), rhs.as_num()) {
                    (Some(x), Some(y)) => ExprVal::Num(x + y),
                    _ => ExprVal::Str(format!("{}{}", lhs.as_text(), rhs.as_text())),
                };
            }
            Some("-") => {
                t.pop_front();
                let rhs = expr_mul(t, v)?;
                lhs = ExprVal::Num(lhs.as_num()? - rhs.as_num()?);
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn expr_mul(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<ExprVal> {
    let mut lhs = expr_unary(t, v)?;
    loop {
        match peek_op(t) {
            Some("*") => {
                t.pop_front();
                let r = expr_unary(t, v)?;
                lhs = ExprVal::Num(lhs.as_num()? * r.as_num()?);
            }
            Some("/") => {
                t.pop_front();
                let r = expr_unary(t, v)?.as_num()?;
                if r == 0.0 {
                    return None;
                }
                lhs = ExprVal::Num(lhs.as_num()? / r);
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn expr_unary(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<ExprVal> {
    match peek_op(t) {
        Some("-") => {
            t.pop_front();
            Some(ExprVal::Num(-expr_unary(t, v)?.as_num()?))
        }
        Some("!") => {
            t.pop_front();
            Some(ExprVal::Bool(!expr_unary(t, v)?.truthy()))
        }
        _ => expr_atom(t, v),
    }
}

fn expr_atom(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<ExprVal> {
    match t.pop_front()? {
        Tok::Num(n) => Some(ExprVal::Num(n)),
        Tok::Str(s) => Some(ExprVal::Str(s)),
        Tok::Op(op) if op == "(" => {
            let val = expr_or(t, v)?;
            match t.pop_front() {
                Some(Tok::Op(ref c)) if c == ")" => Some(val),
                _ => None,
            }
        }
        Tok::Op(_) => None,
        Tok::Ident(name) => {
            if matches!(t.front(), Some(Tok::Op(s)) if s == "(") {
                t.pop_front(); // consume "("
                let args = expr_call_args(t, v)?;
                expr_call_fn(&name, &args)
            } else {
                match name.as_str() {
                    "true" => Some(ExprVal::Bool(true)),
                    "false" => Some(ExprVal::Bool(false)),
                    _ => v.get(&name).cloned(),
                }
            }
        }
    }
}

/// Parse a comma-separated argument list up to and including the closing `)`.
fn expr_call_args(
    t: &mut std::collections::VecDeque<Tok>,
    v: &std::collections::BTreeMap<String, ExprVal>,
) -> Option<Vec<ExprVal>> {
    let mut args = Vec::new();
    if matches!(t.front(), Some(Tok::Op(s)) if s == ")") {
        t.pop_front();
        return Some(args);
    }
    loop {
        args.push(expr_or(t, v)?);
        match t.pop_front() {
            Some(Tok::Op(ref c)) if c == "," => continue,
            Some(Tok::Op(ref c)) if c == ")" => break,
            _ => return None,
        }
    }
    Some(args)
}

/// Apply a built-in `expr` function to its evaluated arguments.
fn expr_call_fn(name: &str, args: &[ExprVal]) -> Option<ExprVal> {
    match name {
        "round" => {
            let x = args.first()?.as_num()?;
            let n = args.get(1).and_then(|a| a.as_num()).unwrap_or(0.0) as i32;
            let f = 10f64.powi(n);
            Some(ExprVal::Num((x * f).round() / f))
        }
        "abs" => Some(ExprVal::Num(args.first()?.as_num()?.abs())),
        "min" => Some(ExprVal::Num(
            args.first()?.as_num()?.min(args.get(1)?.as_num()?),
        )),
        "max" => Some(ExprVal::Num(
            args.first()?.as_num()?.max(args.get(1)?.as_num()?),
        )),
        "len" => Some(ExprVal::Num(args.first()?.as_text().chars().count() as f64)),
        "lower" => Some(ExprVal::Str(args.first()?.as_text().to_lowercase())),
        "upper" => Some(ExprVal::Str(args.first()?.as_text().to_uppercase())),
        "trim" => Some(ExprVal::Str(args.first()?.as_text().trim().to_string())),
        "reverse" => Some(ExprVal::Str(
            args.first()?.as_text().chars().rev().collect(),
        )),
        "contains" => Some(ExprVal::Bool(
            args.first()?.as_text().contains(&args.get(1)?.as_text()),
        )),
        "replace" => Some(ExprVal::Str(
            args.first()?
                .as_text()
                .replace(&args.get(1)?.as_text(), &args.get(2)?.as_text()),
        )),
        "repeat" => {
            let s = args.first()?.as_text();
            let n = args.get(1)?.as_num()?;
            if !(0.0..=100_000.0).contains(&n) || s.len().saturating_mul(n as usize) > 1_000_000 {
                return None;
            }
            Some(ExprVal::Str(s.repeat(n as usize)))
        }
        "concat" => Some(ExprVal::Str(args.iter().map(|a| a.as_text()).collect())),
        _ => None,
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

/// If `value` is a string that parses as a JSON object or array, return the parsed value; otherwise
/// return it unchanged. Op results are stored as JSON strings, so this lets `jq` introspect them (a
/// bare scalar string is left alone, so `jq` over real text still behaves).
fn jq_parse_input(value: serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::String(s) = &value {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
            if parsed.is_object() || parsed.is_array() {
                return parsed;
            }
        }
    }
    value
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
        Node::Scope { .. } => "scope",
        Node::Saga { .. } => "saga",
        Node::Once { .. } => "once",
        Node::Checkpoint { .. } => "checkpoint",
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

    #[tokio::test]
    async fn optimized_plan_drops_a_dead_read_yet_matches_the_result() {
        use crate::ast::HirFlow;

        struct ROCat;
        impl OpCatalog for ROCat {
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
        struct ROHost(ROCat);
        #[async_trait::async_trait]
        impl OpHost for ROHost {
            async fn dispatch(&self, op: &str, input: serde_json::Value) -> OpOutcome {
                OpOutcome::ok(format!("{op}({input})"))
            }
            fn catalog(&self) -> &dyn OpCatalog {
                &self.0
            }
            async fn request_approval(
                &self,
                _l: &str,
                _i: &flux_spec::IntentSet,
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
        // $dead = read "x" (never used); $r = read "y" (the flow's result).
        let body = vec![
            bind_read("dead", flow_lit(json!("x"))),
            bind_read("r", flow_lit(json!("y"))),
        ];
        let host = ROHost(ROCat);

        let hir = HirFlow {
            body: body.clone(),
            ..Default::default()
        };
        let plan = crate::optimize::optimize(&hir, host.catalog());

        let store_plan = MemStore::new();
        let mut sink = BufferSink::default();
        let out_plan = execute_plan(&store_plan, &host, "s", &body, &plan, &mut sink)
            .await
            .unwrap();

        let store_flow = MemStore::new();
        let ast = DraftAst {
            body: body.clone(),
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let out_flow = execute_flow(&store_flow, &host, "s", &ast, &mut sink2)
            .await
            .unwrap();

        // Same observable result despite the dropped step.
        assert_eq!(out_plan.result, out_flow.result);
        // The dead read was eliminated: it never ran in the optimized plan, so `$dead` is unbound
        // there — while `execute_flow`, which runs every node, does bind it.
        assert!(
            store_plan
                .resolve("s", &SymbolName("dead".into()))
                .unwrap()
                .is_none(),
            "the dead read is eliminated from the optimized plan"
        );
        assert!(store_flow
            .resolve("s", &SymbolName("dead".into()))
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn optimized_plan_deduplicates_an_identical_read() {
        use crate::ast::HirFlow;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountCat;
        impl OpCatalog for CountCat {
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
        struct CountHost {
            cat: CountCat,
            calls: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl OpHost for CountHost {
            async fn dispatch(&self, op: &str, input: serde_json::Value) -> OpOutcome {
                self.calls.fetch_add(1, Ordering::SeqCst);
                OpOutcome::ok(format!("{op}({input})"))
            }
            fn catalog(&self) -> &dyn OpCatalog {
                &self.cat
            }
            async fn request_approval(
                &self,
                _l: &str,
                _i: &flux_spec::IntentSet,
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
        // $a = read "x"; $b = read "x" (duplicate); $r = read "{{a}}{{b}}" (result, consumes both).
        let body = vec![
            bind_read("a", flow_lit(json!("x"))),
            bind_read("b", flow_lit(json!("x"))),
            bind_read("r", flow_lit(json!("{{a}}{{b}}"))),
        ];

        // Optimized: CSE collapses $b into an alias of $a, so `read("x")` dispatches ONCE.
        let host = CountHost {
            cat: CountCat,
            calls: AtomicUsize::new(0),
        };
        let hir = HirFlow {
            body: body.clone(),
            ..Default::default()
        };
        let plan = crate::optimize::optimize(&hir, host.catalog());
        let store_plan = MemStore::new();
        let mut sink = BufferSink::default();
        let out_plan = execute_plan(&store_plan, &host, "s", &body, &plan, &mut sink)
            .await
            .unwrap();
        let plan_calls = host.calls.load(Ordering::SeqCst);

        // Linear: every node dispatches — read("x") twice + the read for $r = 3.
        let host2 = CountHost {
            cat: CountCat,
            calls: AtomicUsize::new(0),
        };
        let store_flow = MemStore::new();
        let ast = DraftAst {
            body: body.clone(),
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let out_flow = execute_flow(&store_flow, &host2, "s", &ast, &mut sink2)
            .await
            .unwrap();
        let flow_calls = host2.calls.load(Ordering::SeqCst);

        assert_eq!(plan_calls, 2, "optimized: read(\"x\") once + read for $r");
        assert_eq!(flow_calls, 3, "linear: read(\"x\") twice + read for $r");
        // $b reused $a's value (same stored value), and the observable result is unchanged.
        let va = store_plan
            .resolve("s", &SymbolName("a".into()))
            .unwrap()
            .and_then(|id| store_plan.get_value(&id).unwrap());
        let vb = store_plan
            .resolve("s", &SymbolName("b".into()))
            .unwrap()
            .and_then(|id| store_plan.get_value(&id).unwrap());
        assert!(va.is_some(), "$a is bound");
        assert_eq!(va, vb, "$b aliases $a → identical value");
        assert_eq!(out_plan.result, out_flow.result);
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
    async fn explicit_empty_return_wins_over_stale_last() {
        // Regression (Fix B): a non-empty expression sets `last`, then an explicit `return ""` must
        // still return "" — not leak the stale `last`. The agent loop returns an empty `$answer` when
        // it exhausts its iteration budget; the stale-last leak surfaced loop-machinery text
        // (`observed \`turn.iteration\``) as the turn's answer with a falsely-`ok` outcome.
        let host = CfHost::new();
        let body = vec![
            echo("HELLO"),
            Node::Return {
                value: Box::new(flow_lit(json!(""))),
            },
        ];
        let out = run(&host, body).await.unwrap();
        assert_eq!(
            out.result, "",
            "explicit empty return must override the stale last value"
        );
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
    async fn scope_runs_finally_on_success_return_and_error() {
        // (1) Normal completion: body runs, then finally.
        let host = CfHost::new();
        let body = vec![Node::Scope {
            acquire: None,
            bind: None,
            body: vec![echo("body")],
            finally: vec![echo("cleanup")],
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["body", "cleanup"]);

        // (2) An early `return` inside the body still runs finally.
        let host = CfHost::new();
        let body = vec![Node::Scope {
            acquire: None,
            bind: None,
            body: vec![
                echo("body"),
                Node::Return {
                    value: Box::new(flow_lit(json!("done"))),
                },
                echo("unreached"),
            ],
            finally: vec![echo("cleanup")],
        }];
        run(&host, body).await.unwrap();
        assert_eq!(
            host.marks(),
            vec!["body", "cleanup"],
            "return unwinds the body but finally still runs (and the post-return op does not)"
        );

        // (3) An error in the body still runs finally, then propagates.
        let host = CfHost::new();
        let body = vec![Node::Scope {
            acquire: None,
            bind: None,
            body: vec![call("boom", vec![])],
            finally: vec![echo("cleanup")],
        }];
        assert!(run(&host, body).await.is_err());
        assert_eq!(
            host.marks(),
            vec!["boom", "cleanup"],
            "cleanup runs even when the body errors"
        );
    }

    #[tokio::test]
    async fn scope_acquire_runs_first_and_binds_the_resource() {
        // acquire → use → release ordering; binding the resource to $h must not error.
        let host = CfHost::new();
        let body = vec![Node::Scope {
            acquire: Some(Box::new(echo("acq"))),
            bind: Some(SymbolName("h".into())),
            body: vec![echo("body")],
            finally: vec![echo("cleanup")],
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["acq", "body", "cleanup"]);
    }

    #[tokio::test]
    async fn saga_unwinds_completed_steps_in_reverse_on_failure() {
        use crate::ast::SagaStep;

        // step1 ok (undo r1), step2 ok (undo r2), step3 BOOMS → undo r2 then undo r1, original error.
        let host = CfHost::new();
        let body = vec![Node::Saga {
            steps: vec![
                SagaStep {
                    body: vec![echo("s1")],
                    undo: vec![echo("r1")],
                },
                SagaStep {
                    body: vec![echo("s2")],
                    undo: vec![echo("r2")],
                },
                SagaStep {
                    body: vec![call("boom", vec![])],
                    undo: vec![echo("r3")],
                },
            ],
        }];
        assert!(run(&host, body).await.is_err());
        assert_eq!(
            host.marks(),
            vec!["s1", "s2", "boom", "r2", "r1"],
            "compensations run in reverse for the steps that completed; the failed step's undo is not run"
        );

        // all steps succeed → no compensation runs.
        let host = CfHost::new();
        let body = vec![Node::Saga {
            steps: vec![
                SagaStep {
                    body: vec![echo("s1")],
                    undo: vec![echo("r1")],
                },
                SagaStep {
                    body: vec![echo("s2")],
                    undo: vec![echo("r2")],
                },
            ],
        }];
        run(&host, body).await.unwrap();
        assert_eq!(host.marks(), vec!["s1", "s2"], "no failure → no unwind");
    }

    #[tokio::test]
    async fn once_runs_the_body_at_most_once_across_runs() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Once {
                label: "welcome".into(),
                body: vec![echo("sent")],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();

        // First run: the body executes.
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(host.marks(), vec!["sent"]);

        // Second run, SAME store + session: the durable record skips the side effect.
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            host.marks(),
            vec!["sent"],
            "the side effect did not fire again"
        );
    }

    #[tokio::test]
    async fn once_failure_is_not_recorded_and_retries() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Once {
                label: "x".into(),
                body: vec![call("boom", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        assert!(execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .is_err());
        // Nothing was recorded, so a re-run tries (and fails) again.
        assert!(execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .is_err());
        assert_eq!(
            host.marks(),
            vec!["boom", "boom"],
            "a failed once is retried"
        );
    }

    #[tokio::test]
    async fn once_without_a_durable_store_runs_every_time() {
        use crate::ast::ValueId;
        use crate::store::{SessionView, ValueStore};
        use flux_core::Result;

        // A store whose `as_durable()` is `None` (the default) degrades `once` to "run every time".
        struct NoDurable(MemStore);
        impl ValueStore for NoDurable {
            fn put_value(&self, s: &str, v: &Value) -> Result<ValueId> {
                self.0.put_value(s, v)
            }
            fn get_value(&self, id: &ValueId) -> Result<Option<Value>> {
                self.0.get_value(id)
            }
            fn bind(
                &self,
                s: &str,
                n: &SymbolName,
                vid: &ValueId,
                ty: Option<&str>,
                summary: &str,
                vis: Visibility,
            ) -> Result<()> {
                self.0.bind(s, n, vid, ty, summary, vis)
            }
            fn resolve(&self, s: &str, n: &SymbolName) -> Result<Option<ValueId>> {
                self.0.resolve(s, n)
            }
            fn append_event(&self, s: &str, e: &RunEvent) -> Result<()> {
                self.0.append_event(s, e)
            }
            fn view(&self, s: &str) -> Result<SessionView> {
                self.0.view(s)
            }
            // as_durable() keeps the trait default → None.
        }

        let host = CfHost::new();
        let store = NoDurable(MemStore::new());
        let ast = DraftAst {
            body: vec![Node::Once {
                label: "welcome".into(),
                body: vec![echo("sent")],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            host.marks(),
            vec!["sent", "sent"],
            "no durable store → once runs each time"
        );
    }

    #[tokio::test]
    async fn checkpoint_fast_forwards_past_the_completed_prefix_on_rerun() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            name: Some("phased".into()),
            body: vec![
                echo("step1"),
                Node::Checkpoint { label: "p1".into() },
                echo("step2"),
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();

        // First run: both steps execute; the checkpoint records the resume cursor after step1.
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(host.marks(), vec!["step1", "step2"]);

        // Second run, SAME store + session: fast-forward past the checkpoint → only step2 re-runs.
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            host.marks(),
            vec!["step1", "step2", "step2"],
            "the pre-checkpoint prefix is not re-executed"
        );
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

    // ---- P6c: thing resolution ----

    #[tokio::test]
    async fn thing_bind_resolves_self_identifying_selectors_else_errors() {
        use crate::ast::{Selector, ThingKind, ThingRef};
        let host = CfHost::new(); // uses the default deterministic `resolve_thing`
        let store = MemStore::new();

        // A File addressed by Path resolves deterministically and binds a `Thing` value.
        let ast = DraftAst {
            body: vec![Node::Bind {
                name: SymbolName("f".into()),
                value: Box::new(Node::Thing {
                    thing: ThingRef {
                        kind: ThingKind::File,
                        selector: Selector::Path("README.md".into()),
                    },
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        match store
            .resolve("s", &SymbolName("f".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap())
        {
            Some(Value::Thing(rt)) => {
                assert_eq!(rt.display, "README.md");
                assert_eq!(rt.confidence, 1.0);
            }
            other => panic!("expected a resolved Thing, got {other:?}"),
        }
        assert!(
            store
                .events("s")
                .iter()
                .any(|e| matches!(e, RunEvent::ThingResolved { .. })),
            "a ThingResolved event was recorded"
        );

        // A Person by Name is ambiguous — no deterministic resolution → runtime error.
        let amb = DraftAst {
            body: vec![Node::Bind {
                name: SymbolName("p".into()),
                value: Box::new(Node::Thing {
                    thing: ThingRef {
                        kind: ThingKind::Person,
                        selector: Selector::Name("Ada".into()),
                    },
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        assert!(execute_flow(&store, &host, "s2", &amb, &mut sink2)
            .await
            .is_err());
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

    // --- expr evaluator ------------------------------------------------------

    fn ev(formula: &str, vars: &[(&str, ExprVal)]) -> ExprVal {
        let map: std::collections::BTreeMap<String, ExprVal> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        eval_expr_value(formula, &map).unwrap()
    }

    #[test]
    fn expr_arithmetic_is_backward_compatible() {
        // Numeric vars, a numeric string var (coerced), and the existing functions still work.
        assert_eq!(
            ev("price * 2", &[("price", ExprVal::Num(21.0))]),
            ExprVal::Num(42.0)
        );
        assert_eq!(
            ev("price * 2", &[("price", ExprVal::Str("59557.985".into()))]),
            ExprVal::Num(119115.97)
        );
        assert_eq!(ev("round(3.456, 2)", &[]), ExprVal::Num(3.46));
        assert_eq!(ev("max(min(5, 9), 1)", &[]), ExprVal::Num(5.0));
        assert_eq!(ev("-(2 + 3) * 2", &[]), ExprVal::Num(-10.0));
        // Division by zero is a clean error, not a panic.
        let map = std::collections::BTreeMap::new();
        assert!(eval_expr_value("1 / 0", &map).is_err());
    }

    #[test]
    fn expr_comparison_and_boolean_return_bools() {
        assert_eq!(ev("2 == 2", &[]), ExprVal::Bool(true));
        assert_eq!(ev("3 != 2", &[]), ExprVal::Bool(true));
        assert_eq!(ev("3 < 2", &[]), ExprVal::Bool(false));
        assert_eq!(ev("3 >= 3", &[]), ExprVal::Bool(true));
        assert_eq!(ev("'ok' == 'ok'", &[]), ExprVal::Bool(true));
        assert_eq!(ev("'a' < 'b'", &[]), ExprVal::Bool(true));
        assert_eq!(ev("true && false", &[]), ExprVal::Bool(false));
        assert_eq!(ev("true || false", &[]), ExprVal::Bool(true));
        assert_eq!(ev("!false", &[]), ExprVal::Bool(true));
        // Compose with vars and precedence: && binds tighter than ||.
        assert_eq!(
            ev(
                "status == 'ok' && count > 0",
                &[
                    ("status", ExprVal::Str("ok".into())),
                    ("count", ExprVal::Num(3.0)),
                ]
            ),
            ExprVal::Bool(true)
        );
    }

    #[test]
    fn expr_string_functions() {
        assert_eq!(ev("upper('hi')", &[]), ExprVal::Str("HI".into()));
        assert_eq!(ev("lower('HI')", &[]), ExprVal::Str("hi".into()));
        assert_eq!(ev("trim('  x  ')", &[]), ExprVal::Str("x".into()));
        assert_eq!(ev("len('hello')", &[]), ExprVal::Num(5.0));
        assert_eq!(ev("reverse('abc')", &[]), ExprVal::Str("cba".into()));
        assert_eq!(
            ev("replace('a-b-c', '-', '_')", &[]),
            ExprVal::Str("a_b_c".into())
        );
        assert_eq!(ev("repeat('ab', 3)", &[]), ExprVal::Str("ababab".into()));
        assert_eq!(ev("contains('hello', 'ell')", &[]), ExprVal::Bool(true));
        assert_eq!(ev("concat('a', 'b', 'c')", &[]), ExprVal::Str("abc".into()));
        // `+` concatenates when either side is non-numeric text.
        assert_eq!(
            ev("'v' + n", &[("n", ExprVal::Num(2.0))]),
            ExprVal::Str("v2".into())
        );
        // An out-of-bounds repeat is rejected rather than allocating unboundedly.
        let map: std::collections::BTreeMap<String, ExprVal> = Default::default();
        assert!(eval_expr_value("repeat('x', 9999999)", &map).is_err());
    }

    #[test]
    fn expr_value_truthiness_matches_json() {
        assert!(ExprVal::Bool(true).truthy());
        assert!(!ExprVal::Bool(false).truthy());
        assert!(ExprVal::Num(1.0).truthy());
        assert!(!ExprVal::Num(0.0).truthy());
        assert!(ExprVal::Str("yes".into()).truthy());
        assert!(!ExprVal::Str("false".into()).truthy());
        assert!(!ExprVal::Str("0".into()).truthy());
        assert!(!ExprVal::Str("  ".into()).truthy());
    }
}

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
use flux_core::{Error, Result, Usage};
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

/// Bind an already-stored value id to a session symbol, deriving the one-line summary from its text.
/// Used by `seq`/`each`/`pipe`/`parallel` to bind a block's result (the value already exists in the
/// store; only the symbol mapping is new).
fn bind_existing(
    store: &FlowStore,
    session_id: &str,
    name: &SymbolName,
    vid: &ValueId,
) -> Result<()> {
    let summary = store
        .get_value(vid)?
        .map(|v| summarize(&value_text(&v)))
        .unwrap_or_default();
    store.bind(session_id, name, vid, None, &summary, Visibility::Visible)
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
    /// The flow's result rendered as text (for display) — the *last* node's view. This is what a
    /// one-shot CLI prints and what an explicit `return` carries.
    pub result: String,
    /// The model-facing transcript: every read/call node's view, labeled and concatenated. The engine
    /// feeds THIS back between rounds so the model sees *all* of a plan's reads — not just the last —
    /// which is what lets "read N files, then answer" converge instead of re-reading every round.
    pub transcript: String,
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
    ToolResult(String, ToolResult),
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
    fn replay(self, sink: &mut dyn AgentSink) {
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

impl AgentSink for BufferSink {
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
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
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
    let mut transcript: Vec<String> = Vec::new();
    let (last, _last_value, step) = exec_body(
        store,
        executor,
        session_id,
        &ast.body,
        sink,
        &mut steps,
        &mut transcript,
    )
    .await?;
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
        transcript: transcript.join("\n\n"),
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
                    // Record EVERY node's view in the transcript so the round feedback surfaces all of
                    // a plan's reads, not just the last one.
                    transcript.push(format!("[${} = {op}]\n{}", name.0, outcome.view));
                    last = outcome.view;
                    last_value = outcome.value_id;
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
                    transcript.push(format!("[{op}]\n{}", outcome.view));
                    last = outcome.view;
                    last_value = outcome.value_id;
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
                } => {
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
                            last_value = bvid;
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
                }
                Node::Each {
                    source,
                    item,
                    body: ebody,
                    collect,
                } => {
                    let list = eval_arg(source, store, session_id)?;
                    let serde_json::Value::Array(elems) = list else {
                        return Err(Error::Other(
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
                        let list_vid = store.put_value(session_id, &Value::List(items))?;
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
                        return Err(Error::Other(format!("assertion failed: {detail}")));
                    }
                }
                Node::Pipe {
                    steps: psteps,
                    bind,
                } => {
                    let mut prev: Option<ValueId> = None;
                    for step in psteps {
                        let Node::Call { op, args } = step else {
                            return Err(Error::Other(
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
                            return Err(Error::Other(format!(
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
                        return Err(Error::Other(
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
                        return Err(Error::Other(format!(
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
                        Ok::<_, Error>((b, buf, s, tr, text, lv, step))
                    });
                    let results = futures::future::try_join_all(futs).await?;
                    for (b, buf, s, tr, text, lv, step) in results {
                        if let Step::Return(_) = step {
                            return Err(Error::Other(
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
                            store, executor, session_id, rbody, &mut *sink, &mut *steps, &mut *transcript,
                        ).await {
                            Ok((blast, bvid, step)) => {
                                if !blast.is_empty() { last = blast; }
                                last_vid = bvid;
                                if let Step::Return(v) = step {
                                    return Ok((last, v.clone(), Step::Return(v)));
                                }
                                succeeded = true;
                                break;
                            }
                            Err(e) => {
                                last_err = e.to_string();
                            }
                        }
                    }
                    if !succeeded {
                        return Err(Error::Other(format!(
                            "`retry` exhausted {} attempt(s): {}", max, last_err
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
                        store, executor, session_id, tbody, &mut *sink, &mut *steps, &mut *transcript,
                    ).await {
                        Ok((blast, bvid, step)) => {
                            if !blast.is_empty() { last = blast; }
                            last_value = bvid;
                            if let Step::Return(v) = step {
                                return Ok((last, v.clone(), Step::Return(v)));
                            }
                        }
                        Err(e) => {
                            if let Some(cname) = catch {
                                let err_vid = store.put_value(session_id, &Value::String(e.to_string()))?;
                                bind_existing(store, session_id, cname, &err_vid)?;
                            }
                            let (hblast, hvid, hstep) = exec_body(
                                store, executor, session_id, handler, &mut *sink, &mut *steps, &mut *transcript,
                            ).await?;
                            if !hblast.is_empty() { last = hblast; }
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
                    let intents = flux_spec::IntentSet::new();
                    let risk_tag = risk.as_deref().unwrap_or("medium");
                    let labelled = format!("[{risk_tag}] {message}");
                    let choice = executor.approver().request("confirm", std::slice::from_ref(&labelled), &intents).await;
                    if !matches!(choice, ApprovalChoice::Allow) {
                        return Err(Error::Other(format!(
                            "`confirm` denied: {}", message
                        )));
                    }
                    let (blast, bvid, step) = exec_body(
                        store, executor, session_id, cbody, &mut *sink, &mut *steps, &mut *transcript,
                    ).await?;
                    if !blast.is_empty() { last = blast; }
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
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(*for_ms);
                    let mut last_vid: Option<ValueId> = None;
                    loop {
                        if std::time::Instant::now() >= deadline { break; }
                        match exec_body(
                            store, executor, session_id, lbody, &mut *sink, &mut *steps, &mut *transcript,
                        ).await {
                            Ok((blast, bvid, step)) => {
                                if !blast.is_empty() { last = blast; }
                                last_vid = bvid;
                                if let Step::Return(v) = step {
                                    return Ok((last, v.clone(), Step::Return(v)));
                                }
                            }
                            Err(e) => {
                                return Err(Error::Other(format!("`loop` body failed: {e}")));
                            }
                        }
                        if let Some(u) = until {
                            if eval_cond(store, executor, session_id, u, &mut *sink, &mut *steps).await? {
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
                    let deadline = tokio::time::Instant::now()
                        + std::time::Duration::from_millis(*timeout_ms);
                    // Run branches sequentially; return the first success within the deadline.
                    let mut race_result: Option<(String, Option<ValueId>, Step)> = None;
                    for b in branches {
                        if tokio::time::Instant::now() >= deadline {
                            break;
                        }
                        match exec_body(
                            store, executor, session_id, &b.body, &mut *sink, &mut *steps, &mut *transcript,
                        ).await {
                            Ok((blast, bvid, step)) => {
                                race_result = Some((blast, bvid, step));
                                break;
                            }
                            Err(_) => continue,
                        }
                    }
                    let (blast, bvid, step) = race_result.ok_or_else(|| {
                        Error::Other(format!("`race` timed out after {timeout_ms}ms with no successful branch"))
                    })?;
                    if !blast.is_empty() { last = blast; }
                    if let (Some(name), Some(vid)) = (bind, &bvid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Throttle {
                    max,
                    window_ms,
                    body: tbody,
                } => {
                    // Token-bucket: track call timestamps in the value store as a synthetic key.
                    // Simple in-process approach: store call times as a JSON array in the store.
                    let bucket_key = SymbolName(format!("__throttle_bucket_{session_id}_{max}_{window_ms}"));
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let window_start = now_ms.saturating_sub(*window_ms);
                    // Load existing timestamps.
                    let mut times: Vec<u64> = if let Some(vid) = store.resolve(session_id, &bucket_key).ok().flatten() {
                        if let Some(Value::String(s)) = store.get_value(&vid).ok().flatten() {
                            serde_json::from_str::<Vec<u64>>(&s).unwrap_or_default()
                        } else { vec![] }
                    } else { vec![] };
                    // Evict expired entries.
                    times.retain(|&t| t >= window_start);
                    if times.len() >= *max as usize {
                        return Err(Error::Other(format!(
                            "`throttle` limit of {max} per {window_ms}ms exceeded"
                        )));
                    }
                    times.push(now_ms);
                    let times_json = serde_json::to_string(&times).unwrap_or_default();
                    let vid = store.put_value(session_id, &Value::String(times_json))?;
                    store.bind(session_id, &bucket_key, &vid, None, "", Visibility::Hidden)?;
                    let (blast, bvid, step) = exec_body(
                        store, executor, session_id, tbody, sink, steps, transcript,
                    ).await?;
                    if !blast.is_empty() { last = blast; }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Debounce { wait_ms, body: dbody } => {
                    // Debounce: sleep for wait_ms then run body once.
                    tokio::time::sleep(std::time::Duration::from_millis(*wait_ms)).await;
                    let (blast, bvid, step) = exec_body(
                        store, executor, session_id, dbody, sink, steps, transcript,
                    ).await?;
                    if !blast.is_empty() { last = blast; }
                    last_value = bvid;
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
                    }
                }
                Node::Unless { cond, body: ubody } => {
                    // Sugar for `when !cond`: run body only when condition is falsey.
                    let take = !eval_cond(store, executor, session_id, cond, &mut *sink, &mut *steps).await?;
                    if take {
                        let (blast, bvid, step) = exec_body(
                            store, executor, session_id, ubody, &mut *sink, &mut *steps, &mut *transcript,
                        ).await?;
                        if !blast.is_empty() { last = blast; last_value = bvid; }
                        if let Step::Return(v) = step {
                            return Ok((last, v.clone(), Step::Return(v)));
                        }
                    }
                }
                Node::Verify { cmd, expect, message } => {
                    // Run `cmd`, check output contains/matches `expect`; abort with `message` if not.
                    let (cmd_text, _) =
                        eval_return(store, executor, session_id, cmd, &mut *sink, &mut *steps).await?;
                    let expect_val = eval_arg(expect, store, session_id)?;
                    let pattern = match &expect_val {
                        serde_json::Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    };
                    let ok = cmd_text.contains(pattern.as_str());
                    if !ok {
                        let detail = message.clone().unwrap_or_else(|| {
                            format!("output did not contain {:?}", pattern)
                        });
                        return Err(Error::Other(format!("verify failed: {detail}")));
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
                Node::Unless { .. } | Node::Verify { .. } | Node::Peek { .. } => {
                    // handled above — this arm is unreachable but satisfies exhaustiveness
                    unreachable!()
                }
            }
        }
        Ok((last, last_value, Step::Next))
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
        Node::Each { source, body, .. } => {
            walk_node(source, f);
            walk_calls(body, f);
        }
        Node::Assert { cond, .. } => walk_node(cond, f),
        Node::Pipe { steps, .. } => walk_calls(steps, f),
        Node::Seq { body, .. } => walk_calls(body, f),
        Node::Memo { value, .. } => walk_node(value, f),
        Node::Parallel { branches } => {
            for b in branches {
                walk_calls(&b.body, f);
            }
        }
        Node::Return { value } => walk_node(value, f),
        Node::Retry { body, .. } => walk_calls(body, f),
        Node::Try { body, handler, .. } => {
            walk_calls(body, f);
            walk_calls(handler, f);
        }
        Node::Confirm { body, .. } => walk_calls(body, f),
        Node::Loop { until, body, .. } => {
            if let Some(u) = until {
                walk_node(u, f);
            }
            walk_calls(body, f);
        }
        Node::Race { branches, .. } => {
            for b in branches {
                walk_calls(&b.body, f);
            }
        }
        Node::Throttle { body, .. } => walk_calls(body, f),
        Node::Debounce { body, .. } => walk_calls(body, f),
        Node::Unless { body, .. } => walk_calls(body, f),
        Node::Verify { cmd, expect, .. } => { walk_node(cmd, f); walk_node(expect, f); }
        Node::Peek { .. } => {}
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
    async fn execute_flow_transcript_carries_every_node_not_just_last() {
        // The round feedback must surface ALL of a plan's reads, not just the last node — otherwise a
        // multi-read plan loops (the model re-reads what it couldn't see). `transcript` is that feed.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // $a = echo("alpha"); $b = echo("beta")  (no return)
        let ast = DraftAst {
            body: vec![
                flow_bind("a", "echo", vec![flow_lit(json!("alpha"))]),
                flow_bind("b", "echo", vec![flow_lit(json!("beta"))]),
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            outcome.result, "beta",
            "result is still the LAST node's view"
        );
        assert!(
            outcome.transcript.contains("alpha") && outcome.transcript.contains("beta"),
            "transcript must carry BOTH nodes, got: {}",
            outcome.transcript
        );
        assert!(
            outcome.transcript.contains("[$a = echo]"),
            "transcript labels each node by its bound symbol"
        );
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

    // ---- expanded node kinds (each / assert / pipe / seq / memo / parallel) ----

    #[tokio::test]
    async fn execute_flow_each_iterates_list_and_collects() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // each $f in ["a","b"] { $t = echo($f) } collect $all
        let ast = DraftAst {
            body: vec![Node::Each {
                source: Box::new(flow_lit(json!(["a", "b"]))),
                item: SymbolName("f".into()),
                body: vec![flow_bind("t", "echo", vec![flow_var("f")])],
                collect: Some(SymbolName("all".into())),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2, "body runs once per element");
        assert_eq!(sink.calls, vec!["echo", "echo"]);
        // echo echoes $f, so the last iteration's view is "b".
        assert_eq!(outcome.result, "b");
        // `collect` bound a list of the per-iteration results, in order.
        let vid = store
            .resolve("sess", &SymbolName("all".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::List(vec![
                Value::String("a".into()),
                Value::String("b".into()),
            ]))
        );
    }

    #[tokio::test]
    async fn execute_flow_each_rejects_a_non_list_source() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Each {
                source: Box::new(flow_lit(json!("not a list"))),
                item: SymbolName("f".into()),
                body: vec![],
                collect: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("list"));
    }

    #[tokio::test]
    async fn execute_flow_assert_passes_when_true_and_aborts_when_false() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ok = DraftAst {
            body: vec![Node::Assert {
                cond: Box::new(flow_lit(json!(true))),
                message: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        assert!(execute_flow(&store, &ex, "sess", &ok, &mut sink)
            .await
            .is_ok());

        let bad = DraftAst {
            body: vec![Node::Assert {
                cond: Box::new(flow_lit(json!(false))),
                message: Some("nope".into()),
            }],
            ..Default::default()
        };
        let err = execute_flow(&store, &ex, "sess", &bad, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("assertion failed"));
        assert!(err.to_string().contains("nope"));
    }

    #[tokio::test]
    async fn execute_flow_pipe_feeds_output_as_next_first_arg() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // pipe { echo("alpha"); echo() } -> $out  (the 2nd echo gets "alpha" as its first arg)
        let ast = DraftAst {
            body: vec![Node::Pipe {
                steps: vec![
                    Node::Call {
                        op: "echo".into(),
                        args: vec![flow_lit(json!("alpha"))],
                    },
                    Node::Call {
                        op: "echo".into(),
                        args: vec![],
                    },
                ],
                bind: Some(SymbolName("out".into())),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2);
        assert_eq!(
            outcome.result, "alpha",
            "the second step received the first's output as its first argument"
        );
        let vid = store
            .resolve("sess", &SymbolName("out".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("alpha".into()))
        );
    }

    #[tokio::test]
    async fn execute_flow_seq_runs_body_and_binds_last() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        // seq { echo("one"); $two = echo("two") } -> $last
        let ast = DraftAst {
            body: vec![Node::Seq {
                body: vec![
                    Node::Call {
                        op: "echo".into(),
                        args: vec![flow_lit(json!("one"))],
                    },
                    flow_bind("two", "echo", vec![flow_lit(json!("two"))]),
                ],
                bind: Some(SymbolName("last".into())),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2);
        let vid = store
            .resolve("sess", &SymbolName("last".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::String("two".into())),
            "`bind` captures the block's final value"
        );
    }

    #[tokio::test]
    async fn execute_flow_memo_computes_once_per_session() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Memo {
                name: SymbolName("survey".into()),
                value: Box::new(Node::Call {
                    op: "echo".into(),
                    args: vec![flow_lit(json!("expensive"))],
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        };
        // First run dispatches and binds.
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 1, "first memo run dispatches");
        assert_eq!(sink.calls, vec!["echo"]);

        // Second run on the SAME session+symbol reuses the cache — no dispatch.
        let mut sink2 = CollectSink::default();
        let outcome2 = execute_flow(&store, &ex, "sess", &ast, &mut sink2)
            .await
            .unwrap();
        assert_eq!(outcome2.steps, 0, "a memo hit skips execution");
        assert!(sink2.calls.is_empty(), "no op dispatched on a memo hit");
        assert_eq!(outcome2.result, "expensive", "the cached value is reused");

        // A different session is a fresh memo.
        let mut sink3 = CollectSink::default();
        let outcome3 = execute_flow(&store, &ex, "other", &ast, &mut sink3)
            .await
            .unwrap();
        assert_eq!(outcome3.steps, 1, "a different session recomputes");
    }

    #[tokio::test]
    async fn execute_flow_parallel_runs_branches_and_binds_names() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    crate::ast::Branch {
                        name: SymbolName("left".into()),
                        body: vec![Node::Call {
                            op: "echo".into(),
                            args: vec![flow_lit(json!("L"))],
                        }],
                    },
                    crate::ast::Branch {
                        name: SymbolName("right".into()),
                        body: vec![Node::Call {
                            op: "echo".into(),
                            args: vec![flow_lit(json!("R"))],
                        }],
                    },
                ],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 2, "both branches' ops dispatched");
        // Each branch bound its result to its name.
        let l = store
            .resolve("sess", &SymbolName("left".into()))
            .unwrap()
            .unwrap();
        let r = store
            .resolve("sess", &SymbolName("right".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&l).unwrap(),
            Some(Value::String("L".into()))
        );
        assert_eq!(
            store.get_value(&r).unwrap(),
            Some(Value::String("R".into()))
        );
        // The branches' buffered sink events were replayed into the real sink.
        assert_eq!(sink.calls.len(), 2, "both branch tool-calls replayed");
        assert!(sink.calls.iter().all(|c| c == "echo"));
    }

    #[test]
    fn plan_risk_walks_each_and_parallel_bodies() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        // A destructive bash nested in `each` + a mutating write nested in a `parallel` branch.
        let ast = DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(flow_lit(json!(["x"]))),
                    item: SymbolName("f".into()),
                    body: vec![flow_bind(
                        "d",
                        "bash",
                        vec![flow_lit(json!("rm -rf build"))],
                    )],
                    collect: None,
                },
                Node::Parallel {
                    branches: vec![crate::ast::Branch {
                        name: SymbolName("w".into()),
                        body: vec![flow_bind(
                            "o",
                            "write",
                            vec![flow_lit(json!("out.txt")), flow_lit(json!("data"))],
                        )],
                    }],
                },
            ],
            ..Default::default()
        };
        let risk = plan_risk(&ast, &reg);
        assert!(
            risk.destructive,
            "rm -rf inside `each` is seen by the risk walk"
        );
        assert!(
            risk.mutating,
            "write inside a `parallel` branch is seen by the walk"
        );
        assert!(
            risk.ops.contains(&"bash".to_string()) && risk.ops.contains(&"write".to_string()),
            "the walk recurses into the new container nodes"
        );
    }

    // ---- new node kinds: retry / try / confirm / loop / race / throttle / debounce ----

    #[tokio::test]
    async fn execute_flow_retry_succeeds_on_first_attempt() {
        // retry max 3: body always succeeds → runs once, result is the echo output.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: None,
                delay_ms: None,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("ok"))])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "ok");
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_try_catch_runs_handler_on_error() {
        // try { echo("good") } catch $e — body succeeds, handler not reached.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ok_ast = DraftAst {
            body: vec![Node::Try {
                catch: None,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("good"))])],
                handler: vec![],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_try_ok", &ok_ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "good");
        // handler nodes not executed
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_try_handler_runs_when_body_errors() {
        // try { unknown_op() } catch { echo("caught") } — body errors, handler runs.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let err_ast = DraftAst {
            body: vec![Node::Try {
                catch: None,
                body: vec![Node::Call {
                    op: "this_op_does_not_exist".into(),
                    args: vec![],
                }],
                handler: vec![flow_bind("h", "echo", vec![flow_lit(json!("caught"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_try_err", &err_ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "caught");
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_confirm_allow_runs_body() {
        // An auto-allow executor: confirm should proceed and run the body.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true); // auto-approve = true
        let ast = DraftAst {
            body: vec![Node::Confirm {
                message: "proceed?".into(),
                risk: Some("low".into()),
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("confirmed"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_confirm_ok", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "confirmed");
    }

    #[tokio::test]
    async fn execute_flow_confirm_deny_returns_error() {
        // A denying *approver* (with `echo` permitted, so the body would otherwise run): `confirm`
        // must error and short-circuit before the body. (Using a perm-denied echo would test the
        // wrong thing — the denial has to come from the confirm gate itself.)
        struct DenyApprover;
        #[async_trait]
        impl Approver for DenyApprover {
            async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
                ApprovalChoice::Deny
            }
        }
        let dir =
            std::env::temp_dir().join(format!("flux-flow-rt-{}-confirmdeny", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(DenyApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let store = FlowStore::in_memory().unwrap();
        let ast = DraftAst {
            body: vec![Node::Confirm {
                message: "dangerous action".into(),
                risk: Some("high".into()),
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("should not run"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess_confirm_deny", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("confirm"), "got: {err}");
        assert!(sink.calls.is_empty(), "body must not run when denied");
    }

    #[tokio::test]
    async fn execute_flow_loop_runs_until_deadline() {
        // loop for 50ms every 0ms: body runs at least once; deadline stops it.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Loop {
                for_ms: 50,
                every_ms: 0,
                until: None,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("tick"))])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        execute_flow(&store, &ex, "sess_loop", &ast, &mut sink)
            .await
            .unwrap();
        assert!(!sink.calls.is_empty(), "body must have run at least once");
        assert!(sink.calls.iter().all(|c| c == "echo"));
    }

    #[tokio::test]
    async fn execute_flow_loop_stops_on_until_condition() {
        // loop for 10_000ms every 0ms until lit(true): body runs exactly once then stops.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Loop {
                for_ms: 10_000,
                every_ms: 0,
                until: Some(Box::new(flow_lit(json!(true)))),
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("tick"))])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        execute_flow(&store, &ex, "sess_loop_until", &ast, &mut sink)
            .await
            .unwrap();
        // `until` is checked after the first iteration, so body runs exactly once.
        assert_eq!(sink.calls, vec!["echo"]);
    }

    #[tokio::test]
    async fn execute_flow_race_returns_first_success() {
        // race timeout=1000ms: first branch succeeds → result is first branch's echo.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 1_000,
                bind: Some(SymbolName("winner".into())),
                branches: vec![
                    crate::ast::Branch {
                        name: SymbolName("a".into()),
                        body: vec![flow_bind("ra", "echo", vec![flow_lit(json!("first"))])],
                    },
                    crate::ast::Branch {
                        name: SymbolName("b".into()),
                        body: vec![flow_bind("rb", "echo", vec![flow_lit(json!("second"))])],
                    },
                ],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_race", &ast, &mut sink)
            .await
            .unwrap();
        // The first branch always succeeds, so we get "first".
        assert_eq!(outcome.result, "first");
    }

    #[tokio::test]
    async fn execute_flow_race_errors_when_deadline_exceeded() {
        // race timeout=0ms: deadline is already past before any branch runs.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Race {
                timeout_ms: 0,
                bind: None,
                branches: vec![crate::ast::Branch {
                    name: SymbolName("a".into()),
                    body: vec![flow_bind("r", "echo", vec![flow_lit(json!("x"))])],
                }],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess_race_timeout", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_flow_throttle_allows_under_limit() {
        // throttle max=5 window=60000ms: a single call is well within the limit.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Throttle {
                max: 5,
                window_ms: 60_000,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("ok"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_throttle_ok", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "ok");
    }

    #[tokio::test]
    async fn execute_flow_throttle_rejects_over_limit() {
        // throttle max=1 window=60000ms: run the AST twice in the same session → second is rejected.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Throttle {
                max: 1,
                window_ms: 60_000,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("ok"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        // First run: succeeds.
        execute_flow(&store, &ex, "sess_throttle_limit", &ast, &mut sink)
            .await
            .unwrap();
        // Second run in the same session/window: should be rejected.
        let err = execute_flow(&store, &ex, "sess_throttle_limit", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("throttle"), "got: {err}");
    }

    #[tokio::test]
    async fn execute_flow_debounce_runs_body_after_delay() {
        // debounce wait=0ms: body runs (zero delay is fine).
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Debounce {
                wait_ms: 0,
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("debounced"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_debounce", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "debounced");
        assert_eq!(sink.calls, vec!["echo"]);
    }
}

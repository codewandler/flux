//! flux-flow's execution adapters + thin wrappers over the L0 reference interpreter in
//! [`flux_lang::runtime`]. The interpreter is generic over injected traits; here we implement them
//! over the real safety envelope (`Executor::dispatch` + approver), the SQLite `FlowStore`, and the
//! `AgentSink`, then expose `execute_flow` / `execute_call` with their original signatures so every
//! caller is unchanged.
//!
//! `plan_risk` + `PlanApprover` stay here: they need the concrete `ToolRegistry` and `Tool::intents`
//! (literal-arg destructive/path detection), which the language-level [`OpCatalog`] does not carry.
//! Every op still runs through `Executor::dispatch` — no new bypass surface.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;

use crate::AgentSink;
use flux_runtime::{
    scope_runtime_turn, ApprovalChoice, Approver, Executor, ToolRegistry, ToolResult,
};
use flux_spec::{IntentSet, Risk};

use flux_lang::host::{OpHost, OpOutcome};
use flux_lang::opspec::OpCatalog;
use flux_lang::program::CompositeOpDecl;
use flux_lang::sink::FlowSink;

use crate::ast::{DraftAst, FailureKind, Node};
use crate::registry::{schema_params, OpRegistry};
use crate::state::FlowStore;
use crate::Result;

/// The statement-identity hash a resumable-mode loop host needs to compute the prospective skip
/// prefix and the denial re-emission guard BEFORE executing anything (design Part 2, wired by A-16).
pub use flux_lang::runtime::stmt_hash16;
/// The interpreter's public types, re-exported so `flux_flow::runtime::{…}` paths are unchanged.
pub use flux_lang::runtime::{
    BindSpec, CallOutcome, FlowOutcome, LedgerEntry, PlanHalt, ResumeLedger, Suspension,
};

// ---------------------------------------------------------------------------
// Adapters: the engine's envelope → the interpreter's injected traits
// ---------------------------------------------------------------------------

/// Adapts the real [`Executor`] (dispatch + approver + registry) onto the interpreter's [`OpHost`].
struct ExecutorHost<'a> {
    executor: &'a Executor,
    catalog: OpRegistry<'a>,
    /// The currently-open `with_tools` scope guards, LIFO. `OpHost::push_cap_scope`/`pop_cap_scope`
    /// are two separate async calls (not RAII) — the interpreter's `CapScope` node calls them across
    /// an `await` boundary around its body — so the guard [`Executor::push_cap_scope`] returns has to
    /// live somewhere between the two calls. A stack (not a single slot) makes nested `with_tools`
    /// blocks safe: each push adds a guard, each pop drops exactly the innermost one, matching the
    /// executor's own stack discipline. `Executor::dispatch` is the actual enforcement point; dropping
    /// a guard here only pops the shared stack it also reads.
    cap_scope_guards: std::sync::Mutex<Vec<flux_runtime::CapScopeGuard<'a>>>,
    /// C-43: the active cassette scope, self-wired from [`FlowStore::cassette`] by authored-flow
    /// execution entry points. `Record` appends a redacted cell after every dispatch (the live
    /// outcome flows back unredacted); `Replay` serves cells without ever touching the executor,
    /// so no side effect re-fires. `None` on the outer agent-loop path and whenever the cassette is
    /// off.
    cassette: Option<Arc<crate::cassette::CassetteScope>>,
}

impl<'a> ExecutorHost<'a> {
    fn new(executor: &'a Executor) -> Self {
        Self {
            catalog: OpRegistry::new(executor.registry()),
            executor,
            cap_scope_guards: std::sync::Mutex::new(Vec::new()),
            cassette: None,
        }
    }

    fn new_with_composites(executor: &'a Executor, composites: &'a [CompositeOpDecl]) -> Self {
        Self {
            catalog: OpRegistry::new(executor.registry()).with_composites(composites),
            executor,
            cap_scope_guards: std::sync::Mutex::new(Vec::new()),
            cassette: None,
        }
    }

    /// D-175: append the cell for one completed **live** dispatch — `Record` records unconditionally,
    /// `Frozen(Live)`/`Resume` record only the tail a `Miss` fell through to (the bridge/tail target
    /// each scope carries), `Replay`/`Frozen(Halt)` never reach here (they never fall through to a
    /// live dispatch at all). Generalizes the old `record_cell` so every scope's tail-recording lives
    /// in one place, matching `dispatch`'s single live path.
    fn tail_record(&self, op: &str, input_json: &str, out: &OpOutcome) {
        let Some(scope) = &self.cassette else {
            return;
        };
        let redactor = &self.executor.context().redactor;
        match scope.as_ref() {
            crate::cassette::CassetteScope::Record(rec) => {
                rec.record(redactor, op, input_json, out)
            }
            crate::cassette::CassetteScope::Frozen(frozen) => {
                frozen.record_tail(redactor, op, input_json, out)
            }
            crate::cassette::CassetteScope::Resume(resume) => {
                resume.record_tail(redactor, op, input_json, out)
            }
            // `Replay` and `Frozen(Halt)` never fall through to the live path this is called from.
            crate::cassette::CassetteScope::Replay(_) => {}
        }
    }
}

/// Shape a `Frozen`/`Resume` refusal as the same in-band, fatal, non-silent error
/// [`ReplayTape`](crate::cassette::ReplayTape) divergence already is: `is_error: true`, `denied:
/// false` (a scope refusal is a divergence, not an authorization denial — `Executor::dispatch_outcome`
/// still owns `denied`).
fn cassette_refused(reason: &str) -> OpOutcome {
    OpOutcome {
        denied: false,
        timing: None,
        content: format!("cassette diverged: {reason}"),
        view: None,
        is_error: true,
    }
}

#[async_trait]
impl OpHost for ExecutorHost<'_> {
    async fn dispatch(&self, op: &str, input: serde_json::Value) -> OpOutcome {
        // C-43: serialize the input once when a cassette scope is active — the recorder keys cells
        // on the EXACT string `execute_call` hashes for `StepStarted.input_hash`, and replay
        // re-derives the same key from the same serialization.
        let input_json = if self.cassette.is_some() {
            serde_json::to_string(&input).unwrap_or_default()
        } else {
            String::new()
        };
        // D-175: every cassette-scope arm is matched BEFORE the one live path below — `Served`/
        // `Refused` return immediately (no side effect can re-fire); every `Miss` (including
        // `Record`'s scope, which never serves at all) falls into the SAME live path, so there is
        // exactly one dispatch site no matter which scope is active (no-fallbacks rule).
        if let Some(scope) = &self.cassette {
            match scope.as_ref() {
                // C-43 replay: serve from the tape and never touch the live executor — no side
                // effect can re-fire. A miss is a latched divergence surfaced as an in-band op error
                // (the statement halts; the driver reports `ReplayTape::diverged`), never silent
                // continuation.
                crate::cassette::CassetteScope::Replay(tape) => {
                    return match tape.serve(op, &input_json) {
                        Some(out) => out,
                        None => OpOutcome {
                            denied: false,
                            timing: None,
                            content: format!(
                                "replay diverged: {}",
                                tape.diverged()
                                    .unwrap_or_else(|| "unknown divergence".into())
                            ),
                            view: None,
                            is_error: true,
                        },
                    };
                }
                crate::cassette::CassetteScope::Frozen(frozen) => {
                    // D-177 policy mode: re-decide admissibility against THIS executor's policy and
                    // permission rules before the frozen world answers. Without this, a `what_if`
                    // that tightened the policy would still be served the recorded output and report
                    // no change at all — the taped answer would mask the very denial the caller is
                    // asking about. `Executor::authorize` is the deterministic-gates-only entry: it
                    // cannot execute (it isn't `async`), never prompts, and records nothing.
                    if frozen.reauthorize() {
                        let verdict = self.executor.authorize(op, &input);
                        if let Some(reason) = verdict.reason() {
                            frozen.note_policy_denial(reason);
                            return OpOutcome {
                                denied: true,
                                timing: None,
                                content: reason.to_string(),
                                view: None,
                                is_error: true,
                            };
                        }
                    }
                    match frozen.serve(op, &input_json) {
                        crate::cassette::ScopeServe::Served(out) => return out,
                        crate::cassette::ScopeServe::Refused(reason) => {
                            return cassette_refused(&reason)
                        }
                        crate::cassette::ScopeServe::Miss => {}
                    }
                }
                crate::cassette::CassetteScope::Resume(resume) => {
                    match resume.serve(op, &input_json) {
                        crate::cassette::ScopeServe::Served(out) => return out,
                        crate::cassette::ScopeServe::Refused(reason) => {
                            return cassette_refused(&reason)
                        }
                        crate::cassette::ScopeServe::Miss => {}
                    }
                }
                // `Record` never serves — every dispatch is live, recorded on the way out.
                crate::cassette::CassetteScope::Record(_) => {}
            }
        }
        // L-32: `denied` is read straight off the envelope's own structural flag
        // (`DispatchOutcome::denied`, set at the exact call site inside `Executor::dispatch_outcome`
        // that refuses the call) rather than inferred here by prefix-matching `content` against the
        // envelope's refusal wording. The interpreter turns a denied outcome into the fatal
        // `FlowError::Denied`, so `retry`/`loop`/composites never re-attempt a deliberate refusal
        // (L-21) — but an op that merely *ran* and relayed foreign text shaped like that wording
        // (e.g. a wrapped CLI's own "denied by" stderr) must NOT be misclassified as one.
        let outcome = self.executor.dispatch_outcome(op, input).await;
        let out = OpOutcome {
            denied: outcome.denied,
            timing: Some(outcome.timing),
            content: outcome.result.content,
            view: outcome.result.view,
            is_error: outcome.result.is_error,
        };
        self.tail_record(op, &input_json, &out);
        out
    }

    fn catalog(&self) -> &dyn OpCatalog {
        &self.catalog
    }

    async fn request_approval(
        &self,
        label: &str,
        intents: &IntentSet,
    ) -> flux_lang::host::ApprovalChoice {
        // D-175 per-arm table: `Replay` and `Frozen(Halt)` never dispatch anything live — their
        // recorded run (or the frozen world) already passed its `confirm` gates, so hermetic replay
        // needs no interactive approver. `Record`, `Frozen(Live)`, and `Resume` can all reach a real
        // live dispatch (the bridge/tail), so they MUST gate through the real approver — an adversarial
        // deny-approver has to be able to deny a live-bridge or crash-tail call exactly like a normal
        // live turn would.
        if let Some(scope) = &self.cassette {
            let auto_allow = match scope.as_ref() {
                crate::cassette::CassetteScope::Replay(_) => true,
                crate::cassette::CassetteScope::Frozen(frozen) => {
                    matches!(frozen.off_tape(), crate::cassette::OffTape::Halt)
                }
                crate::cassette::CassetteScope::Record(_)
                | crate::cassette::CassetteScope::Resume(_) => false,
            };
            if auto_allow {
                return flux_lang::host::ApprovalChoice::Allow;
            }
        }
        let subjects = [label.to_string()];
        let choice = self
            .executor
            .approver()
            .request("confirm", &subjects, intents)
            .await;
        // `AllowAlways` is an approval too (the user chose "allow & remember"); exhaustive match so a
        // new `ApprovalChoice` variant forces a decision here rather than silently mapping to `Deny`.
        match choice {
            ApprovalChoice::Allow | ApprovalChoice::AllowAlways(_) => {
                flux_lang::host::ApprovalChoice::Allow
            }
            // The flux-lang host choice is binary — a C-113 reason has nowhere to ride here and
            // is deliberately dropped at this boundary.
            ApprovalChoice::Deny | ApprovalChoice::DenyWithReason(_) => {
                flux_lang::host::ApprovalChoice::Deny
            }
        }
    }

    fn trim_output(&self, view: String, op: &str) -> String {
        flux_runtime::trim_tool_output(view, flux_runtime::tool_output_cap(), op)
    }

    async fn push_cap_scope(&self, tools: &[String]) {
        // Forwards straight to the executor's own stack — the SAME `Arc` `Executor::dispatch` checks,
        // so pushing here immediately narrows every dispatch from this point on (including a nested
        // composite op or a sub-agent reading the same context). Holding the guard is just bookkeeping
        // to defer the pop to the matching `pop_cap_scope` call.
        let guard = self.executor.push_cap_scope(tools);
        self.cap_scope_guards.lock().unwrap().push(guard);
    }

    async fn pop_cap_scope(&self) {
        // Drop the innermost guard — its `Drop` pops the executor's stack. A missing push (a bug in
        // the caller) is a silent no-op rather than a panic, matching `pop_cap_scope`'s "close the
        // innermost scope" contract when there happens to be none open.
        self.cap_scope_guards.lock().unwrap().pop();
    }
}

/// Bridges the interpreter's [`FlowSink`] back onto the engine's [`AgentSink`].
struct SinkBridge<'a> {
    inner: &'a mut dyn AgentSink,
    /// Structural-trace opt-in (A-39) — forwarded to [`FlowSink::trace_structural`]. Only the
    /// engine's OUTER `execute_flow_traced` call ever sets this; nested authored flows and resume
    /// stay `false`.
    trace: bool,
}

impl FlowSink for SinkBridge<'_> {
    fn text_delta(&mut self, text: &str) {
        self.inner.text_delta(text);
    }
    fn thinking_delta(&mut self, text: &str) {
        self.inner.thinking_delta(text);
    }
    fn planning(&mut self, active: bool) {
        self.inner.planning(active);
    }
    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.inner.tool_call(name, input);
    }
    fn tool_result(&mut self, name: &str, result: &OpOutcome) {
        if let Some(timing) = result.timing.as_ref() {
            self.inner.tool_timing(name, timing);
        }
        self.inner.tool_result(
            name,
            &ToolResult {
                content: result.content.clone(),
                view: result.view.clone(),
                is_error: result.is_error,
            },
        );
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.inner.observation(o);
    }
    fn turn_end(&mut self, usage: Option<flux_core::Usage>) {
        self.inner.turn_end(usage);
    }
    fn trace_structural(&self) -> bool {
        self.trace
    }
}

// ---------------------------------------------------------------------------
// Thin wrappers: original signatures, delegating to the interpreter
// ---------------------------------------------------------------------------

/// Execute one registered operation through the envelope, storing the result and (optionally) binding
/// a symbol — the original signature, delegating to [`flux_lang::runtime::execute_call`].
pub async fn execute_call(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    op: &str,
    input: serde_json::Value,
    bind: Option<BindSpec<'_>>,
) -> Result<CallOutcome> {
    let host = ExecutorHost::new(executor);
    flux_lang::runtime::execute_call(store, &host, session_id, op, input, bind).await
}

/// Execute a compiled flow — the original signature, delegating to [`flux_lang::runtime::execute_flow`]
/// with the engine's executor/sink adapted onto the interpreter's traits. Structural tracing is off
/// (see [`execute_flow_traced`] for the opt-in entry the engine's OUTER loop uses).
pub async fn execute_flow(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    ast: &DraftAst,
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    execute_flow_traced(store, executor, session_id, ast, sink, false).await
}

/// The **structural-trace-aware** analog of [`execute_flow`] (A-39): when `trace_structural` is
/// `true`, the interpreter emits live-only `loop.round`/`loop.node` observations (never persisted —
/// see [`flux_lang::runtime`]'s trace helper) through this same sink. Scoping is by call site: the
/// engine's outer agent-loop call is the ONLY caller that ever passes `true` (from `trace_loop()`
/// reading `FLUX_TRACE_LOOP`/`--trace-loop`); nested `flow run` and resume paths keep calling plain
/// [`execute_flow`] (`false`), so only the OUTER loop's structure is ever traced.
pub async fn execute_flow_traced(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    ast: &DraftAst,
    sink: &mut dyn AgentSink,
    trace_structural: bool,
) -> Result<FlowOutcome> {
    let host = ExecutorHost::new(executor);
    let mut bridge = SinkBridge {
        inner: sink,
        trace: trace_structural,
    };
    flux_lang::runtime::execute_flow(store, &host, session_id, ast, &mut bridge).await
}

/// Execute a compiled flow with module-local composite ops installed in the operation catalog.
pub async fn execute_flow_with_composites(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    ast: &DraftAst,
    composites: &[CompositeOpDecl],
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    // A-59 / F-016: a direct composite flow supplies its own session lineage; when nested inside a
    // live turn, preserve that real parent session together with its cancel/reporter. Scope the
    // result lexically so a reused executor never retains this run after it returns.
    let runtime_turn = executor.context().runtime_turn_context();
    let runtime_turn = if runtime_turn.session_id().is_some() {
        runtime_turn
    } else {
        runtime_turn.with_session(session_id)
    };
    let mut host = ExecutorHost::new_with_composites(executor, composites);
    // C-43: plan execution self-wires the store's active cassette scope (record or replay).
    host.cassette = store.cassette();
    let mut bridge = SinkBridge {
        inner: sink,
        trace: false,
    };
    scope_runtime_turn(
        runtime_turn,
        flux_lang::runtime::execute_flow(store, &host, session_id, ast, &mut bridge),
    )
    .await
}

/// The **resumable** analog of [`execute_flow_with_composites`] — the shared entry point for
/// authored `flux flow run --resumable`/`--resume`, action-batch repair, replay, and fork. A failing
/// TOP-LEVEL statement is reified onto
/// `FlowOutcome::failure` instead of propagating `Err`; `ledger`, when given (folded via
/// [`FlowStore::open_halted_plan`](crate::state::FlowStore::open_halted_plan) over the session's
/// run-event log), fast-forwards the longest content-hash-matching completed prefix before executing
/// from the first divergence.
pub async fn execute_flow_resumable_with_composites(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    ast: &DraftAst,
    composites: &[CompositeOpDecl],
    ledger: Option<&ResumeLedger>,
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    // Same lexical lineage boundary as `execute_flow_with_composites`: retain an outer cancel,
    // parent session and reporter; otherwise supply this direct run's session; always restore.
    let runtime_turn = executor.context().runtime_turn_context();
    let runtime_turn = if runtime_turn.session_id().is_some() {
        runtime_turn
    } else {
        runtime_turn.with_session(session_id)
    };
    let mut host = ExecutorHost::new_with_composites(executor, composites);
    // C-43: authored execution self-wires the store's active cassette scope (record or replay).
    // The outer agent-loop path (`execute_flow_traced`) stays unwired because replay drives its
    // recorded host-derived action flows directly.
    host.cassette = store.cassette();
    let mut bridge = SinkBridge {
        inner: sink,
        trace: false,
    };
    scope_runtime_turn(
        runtime_turn,
        flux_lang::runtime::execute_flow_resumable(
            store,
            &host,
            session_id,
            ast,
            &mut bridge,
            ledger,
        ),
    )
    .await
}

/// A **denied-statement resume guard** for authored `flux flow run --resume`:
/// once policy or the user has refused a statement (`Denied`/`ConfirmDenied`), that EXACT statement
/// must never be silently re-dispatched just because it re-appears unchanged in a corrected source
/// file — the caller must see the refusal again, not a silent retry. Returns
/// `true` when `halt` was a denial AND `body` still contains a statement with `halt`'s exact
/// `stmt_hash16` anywhere (mirroring the loop host's own scan, which is position-independent so a
/// reordered-but-unchanged statement is still caught); the caller should refuse to execute rather
/// than fast-forward at all. A genuinely edited statement (any change to its content) has a
/// different hash and is unaffected — it flows through fast-forward normally, exactly like any other
/// correction.
pub fn denied_resume_guard(body: &[Node], halt: &PlanHalt) -> bool {
    matches!(halt.kind, FailureKind::Denied | FailureKind::ConfirmDenied)
        && body.iter().any(|n| stmt_hash16(n) == halt.stmt)
}

/// Render a resumable-mode halt for a **human-facing** surface (`flux flow run --resumable`/
/// `--resume`, L-25) — a ✓/✗/· marked statement tree plus a machine-readable failure summary and
/// the session id needed to correct-and-continue. Reuses [`crate::render::render_statement`] and
/// [`crate::loop_host::failure_kind_label`] so halt vocabulary stays consistent.
pub fn render_halt_report(ast: &DraftAst, halt: &PlanHalt, session_id: &str) -> String {
    let marked = ast
        .body
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let marker = match i as u32 {
                n if n == halt.node.0 => "\u{2717}", // ✗
                n if n < halt.node.0 => "\u{2713}",  // ✓
                _ => "\u{b7}",                       // ·
            };
            format!(
                "{marker} {i}: {}",
                crate::render::render_statement(node, &crate::render::Palette::PLAIN)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let failure = serde_json::json!({
        "node": halt.node.0,
        "stmt": halt.stmt,
        "op": halt.op,
        "kind": crate::loop_host::failure_kind_label(halt.kind),
        "fatal": halt.kind.is_fatal(),
        "message": halt.message,
        "plan": halt.plan,
        "session": session_id,
    });
    format!(
        "{marked}\n\n[flow halted at step {} of {}] {}\n{}\n\n\
Correct the flow and continue: `flux flow run <file> --resume {session_id}` (or `--resume last`).",
        halt.node.0 + 1,
        ast.body.len(),
        halt.message,
        serde_json::to_string(&failure).unwrap_or_default(),
    )
}

/// Resume a flow suspended on a top-level `await` — the engine wrapper over
/// [`flux_lang::runtime::resume_flow`]. Binds `input` to the suspended `await` at `at` and continues
/// from the next statement (the prefix is not re-run); the flow may suspend again or complete.
pub async fn resume_flow(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    body: &[flux_lang::ast::Node],
    at: flux_lang::ast::NodeId,
    input: flux_lang::ast::Value,
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    let host = ExecutorHost::new(executor);
    let mut bridge = SinkBridge {
        inner: sink,
        trace: false,
    };
    flux_lang::runtime::resume_flow(store, &host, session_id, body, at, input, &mut bridge).await
}

/// Resume a flow with composite ops installed in the operation catalog. `name` is the flow's
/// declared name as persisted with the suspension (see `FlowStore::save_suspension`), threaded into
/// [`flux_lang::runtime::resume_flow_named`] so a **named** flow's resumed run records its
/// checkpoints under the same `flow_key` (name + body hash) the original run used — run and resume
/// agree on keys (L-21; the flux-lang derivation shipped with F17, this is the engine wiring).
#[allow(clippy::too_many_arguments)]
pub async fn resume_flow_with_composites(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    name: Option<&str>,
    body: &[flux_lang::ast::Node],
    at: flux_lang::ast::NodeId,
    input: flux_lang::ast::Value,
    composites: &[CompositeOpDecl],
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    let host = ExecutorHost::new_with_composites(executor, composites);
    let mut bridge = SinkBridge {
        inner: sink,
        trace: false,
    };
    flux_lang::runtime::resume_flow_named(
        store,
        &host,
        session_id,
        name,
        body,
        at,
        input,
        &mut bridge,
    )
    .await
}

/// Execute an optimizer [`flux_lang::ast::PhysicalPlan`] over a flow's top-level `body` — the engine
/// wrapper over [`flux_lang::runtime::execute_plan`], adapting the executor/sink onto the
/// interpreter's traits exactly as [`execute_flow`] does.
pub async fn execute_plan(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    body: &[flux_lang::ast::Node],
    plan: &flux_lang::ast::PhysicalPlan,
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    let host = ExecutorHost::new(executor);
    let mut bridge = SinkBridge {
        inner: sink,
        trace: false,
    };
    flux_lang::runtime::execute_plan(store, &host, session_id, body, plan, &mut bridge).await
}

/// Execute an optimizer plan with module-local composite ops installed in the operation catalog.
pub async fn execute_plan_with_composites(
    store: &FlowStore,
    executor: &Executor,
    session_id: &str,
    body: &[flux_lang::ast::Node],
    plan: &flux_lang::ast::PhysicalPlan,
    composites: &[CompositeOpDecl],
    sink: &mut dyn AgentSink,
) -> Result<FlowOutcome> {
    let host = ExecutorHost::new_with_composites(executor, composites);
    let mut bridge = SinkBridge {
        inner: sink,
        trace: false,
    };
    flux_lang::runtime::execute_plan(store, &host, session_id, body, plan, &mut bridge).await
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
    /// True if any op is destructive-shaped — surfaced in the plan's risk summary so the user sees it
    /// before approving the plan as a whole (an approved plan then runs in full, no per-op re-confirm).
    pub destructive: bool,
    /// True if any op writes / executes / connects out.
    pub mutating: bool,
    /// The distinct op names the plan calls, in first-seen order.
    pub ops: Vec<String>,
    /// Aggregate statically-visible intents across the plan's op calls — the same pre-execution
    /// signal the per-op gate sees, so a plan-level approver (notably the sub-agent one) can apply
    /// its per-op policy to the plan as a unit. Only literal args contribute; a `$symbol` arg is
    /// invisible here, which is why dispatch re-fires the gate for an undisclosed destructive op.
    pub intents: flux_spec::IntentSet,
    /// Exact invocation requirements derived through the same tool contract dispatch evaluates.
    pub requirements: Vec<flux_runtime::AuthorityRequirement>,
}

impl PlanRisk {
    /// The request handed to plan approval ([`Executor::approve_plan`](flux_runtime::Executor)) —
    /// the whole-plan prompt decides on exactly this preview.
    pub fn approval_request(&self) -> flux_runtime::PlanApprovalRequest {
        flux_runtime::PlanApprovalRequest {
            summary: self.summary(),
            ops: self.ops.clone(),
            destructive: self.destructive,
            mutating: self.mutating,
            intents: self.intents.clone(),
            requirements: self.requirements.clone(),
        }
    }

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
            format!("{base} · contains a destructive operation")
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
        let input = literal_input(args, &spec.input_schema);
        let subjects = tool.permission_subjects(&input);
        match tool.authority_requirements(&input, &subjects) {
            Ok(requirements) => apply_requirements(&mut risk, requirements),
            Err(_) => {
                // An invalid authority declaration is refused at dispatch. Preview it as
                // destructive as well so aggregate approval can never make it look harmless.
                risk.destructive = true;
                risk.mutating = true;
            }
        }
        let intents = tool.intents(&input);
        if intents.is_destructive() {
            risk.destructive = true;
        }
        if intents.is_mutating() {
            risk.mutating = true;
        }
        risk.intents.intents.extend(intents.intents);
    });
    risk
}

/// Like [`plan_risk`], but expands module-local composite ops so the preview includes their inner
/// calls and transitive declared risk/effects.
pub fn plan_risk_with_composites(
    ast: &DraftAst,
    registry: &ToolRegistry,
    composites: &[CompositeOpDecl],
) -> PlanRisk {
    let mut risk = PlanRisk::default();
    accumulate_risk(&ast.body, registry, composites, &mut Vec::new(), &mut risk);
    risk
}

fn accumulate_risk(
    body: &[Node],
    registry: &ToolRegistry,
    composites: &[CompositeOpDecl],
    stack: &mut Vec<String>,
    risk: &mut PlanRisk,
) {
    walk_calls(body, &mut |op, args| {
        if !risk.ops.iter().any(|o| o == op) {
            risk.ops.push(op.to_string());
        }
        if let Some(tool) = registry.get(op) {
            let spec = tool.spec();
            apply_risk(risk, spec.risk);
            let input = literal_input(args, &spec.input_schema);
            let subjects = tool.permission_subjects(&input);
            match tool.authority_requirements(&input, &subjects) {
                Ok(requirements) => apply_requirements(risk, requirements),
                Err(_) => {
                    risk.destructive = true;
                    risk.mutating = true;
                }
            }
            let intents = tool.intents(&input);
            if intents.is_destructive() {
                risk.destructive = true;
            }
            if intents.is_mutating() {
                risk.mutating = true;
            }
            risk.intents.intents.extend(intents.intents);
            return;
        }
        let Some(composite) = composites.iter().find(|c| c.name == op) else {
            return;
        };
        apply_risk(risk, composite.meta.risk);
        if composite.meta.risk == Risk::Destructive {
            risk.destructive = true;
        }
        if composite
            .meta
            .effects
            .iter()
            .any(|e| !matches!(e, flux_spec::Effect::Read))
        {
            risk.mutating = true;
        }
        if stack.contains(&composite.name) {
            return;
        }
        stack.push(composite.name.clone());
        accumulate_risk(&composite.body.body, registry, composites, stack, risk);
        stack.pop();
    });
}

fn apply_risk(risk: &mut PlanRisk, next: Risk) {
    risk.max_risk = Some(match risk.max_risk {
        Some(r) => r.max(next),
        None => next,
    });
}

fn apply_requirements(risk: &mut PlanRisk, requirements: Vec<flux_runtime::AuthorityRequirement>) {
    for requirement in requirements {
        if requirement.is_mutating() {
            risk.mutating = true;
        }
        if requirement.is_destructive() {
            risk.destructive = true;
        }
        if !risk.requirements.contains(&requirement) {
            risk.requirements.push(requirement);
        }
    }
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
        Node::Verify { cmd, expect, .. } => {
            walk_node(cmd, f);
            walk_node(expect, f);
        }
        Node::Peek { .. } => {}
        Node::Expr { vars, .. } => {
            for v in vars.values() {
                walk_node(v, f);
            }
        }
        Node::Fmt { .. } => {}
        Node::Jq { input, .. } => walk_node(input, f),
        Node::Match {
            subject,
            cases,
            default,
        } => {
            walk_node(subject, f);
            for c in cases {
                walk_node(&c.value, f);
                walk_calls(&c.body, f);
            }
            walk_calls(default, f);
        }
        Node::Route {
            selector,
            cases,
            default,
        } => {
            walk_node(selector, f);
            for c in cases {
                walk_calls(&c.body, f);
            }
            walk_calls(default, f);
        }
        Node::Fallback { branches, .. } => {
            for b in branches {
                walk_calls(&b.body, f);
            }
        }
        Node::Timeout { body, .. } | Node::Budget { body, .. } | Node::CapScope { body, .. } => {
            walk_calls(body, f)
        }
        Node::Scope {
            acquire,
            body,
            finally,
            ..
        } => {
            if let Some(acq) = acquire {
                walk_node(acq, f);
            }
            walk_calls(body, f);
            walk_calls(finally, f);
        }
        Node::Saga { steps } => {
            for step in steps {
                walk_calls(&step.body, f);
                walk_calls(&step.undo, f);
            }
        }
        Node::Once { body, .. } => walk_calls(body, f),
        Node::Obj { fields } => {
            for v in fields.values() {
                walk_node(v, f);
            }
        }
        Node::List { items } => {
            for it in items {
                walk_node(it, f);
            }
        }
        Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::Await { .. }
        | Node::Checkpoint { .. }
        | Node::Parse { .. }
        | Node::Ctx { .. }
        | Node::CtxAppend { .. } => {}
    }
}

/// Build a best-effort named input from a call's *literal* args only (for intent preview); non-literal
/// args (`$symbols`) are skipped and arity is not enforced. Under named-args semantics: a lone
/// object literal is the named input; a single bare literal binds to the sole param; 2+ bare literals
/// fall back to catalog-order binding (the deprecated positional form).
fn literal_input(args: &[Node], schema: &serde_json::Value) -> serde_json::Value {
    if let [Node::Lit { value }] = args {
        if value.is_object() {
            return value.clone();
        }
    }
    let (required, optional) = schema_params(schema);
    let n_params = required.len() + optional.len();
    // Single bare literal: bind to the sole required param (`read("x")` sugar) when there's
    // exactly one required, else the sole param; untyped (n_params==0) passes the value through.
    if args.len() == 1 {
        if let [Node::Lit { value }] = args {
            if n_params == 0 {
                return value.clone();
            }
            let pname = if required.len() == 1 {
                required.first().cloned()
            } else {
                required.first().or(optional.first()).cloned()
            };
            if let Some(name) = pname {
                let mut input = serde_json::Map::new();
                input.insert(name, value.clone());
                return serde_json::Value::Object(input);
            }
        }
    }
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
    use crate::ast::{NodeId, RunEvent, SagaStep, SymbolName, Value, Visibility};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;

    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

    /// Unique-directory suffix for tests that need their own isolated workspace (not shared with
    /// `temp_executor`'s fixed per-allow-flag directory).
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
        ) -> flux_core::Result<ToolResult> {
            Ok(ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// A second read-only tool, distinct from `echo` — used to prove capability-scope narrowing (one
    /// tool allowed inside a `with_tools` block, the other denied purely by the scope).
    struct GrepTool;

    #[async_trait]
    impl Tool for GrepTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("grep", "grep text", json!({"type": "object"}))
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            Ok(ToolResult::ok("match"))
        }
    }

    /// Declares its mutation only in the operation contract. Plan approval must not require every
    /// integration to duplicate `Effect::Write` as a concrete intent tag.
    struct EffectOnlyWriteTool;

    #[async_trait]
    impl Tool for EffectOnlyWriteTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("effect_write", "write", json!({"type": "object"}))
                .with_effects(vec![
                    flux_spec::Effect::Write,
                    flux_spec::Effect::Filesystem,
                ])
                .with_access(vec![flux_spec::AccessKind::Filesystem])
        }

        fn permission_subjects(&self, _params: &serde_json::Value) -> Vec<String> {
            vec!["out.txt".to_string()]
        }

        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            Ok(ToolResult::ok("wrote"))
        }
    }

    struct SlowTool;

    #[async_trait]
    impl Tool for SlowTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only("slow", "slow read", json!({"type": "object"}))
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(ToolResult::ok("slow"))
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
        ) -> flux_core::Result<ToolResult> {
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

    /// An executor with BOTH `echo` and `grep` registered and explicitly allowed by the permission
    /// rules (no policy floor either) — so the only thing that can ever deny a call in the tests below
    /// is an active `with_tools` capability scope, never the outer session's own permissiveness.
    fn temp_executor_two_tools() -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-capscope-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(GrepTool));
        let perms = PermissionManager::from_rules(&["echo".into(), "grep".into()], &[]);
        Executor::new(
            reg,
            perms,
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
    }

    fn temp_executor_with_slow_tool() -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-cancel-scope-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(SlowTool));
        let perms = PermissionManager::from_rules(&["echo".into(), "slow".into()], &[]);
        Executor::new(
            reg,
            perms,
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
    }

    fn temp_executor_with_builtins_and_echo() -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-builtins-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_builtins(&mut reg);
        let perms = PermissionManager::from_rules(&["echo".into()], &[]);
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

    /// A sink that records the op names it was told about, and (A-39) the `kind` of every
    /// observation it receives — used by the structural-trace gating test to check for `loop.*`
    /// observations without depending on any one field shape.
    #[derive(Default)]
    struct CollectSink {
        calls: Vec<String>,
        observations: Vec<String>,
        timings: Vec<(String, flux_core::OperationTiming)>,
    }
    impl AgentSink for CollectSink {
        fn tool_call(&mut self, name: &str, _input: &serde_json::Value) {
            self.calls.push(name.to_string());
        }
        fn observation(&mut self, o: &flux_evidence::Observation) {
            self.observations.push(o.kind.clone());
        }
        fn tool_timing(&mut self, name: &str, timing: &flux_core::OperationTiming) {
            self.timings.push((name.to_string(), *timing));
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

    /// C-270: the engine runs a plan against a **non-SQLite** implementation of the state port and
    /// observes the same thing it does against the SQLite one — the whole point of the port. The
    /// store is built with [`FlowStore::with_backend`] over [`crate::state::MemoryState`], so no
    /// `rusqlite` handle exists anywhere on the state path.
    #[tokio::test]
    async fn execute_flow_runs_a_linear_plan_over_a_non_sqlite_state_backend() {
        let store = FlowStore::with_backend(
            Arc::new(crate::state::MemoryState::default()),
            Arc::new(flux_events::EventStore::in_memory().unwrap()),
        );
        let ex = temp_executor(true);
        // $a = echo("hi"); $b = echo($a); return $b — the same plan the SQLite-backed
        // `execute_flow_runs_a_linear_plan_through_dispatch` above drives.
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
        // The durable state the engine actually relies on landed in the portable backend: both
        // symbols resolve, and the values they point at are readable back.
        for name in ["a", "b"] {
            let vid = store
                .resolve("sess", &SymbolName(name.into()))
                .unwrap()
                .unwrap_or_else(|| panic!("${name} bound in the portable backend"));
            assert_eq!(
                store.get_value(&vid).unwrap(),
                Some(Value::String("hi".into())),
                "${name}'s value reads back from the portable backend"
            );
        }
        assert!(
            store.total_value_bytes("sess").unwrap() > 0,
            "byte accounting works off the port, not off SQLite's SUM()"
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
        assert_eq!(sink.timings.len(), 2, "each live dispatch exposes timing");
        assert!(sink.timings[0].1.execution_us.is_some());
        assert_eq!(
            sink.timings[1].1.execution_us, None,
            "the second identical read was a cache hit, not a fresh execution"
        );
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
    async fn execute_flow_with_composites_scopes_the_session_for_subagent_correlation() {
        // A-59 / F-016 + A-80: the direct flow-run entry supplies the run's session while its ops
        // execute, so `task(...)` can correlate a child, then restores the reusable context.
        struct SessionProbe(Arc<Mutex<Option<String>>>);
        #[async_trait]
        impl Tool for SessionProbe {
            fn spec(&self) -> ToolSpec {
                ToolSpec::read_only(
                    "session_probe",
                    "capture session",
                    json!({"type": "object"}),
                )
            }

            async fn execute(
                &self,
                ctx: &ToolContext,
                _params: serde_json::Value,
            ) -> flux_core::Result<ToolResult> {
                *self.0.lock().unwrap() = ctx.session_id();
                Ok(ToolResult::ok("ok"))
            }
        }

        let store = FlowStore::in_memory().unwrap();
        let seen = Arc::new(Mutex::new(None));
        let dir =
            std::env::temp_dir().join(format!("flux-flow-session-scope-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(SessionProbe(seen.clone())));
        let ex = Executor::new(
            registry,
            PermissionManager::from_rules(&["session_probe".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        assert_eq!(
            ex.context().session_id(),
            None,
            "a fresh executor context has no session"
        );
        let ast = DraftAst {
            body: vec![Node::Call {
                op: "session_probe".into(),
                args: Vec::new(),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        execute_flow_with_composites(&store, &ex, "sess-123", &ast, &[], &mut sink)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().as_deref(),
            Some("sess-123"),
            "ops see the direct run session for task() correlation"
        );
        assert_eq!(
            ex.context().session_id(),
            None,
            "the direct run must not leave obsolete lineage on a reusable executor"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// A-39: `execute_flow` never emits structural-trace `loop.*` observations (the outer loop opts
    /// in explicitly); `execute_flow_traced(..., true)` does — pinning the `SinkBridge` seam the
    /// engine's outer call uses to scope tracing to the OUTER loop only.
    #[tokio::test]
    async fn execute_flow_traced_gates_structural_observations() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 1,
                until: None,
                body: vec![],
                collect: None,
            }],
            ..Default::default()
        };

        let mut plain = CollectSink::default();
        execute_flow(&store, &ex, "sess", &ast, &mut plain)
            .await
            .unwrap();
        assert!(
            plain.observations.iter().all(|k| !k.starts_with("loop.")),
            "execute_flow must never emit structural-trace observations: {:?}",
            plain.observations
        );

        let mut traced = CollectSink::default();
        execute_flow_traced(&store, &ex, "sess", &ast, &mut traced, true)
            .await
            .unwrap();
        assert!(
            traced.observations.iter().any(|k| k == "loop.round"),
            "execute_flow_traced(..., true) must emit loop.round: {:?}",
            traced.observations
        );
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

    /// C-10: a `glob` result is a real LIST value — `each` iterates it and `merge` concatenates
    /// it. This exact composition failed live when `glob`
    /// bound a newline-joined string ("merge: element 0 of `lists` is not an array").
    #[tokio::test]
    async fn glob_results_compose_with_each_and_merge() {
        let dir = std::env::temp_dir().join(format!("flux-flow-c10-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "alpha").unwrap();
        std::fs::write(dir.join("b.md"), "beta").unwrap();
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        reg.register(Arc::new(EchoTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["glob".into(), "merge".into(), "echo".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let store = FlowStore::in_memory().unwrap();
        let var = |n: &str| Node::Var {
            name: SymbolName(n.into()),
        };
        let ast = DraftAst {
            body: vec![
                // $files = glob("*.md")
                flow_bind("files", "glob", vec![flow_lit(json!({"pattern": "*.md"}))]),
                // $all = merge({ lists: [$files, $files] })
                Node::Bind {
                    name: SymbolName("all".into()),
                    ty: None,
                    effect: None,
                    value: Box::new(Node::Call {
                        op: "merge".into(),
                        args: vec![Node::Obj {
                            fields: [(
                                "lists".to_string(),
                                Box::new(Node::List {
                                    items: vec![var("files"), var("files")],
                                }),
                            )]
                            .into_iter()
                            .collect(),
                        }],
                    }),
                },
                // each $f in $files { echo($f) } collect $names
                Node::Each {
                    source: Box::new(var("files")),
                    item: SymbolName("f".into()),
                    body: vec![flow_bind("x", "echo", vec![var("f")])],
                    collect: Some(SymbolName("names".into())),
                    flat: false,
                },
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .expect("glob → merge/each must compose");
        assert!(outcome.steps >= 4, "glob + merge + 2 echo iterations");
        // $all = the two glob lists concatenated (4 entries).
        let vid = store
            .resolve("sess", &SymbolName("all".into()))
            .unwrap()
            .expect("$all bound");
        // Values are stored as JSON strings (the store quirk) — consumers re-parse string leaves,
        // so the assertion does the same.
        let merged = match store.get_value(&vid).unwrap().unwrap().to_json() {
            serde_json::Value::String(s) => {
                serde_json::from_str::<serde_json::Value>(&s).expect("merge content is JSON")
            }
            other => other,
        };
        let arr = merged.as_array().expect("merge produced a JSON array");
        assert_eq!(arr.len(), 4, "2 files × 2 lists: {arr:?}");
    }

    #[tokio::test]
    async fn execute_flow_transcript_marks_silent_successes() {
        // A-05: an op succeeding with EMPTY output must read as an explicit success in the round
        // feedback — a blank entry is indistinguishable from "nothing happened", and the model
        // re-runs the (possibly non-idempotent) op.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![flow_bind("quiet", "echo", vec![flow_lit(json!(""))])],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert!(
            outcome
                .transcript
                .contains("[$quiet = echo]\n✓ ok (no output)"),
            "an empty view must render a success marker, got: {:?}",
            outcome.transcript
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
    async fn any_in_when_gates_a_flow_step() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor_with_builtins_and_echo();
        let ast = DraftAst {
            body: vec![Node::When {
                cond: Box::new(Node::Call {
                    op: "any".into(),
                    args: vec![flow_lit(json!({
                        "items": [{"score": 1}, {"score": 9}],
                        "where": "it.score > 5"
                    }))],
                }),
                then: vec![flow_bind("taken", "echo", vec![flow_lit(json!("yes"))])],
                otherwise: vec![flow_bind("taken", "echo", vec![flow_lit(json!("no"))])],
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
            Some(Value::String("yes".into()))
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
                collect: None,
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
                collect: None,
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
    async fn execute_flow_repeat_collects_each_iterations_result() {
        // repeat max 3 { $x = echo("hi") } collect $all → $all is the ordered list of results.
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 3,
                until: None,
                body: vec![flow_bind("x", "echo", vec![flow_lit(json!("hi"))])],
                collect: Some(SymbolName("all".into())),
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.steps, 3, "repeat runs exactly max times");
        // `collect` bound a list of every iteration's last result, in order.
        let vid = store
            .resolve("sess", &SymbolName("all".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            store.get_value(&vid).unwrap(),
            Some(Value::List(vec![
                Value::String("hi".into()),
                Value::String("hi".into()),
                Value::String("hi".into()),
            ]))
        );
    }

    #[tokio::test]
    async fn execute_flow_suspends_on_a_top_level_await_and_resumes() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            body: vec![Node::Await {
                binding: Some(crate::ast::SymbolName("x".into())),
                source: "input".into(),
                as_type: None,
                condition: None,
            }],
            ..Default::default()
        };

        // A top-level `await` now suspends the flow (it no longer errors).
        let mut sink = CollectSink::default();
        let out = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        let susp = out.suspension.expect("a top-level await suspends the flow");
        assert_eq!(susp.source, "input");
        assert_eq!(susp.node, crate::ast::NodeId(0));

        // resume_flow binds the awaited value and completes.
        let mut sink2 = CollectSink::default();
        let out2 = resume_flow(
            &store,
            &ex,
            "sess",
            &ast.body,
            susp.node,
            Value::String("hi".into()),
            &mut sink2,
        )
        .await
        .unwrap();
        assert!(out2.suspension.is_none());
        let x = store
            .resolve("sess", &crate::ast::SymbolName("x".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap());
        assert_eq!(x, Some(Value::String("hi".into())));
    }

    /// L-21: a **named** flow resumed through the engine path must record its checkpoints under the
    /// SAME `flow_key` (name + body hash) the original run used. Run → suspend at `await` → persist
    /// (name included) → take → resume named: the checkpoint reached *after* the await must
    /// fast-forward a subsequent fresh run of the same named flow past the await entirely. Before
    /// the fix the resume derived its key hash-only, so the fresh run only saw the pre-await
    /// checkpoint and re-suspended at the await.
    #[tokio::test]
    async fn named_flow_resume_uses_the_same_checkpoint_key_as_the_run() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(true);
        let ast = DraftAst {
            name: Some("wf".into()),
            body: vec![
                flow_bind("a", "echo", vec![flow_lit(json!("one"))]),
                Node::Checkpoint { label: "p1".into() },
                Node::Await {
                    binding: Some(SymbolName("x".into())),
                    source: "user_input".into(),
                    as_type: None,
                    condition: None,
                },
                flow_bind("b", "echo", vec![flow_lit(json!("two"))]),
                Node::Checkpoint { label: "p2".into() },
                Node::Return {
                    value: Box::new(flow_var("b")),
                },
            ],
            ..Default::default()
        };

        // Run: checkpoint `p1` is recorded under the NAMED key; the flow suspends at the await.
        let mut sink = CollectSink::default();
        let out = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap();
        let susp = out
            .suspension
            .expect("the named flow suspends at the await");

        // Persist + take across the turn boundary, exactly as the engine does.
        store
            .save_suspension(
                "sess",
                ast.name.as_deref(),
                &ast.body,
                susp.node,
                &susp.source,
            )
            .unwrap();
        let (flow_name, body, node, _source) = store
            .take_suspension("sess")
            .unwrap()
            .expect("a suspension");
        assert_eq!(flow_name.as_deref(), Some("wf"));

        // Resume named: runs the post-await suffix and records checkpoint `p2`.
        let mut sink2 = CollectSink::default();
        let resumed = resume_flow_with_composites(
            &store,
            &ex,
            "sess",
            flow_name.as_deref(),
            &body,
            node,
            Value::String("go".into()),
            &[],
            &mut sink2,
        )
        .await
        .unwrap();
        assert!(resumed.suspension.is_none(), "the resume completes");

        // A fresh run of the SAME named flow fast-forwards past `p2` — proof the resume recorded it
        // under the run's own key. With mismatched keys it would only see `p1` and re-suspend.
        let mut sink3 = CollectSink::default();
        let rerun = execute_flow(&store, &ex, "sess", &ast, &mut sink3)
            .await
            .unwrap();
        assert!(
            rerun.suspension.is_none(),
            "run and resume agree on the checkpoint key — the fresh run must not re-suspend at the await"
        );
        assert!(
            sink3.calls.is_empty(),
            "everything before the last checkpoint is fast-forwarded, nothing re-dispatches: {:?}",
            sink3.calls
        );
        assert_eq!(rerun.result, "two", "the flow completes from the suffix");
    }

    /// L-21: an op the REAL executor's envelope denies (a permission-rule deny here — the same
    /// canonical `` `{op}` denied by … `` shape policy/capability-scope/user denials use) is
    /// classified `denied` at the flux-lang boundary and surfaces as the fatal `FlowError::Denied`:
    /// `retry` must not re-attempt it. This also pins the executor's structural denial contract
    /// (`DispatchOutcome::denied`, set by `Executor::dispatch_outcome` at the exact deny call site —
    /// L-32) that `ExecutorHost::dispatch` reads to fill `OpOutcome::denied`.
    #[tokio::test]
    async fn policy_denied_op_is_not_retried_inside_loop() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(false); // `echo` denied by permission rules
        let ast = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: None,
                delay_ms: Some(0),
                body: vec![Node::Loop {
                    for_ms: 10_000,
                    every_ms: 0,
                    until: None,
                    body: vec![flow_bind("x", "echo", vec![flow_lit(json!("hi"))])],
                    bind: None,
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.is_fatal(),
            "an envelope denial is fatal through loop+retry: {err}"
        );
        assert!(
            err.to_string().contains("denied by permission rules"),
            "the executor's denial message survives: {err}"
        );
        assert_eq!(
            sink.calls,
            vec!["echo"],
            "the denied op was attempted exactly once — never retried"
        );
    }

    /// L-32: an op that the envelope actually RAN — and which failed on its own, merely relaying
    /// foreign text shaped exactly like the envelope's own canonical denial wording (`` `bash`
    /// denied by `` — here a nested CLI's stderr) — must NOT be misclassified as an envelope denial.
    /// It is an ordinary repairable failure: `retry` must keep re-attempting it (unlike the genuine
    /// denial pinned by `policy_denied_op_is_not_retried_inside_loop`, which runs exactly once).
    /// Before the fix this was classified purely by a `content.starts_with(...)` prefix match, so
    /// `bash` surfacing that exact text as its own stderr killed the whole turn with a fatal,
    /// never-retried `FlowError::Denied`.
    #[tokio::test]
    async fn op_relaying_denial_shaped_text_is_repairable_not_fatal() {
        let store = FlowStore::in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-l32-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["bash".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let ast = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: None,
                delay_ms: Some(0),
                body: vec![Node::Loop {
                    for_ms: 10_000,
                    every_ms: 0,
                    until: None,
                    body: vec![flow_bind(
                        "x",
                        "bash",
                        vec![flow_lit(json!(
                            "echo '`bash` denied by nested-cli-policy' >&2; exit 1"
                        ))],
                    )],
                    bind: None,
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            !err.is_fatal(),
            "an op that ran and merely relayed denial-shaped text is repairable, not fatal: {err}"
        );
        assert!(
            err.to_string().contains("denied by nested-cli-policy"),
            "the op's own failure content survives: {err}"
        );
        assert_eq!(
            sink.calls,
            vec!["bash", "bash", "bash"],
            "a repairable (non-fatal) failure is retried up to `max`, unlike a genuine denial: {:?}",
            sink.calls
        );
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

    #[test]
    fn plan_risk_treats_declared_write_effect_as_mutating_without_intent_tag() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EffectOnlyWriteTool));
        let ast = DraftAst {
            body: vec![flow_bind("w", "effect_write", vec![flow_lit(json!({}))])],
            ..DraftAst::default()
        };

        let risk = plan_risk(&ast, &registry);

        assert!(risk.mutating);
        assert_eq!(risk.ops, vec!["effect_write"]);
        assert_eq!(risk.requirements.len(), 1);
        assert_eq!(risk.requirements[0].action.0, "workspace.write");
        assert_eq!(
            risk.requirements[0].resource.path.as_deref(),
            Some("out.txt")
        );
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
                flat: false,
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
                flat: false,
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
                    flat: false,
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
                body: vec![flow_bind(
                    "r",
                    "echo",
                    vec![flow_lit(json!("should not run"))],
                )],
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
    async fn execute_flow_confirm_allow_always_runs_body() {
        // "Allow & always" (`ApprovalChoice::AllowAlways`) is an approval — the confirm body must run.
        // Regression: the engine→language approval adapter once mapped `AllowAlways` to `Deny`.
        struct AllowAlwaysApprover;
        #[async_trait]
        impl Approver for AllowAlwaysApprover {
            async fn request(&self, _t: &str, _s: &[String], _i: &IntentSet) -> ApprovalChoice {
                ApprovalChoice::AllowAlways("confirm".into())
            }
        }
        let dir =
            std::env::temp_dir().join(format!("flux-flow-rt-{}-confirmalways", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let ex = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowAlwaysApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        let store = FlowStore::in_memory().unwrap();
        let ast = DraftAst {
            body: vec![Node::Confirm {
                message: "proceed?".into(),
                risk: Some("medium".into()),
                body: vec![flow_bind("r", "echo", vec![flow_lit(json!("did run"))])],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_confirm_always", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            outcome.result, "did run",
            "AllowAlways must run the confirm body"
        );
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
                name: "test_throttle_ok".to_string(),
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
                name: "test_throttle_limit".to_string(),
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
                name: "test_debounce".to_string(),
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

    // ---- capability scopes (`with_tools` / L-11) ----

    /// **The headline test.** A flow with `with_tools ["grep"] { grep(...) }` can call `grep`, but a
    /// call to `echo` (allowed by the outer session's permission rules) inside the SAME block is
    /// denied — proving it's the scope, not policy: `echo` succeeds when the same op runs OUTSIDE the
    /// block, in the same session, on the same executor.
    #[tokio::test]
    async fn with_tools_scope_allows_the_named_tool_and_denies_the_rest() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor_two_tools();
        let ast = DraftAst {
            body: vec![
                Node::CapScope {
                    tools: vec!["grep".into()],
                    body: vec![
                        flow_bind("hit", "grep", vec![]),
                        // `echo` is permission-allowed at the session level, but NOT in this scope.
                        flow_bind("leak", "echo", vec![flow_lit(json!("hi"))]),
                    ],
                    bind: None,
                },
                // Outside the block, on the SAME executor/session: echo succeeds — proving the earlier
                // denial came from the scope, not from a policy/permission-level block on `echo`.
                flow_bind("after", "echo", vec![flow_lit(json!("outside"))]),
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess_headline", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("echo")
                && err.to_string().contains("denied by capability scope"),
            "got: {err}"
        );

        // The SAME `echo` call, outside any scope, on a fresh flow over the same executor/session,
        // succeeds — proving the block (not the permission rules) denied it above.
        let ast2 = DraftAst {
            body: vec![flow_bind("after", "echo", vec![flow_lit(json!("outside"))])],
            ..Default::default()
        };
        let outcome = execute_flow(&store, &ex, "sess_headline_2", &ast2, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "outside");
    }

    /// **Non-bypassable.** The denial holds even when the disallowed call is reached indirectly,
    /// through a composite op invoked inside the scope — proving the gate is at `Executor::dispatch`,
    /// not just the `CapScope` node's own handler (which never sees the composite's inner calls
    /// directly; only `dispatch` does, on every recursive `execute_flow` the composite runs).
    #[tokio::test]
    async fn with_tools_scope_denies_a_call_reached_through_a_composite_op() {
        use flux_lang::program::CompositeOpDecl;

        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor_two_tools();
        // A composite op whose OWN body calls `echo` — the scope only names the composite's name, not
        // `echo`, so the composite is reachable but its inner call is not.
        let composite = CompositeOpDecl {
            name: "wraps_echo".into(),
            body: DraftAst {
                body: vec![flow_bind(
                    "inner",
                    "echo",
                    vec![flow_lit(json!("indirect"))],
                )],
                ..Default::default()
            },
            ..Default::default()
        };
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["wraps_echo".into()],
                body: vec![flow_bind("r", "wraps_echo", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow_with_composites(
            &store,
            &ex,
            "sess_composite_bypass",
            &ast,
            std::slice::from_ref(&composite),
            &mut sink,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("echo") && err.to_string().contains("capability scope"),
            "the scope must catch the composite's INNER call, got: {err}"
        );
    }

    /// **Evidence.** After a denied call inside a scope, the evidence log carries `cap_scope_enter`,
    /// `cap_scope_denied`, and `cap_scope_exit` observations.
    #[tokio::test]
    async fn with_tools_scope_records_enter_denial_and_exit_in_evidence() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor_two_tools();
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["grep".into()],
                body: vec![flow_bind("leak", "echo", vec![flow_lit(json!("hi"))])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let _ = execute_flow(&store, &ex, "sess_evidence", &ast, &mut sink).await;

        let log = ex.evidence();
        let kinds: Vec<&str> = log.all().iter().map(|o| o.kind.as_str()).collect();
        assert!(kinds.contains(&"cap_scope_enter"), "got: {kinds:?}");
        assert!(kinds.contains(&"cap_scope_denied"), "got: {kinds:?}");
        assert!(kinds.contains(&"cap_scope_exit"), "got: {kinds:?}");
        // Enter must precede the denial, and the denial must precede exit (pop only happens once the
        // body — including its failing step — has finished running).
        let enter_i = kinds.iter().position(|k| *k == "cap_scope_enter").unwrap();
        let denied_i = kinds.iter().position(|k| *k == "cap_scope_denied").unwrap();
        let exit_i = kinds.iter().position(|k| *k == "cap_scope_exit").unwrap();
        assert!(enter_i < denied_i, "enter must come before the denial");
        assert!(denied_i < exit_i, "denial must come before exit");
    }

    /// Dropping the whole interpreter future (the engine's external cancellation mechanism) also
    /// drops `ExecutorHost`, whose stored RAII guards must restore the executor's capability stack.
    #[tokio::test]
    async fn external_cancellation_drops_the_capability_scope_guard() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor_with_slow_tool();
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["slow".into()],
                body: vec![flow_bind("pending", "slow", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            execute_flow(&store, &ex, "sess_cancel_scope", &ast, &mut sink),
        )
        .await;
        assert!(cancelled.is_err(), "the outer driver cancelled the flow");

        // `echo` was absent from the cancelled scope. Its success proves the guard was dropped.
        let after = DraftAst {
            body: vec![flow_bind("after", "echo", vec![flow_lit(json!("outside"))])],
            ..Default::default()
        };
        let outcome = execute_flow(&store, &ex, "sess_cancel_scope_after", &after, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "outside");
        let kinds: Vec<String> = ex
            .evidence()
            .all()
            .iter()
            .map(|observation| observation.kind.clone())
            .collect();
        assert!(kinds.iter().any(|kind| kind == "cap_scope_enter"));
        assert!(kinds.iter().any(|kind| kind == "cap_scope_exit"));
    }

    /// **Nesting narrows, never widens.** An inner `with_tools` cannot re-grant a tool the outer scope
    /// removed, even though the inner scope's own literal list names it.
    #[tokio::test]
    async fn nested_with_tools_cannot_widen_past_the_outer_scope() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor_two_tools();
        let ast = DraftAst {
            body: vec![Node::CapScope {
                tools: vec!["grep".into()],
                body: vec![Node::CapScope {
                    // Inner scope asks for BOTH — the outer only allowed `grep`.
                    tools: vec!["grep".into(), "echo".into()],
                    body: vec![flow_bind("leak", "echo", vec![flow_lit(json!("hi"))])],
                    bind: None,
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let err = execute_flow(&store, &ex, "sess_nested_narrow", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("echo") && err.to_string().contains("capability scope"),
            "inner scope must not re-grant what the outer removed, got: {err}"
        );
    }

    /// A no-op empty stack: a flow that never opens a `with_tools` scope is completely unaffected —
    /// every existing behavior (both tools, unrestricted) keeps working exactly as before this feature.
    #[tokio::test]
    async fn no_active_scope_leaves_every_flow_unaffected() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor_two_tools();
        let ast = DraftAst {
            body: vec![
                flow_bind("a", "echo", vec![flow_lit(json!("hi"))]),
                flow_bind("b", "grep", vec![]),
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow(&store, &ex, "sess_no_scope", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(outcome.result, "match");
    }

    // ---- L-25: authored `flux flow run --resumable`/`--resume` engine seam ----

    /// A tool that always fails (an ordinary `is_error`, never `denied`) — reifies a
    /// `FailureKind::Runtime` halt without touching permissions/approval.
    struct BoomTool;

    #[async_trait]
    impl Tool for BoomTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "boom",
                "always fails",
                json!({"type": "object", "properties": {}}),
            )
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            Ok(ToolResult::error("boom failed"))
        }
    }

    /// A tool that records its `text` arg into a shared, order-preserving log — used to observe
    /// exactly which statements actually dispatched across two resumable runs. A ledger-skipped
    /// (fast-forwarded) statement never touches this, which is exactly the property the `once`/
    /// `saga` invariant tests below check for.
    #[derive(Clone, Default)]
    struct MarkTool {
        marks: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for MarkTool {
        fn spec(&self) -> ToolSpec {
            let mut spec = ToolSpec::read_only(
                "mark",
                "record a mark",
                json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            );
            // A dispatch-recording probe: every call appends to the mark log, so it is NOT
            // idempotent — declaring that keeps it out of the deterministic-read cache (L-54),
            // whose whole contract is "replaying the result is indistinguishable from re-running".
            spec.idempotency = flux_spec::Idempotency::NonIdempotent;
            spec
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.marks.lock().unwrap().push(text.clone());
            Ok(ToolResult::ok(text))
        }
    }

    /// An executor with `mark` (recording, always succeeds) and `boom` (always fails) registered
    /// and allowed — the L-25 authored-resumable-path fixture. Returns the `MarkTool` handle so a
    /// test can inspect the dispatch log directly, independent of the transcript.
    fn temp_executor_l25() -> (Executor, MarkTool) {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-l25-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mark = MarkTool::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(mark.clone()));
        reg.register(Arc::new(BoomTool));
        let perms = PermissionManager::from_rules(&["mark".into(), "boom".into()], &[]);
        let ex = Executor::new(
            reg,
            perms,
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        );
        (ex, mark)
    }

    /// A bare `mark("text")` call node (a single literal binds to `mark`'s sole required param,
    /// same sugar `flow_bind`'s "echo" calls above rely on).
    fn mark_call(text: &str) -> Node {
        Node::Call {
            op: "mark".into(),
            args: vec![flow_lit(json!(text))],
        }
    }

    /// [`render_halt_report`] marks the completed prefix `✓`, the failed statement `✗`, and the rest
    /// `·`; embeds a machine-readable `failure` object (node/stmt/op/kind/fatal/message/plan/session);
    /// and tells the reader the exact `--resume <session>` command to correct-and-continue with.
    #[test]
    fn render_halt_report_marks_prefix_and_embeds_machine_readable_failure() {
        let ast = DraftAst {
            body: vec![
                flow_bind("a", "echo", vec![flow_lit(json!("hi"))]),
                flow_bind("b", "boom", vec![]),
                Node::Return {
                    value: Box::new(flow_var("b")),
                },
            ],
            ..Default::default()
        };
        let halt = PlanHalt {
            node: NodeId(1),
            stmt: stmt_hash16(&ast.body[1]),
            op: Some("boom".into()),
            kind: FailureKind::Runtime,
            message: "boom failed: boom failed".into(),
            plan: "h:deadbeefdeadbeef".into(),
        };
        let report = render_halt_report(&ast, &halt, "s_42");
        assert!(
            report.contains("\u{2713} 0:"),
            "step 0 marked done: {report}"
        );
        assert!(
            report.contains("\u{2717} 1:"),
            "the failed step marked: {report}"
        );
        assert!(
            report.contains("\u{b7} 2:"),
            "the not-yet-run step marked: {report}"
        );
        assert!(report.contains("[flow halted at step 2 of 3]"));
        assert!(report.contains("\"session\":\"s_42\""));
        assert!(report.contains("\"kind\":\"runtime\""));
        assert!(report.contains("\"fatal\":false"));
        assert!(report.contains("--resume s_42"));
    }

    /// The denial re-emission guard (A-16): a `Denied`/`ConfirmDenied` halt whose exact statement is
    /// still present anywhere in the body is blocked; an EDITED statement (different content hash)
    /// is not; a non-denial halt kind is never guarded (a plain runtime failure IS meant to be
    /// retried unchanged — that's patch-and-continue).
    #[test]
    fn denied_resume_guard_blocks_only_the_unchanged_denied_statement() {
        let unchanged = flow_bind("a", "echo", vec![flow_lit(json!("x"))]);
        let edited = flow_bind("a", "echo", vec![flow_lit(json!("y"))]);
        let halt = PlanHalt {
            node: NodeId(0),
            stmt: stmt_hash16(&unchanged),
            op: Some("echo".into()),
            kind: FailureKind::Denied,
            message: "denied by policy".into(),
            plan: "h:aaaa111122223333".into(),
        };
        assert!(
            denied_resume_guard(std::slice::from_ref(&unchanged), &halt),
            "the exact refused statement, unchanged, is blocked"
        );
        assert!(
            !denied_resume_guard(std::slice::from_ref(&edited), &halt),
            "an edited statement (different content hash) is not blocked"
        );
        let runtime_halt = PlanHalt {
            kind: FailureKind::Runtime,
            ..halt.clone()
        };
        assert!(
            !denied_resume_guard(std::slice::from_ref(&unchanged), &runtime_halt),
            "only Denied/ConfirmDenied are guarded — a plain runtime failure is meant to be retried"
        );
    }

    /// The authored path's denial invariant end to end: a denied statement halts; folding the
    /// session's ledger and re-checking the SAME unchanged statement blocks it (the guard a
    /// `flux flow run --resume` caller must consult before calling
    /// [`execute_flow_resumable_with_composites`] again) — and, showing why the guard is needed,
    /// the interpreter itself has no memory of the refusal: nothing in the ledger machinery stops a
    /// second attempt from re-dispatching the same denied call (it was never ledgered as completed),
    /// so the CLI-level guard is what actually enforces "never re-dispatch unchanged" (A-16).
    #[tokio::test]
    async fn resumable_authored_path_denied_statement_would_redispatch_without_the_guard() {
        let store = FlowStore::in_memory().unwrap();
        let ex = temp_executor(false); // "echo" registered but denied by permission rules
        let session = "sess-l25-denied";
        let ast = DraftAst {
            body: vec![flow_bind("a", "echo", vec![flow_lit(json!("x"))])],
            ..Default::default()
        };
        let mut sink = CollectSink::default();

        let outcome = execute_flow_resumable_with_composites(
            &store,
            &ex,
            session,
            &ast,
            &[],
            None,
            &mut sink,
        )
        .await
        .unwrap();
        let halt = outcome.failure.clone().expect("the denied call halts");
        assert_eq!(halt.kind, FailureKind::Denied);

        let open = store
            .open_halted_plan(session)
            .unwrap()
            .expect("the halt latch is open");
        assert!(
            open.ledger.completed.is_empty(),
            "the denied statement never ledgers as completed"
        );

        // The guard says: block. A caller that (incorrectly) skipped the guard and ran again anyway
        // would re-dispatch the identical denied call — proving the guard, not the ledger, is what
        // must stop it.
        assert!(denied_resume_guard(&ast.body, &halt));
        let outcome2 = execute_flow_resumable_with_composites(
            &store,
            &ex,
            session,
            &ast,
            &[],
            Some(&open.ledger),
            &mut sink,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome2.failure.map(|h| h.kind),
            Some(FailureKind::Denied),
            "without the guard, the interpreter alone re-dispatches the same denied statement"
        );
    }

    /// L-25 acceptance: `once` must not re-fire across a ledger fast-forward on the authored path —
    /// verified through the REAL flux-flow engine adapter (`execute_flow_resumable_with_composites`
    /// over a live `Executor`/`FlowStore`), not just flux-lang's generic interpreter tests.
    #[tokio::test]
    async fn resumable_authored_path_never_refires_once_across_fast_forward() {
        let store = FlowStore::in_memory().unwrap();
        let (ex, mark) = temp_executor_l25();
        let session = "sess-l25-once";

        // First attempt: `once` fires, then the second statement BOOMs.
        let ast1 = DraftAst {
            body: vec![
                Node::Once {
                    label: "greet".into(),
                    body: vec![mark_call("once-body")],
                    bind: None,
                },
                flow_bind("b", "boom", vec![]),
                Node::Return {
                    value: Box::new(flow_var("b")),
                },
            ],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow_resumable_with_composites(
            &store,
            &ex,
            session,
            &ast1,
            &[],
            None,
            &mut sink,
        )
        .await
        .unwrap();
        assert!(outcome.failure.is_some(), "the boom statement halts");
        assert_eq!(*mark.marks.lock().unwrap(), vec!["once-body".to_string()]);

        let open = store
            .open_halted_plan(session)
            .unwrap()
            .expect("the halt latch is open");
        assert_eq!(
            open.ledger.completed.len(),
            1,
            "the `once` statement completed"
        );

        // Corrected re-emission: the SAME `once` statement, byte-identical; the failing statement
        // replaced with one that succeeds.
        let ast2 = DraftAst {
            body: vec![
                Node::Once {
                    label: "greet".into(),
                    body: vec![mark_call("once-body")],
                    bind: None,
                },
                flow_bind("b", "mark", vec![flow_lit(json!("fixed"))]),
                Node::Return {
                    value: Box::new(flow_var("b")),
                },
            ],
            ..Default::default()
        };
        let outcome2 = execute_flow_resumable_with_composites(
            &store,
            &ex,
            session,
            &ast2,
            &[],
            Some(&open.ledger),
            &mut sink,
        )
        .await
        .unwrap();
        assert!(outcome2.failure.is_none(), "the corrected plan completes");
        assert_eq!(
            *mark.marks.lock().unwrap(),
            vec!["once-body".to_string(), "fixed".to_string()],
            "the fast-forwarded `once` statement must not re-dispatch its body"
        );
    }

    /// L-25 acceptance: a `saga` halted mid-body re-enters compensation bookkeeping consistently on
    /// resume — the whole saga statement re-runs (it never ledgers as completed, F14), so a SECOND
    /// genuine failure would compensate the SAME steps again, and a resume that now succeeds fires
    /// NO extra compensation (the first attempt's `undo` is not repeated, and no phantom undo leaks
    /// into the successful second attempt).
    #[tokio::test]
    async fn resumable_authored_path_saga_recompensates_consistently_on_resume() {
        let store = FlowStore::in_memory().unwrap();
        let (ex, mark) = temp_executor_l25();
        let session = "sess-l25-saga";

        // First attempt: step1 ok (undo registered), step2 BOOMs -> undo(step1) fires, then the
        // saga's own error propagates from this bare top-level statement, which halts (never
        // ledgers). `Saga` has no `bind` of its own (unlike `seq`/`scope`/…), so it stands alone —
        // exactly how flux-lang's own saga tests construct it.
        let ast1 = DraftAst {
            body: vec![Node::Saga {
                steps: vec![
                    SagaStep {
                        body: vec![mark_call("s1")],
                        undo: vec![mark_call("r1")],
                    },
                    SagaStep {
                        body: vec![Node::Call {
                            op: "boom".into(),
                            args: vec![],
                        }],
                        undo: vec![],
                    },
                ],
            }],
            ..Default::default()
        };
        let mut sink = CollectSink::default();
        let outcome = execute_flow_resumable_with_composites(
            &store,
            &ex,
            session,
            &ast1,
            &[],
            None,
            &mut sink,
        )
        .await
        .unwrap();
        assert!(outcome.failure.is_some(), "the saga's own failure halts");
        assert_eq!(
            *mark.marks.lock().unwrap(),
            vec!["s1".to_string(), "r1".to_string()],
            "step1 ran and step2's failure compensated it (reverse order)"
        );

        let open = store
            .open_halted_plan(session)
            .unwrap()
            .expect("the halt latch is open");
        assert!(
            open.ledger.completed.is_empty(),
            "the saga statement never ledgers as completed — F14"
        );

        // Corrected re-emission: step2 now succeeds too.
        let ast2 = DraftAst {
            body: vec![Node::Saga {
                steps: vec![
                    SagaStep {
                        body: vec![mark_call("s1")],
                        undo: vec![mark_call("r1")],
                    },
                    SagaStep {
                        body: vec![mark_call("s2")],
                        undo: vec![],
                    },
                ],
            }],
            ..Default::default()
        };
        let outcome2 = execute_flow_resumable_with_composites(
            &store,
            &ex,
            session,
            &ast2,
            &[],
            Some(&open.ledger),
            &mut sink,
        )
        .await
        .unwrap();
        assert!(outcome2.failure.is_none(), "the saga completes on resume");
        assert_eq!(
            *mark.marks.lock().unwrap(),
            vec!["s1", "r1", "s1", "s2"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            "the whole saga re-runs wholly (step1 dispatches again) and, since it now succeeds, \
             fires NO extra compensation — `r1` appears exactly once, from the first genuine failure"
        );
    }

    // ---- D-175: cassette-scope family at the ONE dispatch chokepoint ----

    /// A tool that counts real executions — proves a served (tape) dispatch never touches the live
    /// executor, and a missed one runs it exactly once.
    struct CountingTool(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait]
    impl Tool for CountingTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "counted",
                "count real executions",
                json!({"type": "object", "additionalProperties": false}),
            )
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _params: serde_json::Value,
        ) -> flux_core::Result<ToolResult> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolResult::ok("live-value"))
        }
    }

    /// An approver that counts every consultation and always returns `choice` — pins the D-175
    /// per-arm `request_approval` table: `Replay`/`Frozen(Halt)` must auto-allow WITHOUT ever
    /// reaching here; `Record`/`Frozen(Live)`/`Resume` must consult it for real.
    struct ScriptedApprover {
        calls: std::sync::atomic::AtomicUsize,
        choice: ApprovalChoice,
    }

    #[async_trait]
    impl Approver for ScriptedApprover {
        async fn request(
            &self,
            _tool: &str,
            _subjects: &[String],
            _intents: &IntentSet,
        ) -> ApprovalChoice {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.choice.clone()
        }
    }

    fn counting_tool_executor(
        approver: Arc<dyn Approver>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-rt-cassette-scope-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(CountingTool(calls)));
        Executor::new(
            reg,
            PermissionManager::from_rules(&["counted".into()], &[]),
            approver,
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
    }

    /// `$x = counted(); return $x`.
    fn counted_ast() -> DraftAst {
        DraftAst {
            body: vec![
                flow_bind("x", "counted", vec![]),
                Node::Return {
                    value: Box::new(flow_var("x")),
                },
            ],
            ..Default::default()
        }
    }

    /// `confirm "run counted" { $x = counted() }; return $x` — a `confirm` gate wraps the dispatch so
    /// the adversarial approver tests can pin `request_approval`'s per-arm decision independently of
    /// `dispatch`'s.
    fn confirm_counted_ast() -> DraftAst {
        DraftAst {
            body: vec![
                Node::Confirm {
                    message: "run counted".into(),
                    risk: None,
                    body: vec![flow_bind("x", "counted", vec![])],
                },
                Node::Return {
                    value: Box::new(flow_var("x")),
                },
            ],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn frozen_halt_serves_from_tape_and_never_touches_the_executor() {
        // Record once, live, to get one real `counted` cell.
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let record_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        record_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Record(
            crate::cassette::RecordScope::new(events.clone(), "sess"),
        ))));
        let live_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = CollectSink::default();
        let outcome = execute_flow_with_composites(
            &record_store,
            &ex,
            "sess",
            &counted_ast(),
            &[],
            &mut sink,
        )
        .await
        .unwrap();
        assert_eq!(outcome.result, "live-value");
        assert_eq!(live_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Serve the SAME flow from a `Frozen(Halt)` scope over the recorded tape.
        let trace = events.run_trace("sess").unwrap();
        let tape = crate::cassette::ReplayTape::from_trace(&trace);
        let store = FlowStore::in_memory().unwrap();
        store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Frozen(
            crate::cassette::FrozenTape::hermetic(tape),
        ))));
        let tape_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tape_ex = counting_tool_executor(Arc::new(AllowApprover), tape_calls.clone());
        let mut sink2 = CollectSink::default();
        let outcome2 =
            execute_flow_with_composites(&store, &tape_ex, "sess", &counted_ast(), &[], &mut sink2)
                .await
                .unwrap();

        assert_eq!(outcome2.result, "live-value", "served the recorded content");
        assert_eq!(
            tape_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Frozen(Halt) must never touch the live executor for a matched cell"
        );
    }

    #[tokio::test]
    async fn frozen_live_dispatches_live_exactly_once_and_records_the_bridge_tail() {
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        // An empty tape: every dispatch is a miss, so a `Frozen(Live)` scope must bridge to the
        // live executor exactly once and record the tail cell.
        let bridge = crate::cassette::RecordScope::new(events.clone(), "sess");
        store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Frozen(
            crate::cassette::FrozenTape::live_bridge(
                crate::cassette::ReplayTape::from_cells(vec![]),
                bridge,
            ),
        ))));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), calls.clone());
        let mut sink = CollectSink::default();
        let outcome =
            execute_flow_with_composites(&store, &ex, "sess", &counted_ast(), &[], &mut sink)
                .await
                .unwrap();

        assert_eq!(outcome.result, "live-value");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the miss must fall through to the ONE live path exactly once"
        );
        let trace = events.run_trace("sess").unwrap();
        assert!(
            trace
                .iter()
                .any(|e| matches!(e, RunEvent::OpRecorded { op, .. } if op == "counted")),
            "the live-bridge dispatch must be recorded into the tail, just like `Record`"
        );
    }

    #[tokio::test]
    async fn resume_serves_the_completed_op_without_refiring() {
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let record_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        record_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Record(
            crate::cassette::RecordScope::new(events.clone(), "sess"),
        ))));
        let live_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = CollectSink::default();
        execute_flow_with_composites(&record_store, &ex, "sess", &counted_ast(), &[], &mut sink)
            .await
            .unwrap();
        assert_eq!(live_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let trace = events.run_trace("sess").unwrap();
        let cells: Vec<crate::cassette::Cell> = trace
            .iter()
            .filter_map(|e| match e {
                RunEvent::OpRecorded {
                    op,
                    input_hash,
                    input_hash_redacted,
                    content,
                    view,
                    is_error,
                    denied,
                    truncated,
                    ..
                } => Some(crate::cassette::Cell {
                    op: op.clone(),
                    input_hash: input_hash.clone(),
                    input_hash_redacted: input_hash_redacted.clone(),
                    content: content.clone(),
                    view: view.clone(),
                    is_error: *is_error,
                    denied: *denied,
                    truncated: *truncated,
                }),
                _ => None,
            })
            .collect();
        assert_eq!(cells.len(), 1, "one crash-tail cell for the completed op");

        let resume_store = FlowStore::in_memory().unwrap();
        let resume_events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let tail = crate::cassette::RecordScope::new(resume_events.clone(), "sess");
        resume_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Resume(
            crate::cassette::ResumeTape::new(cells, tail),
        ))));
        let resume_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resume_ex = counting_tool_executor(Arc::new(AllowApprover), resume_calls.clone());
        let mut sink2 = CollectSink::default();
        let outcome = execute_flow_with_composites(
            &resume_store,
            &resume_ex,
            "sess",
            &counted_ast(),
            &[],
            &mut sink2,
        )
        .await
        .unwrap();

        assert_eq!(outcome.result, "live-value", "served from the crash tail");
        assert_eq!(
            resume_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a completed op must never re-fire on resume"
        );
    }

    #[tokio::test]
    async fn resume_miss_runs_the_op_live_and_records_the_tail() {
        let store = FlowStore::in_memory().unwrap();
        let tail_events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let tail = crate::cassette::RecordScope::new(tail_events.clone(), "sess");
        // No crash-tail cells at all — every dispatch is the honest at-least-once window.
        store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Resume(
            crate::cassette::ResumeTape::new(vec![], tail),
        ))));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), calls.clone());
        let mut sink = CollectSink::default();
        let outcome =
            execute_flow_with_composites(&store, &ex, "sess", &counted_ast(), &[], &mut sink)
                .await
                .unwrap();

        assert_eq!(outcome.result, "live-value");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let trace = tail_events.run_trace("sess").unwrap();
        assert!(
            trace
                .iter()
                .any(|e| matches!(e, RunEvent::OpRecorded { op, .. } if op == "counted")),
            "the live tail dispatch must be recorded, exactly like a normal `Record` scope"
        );
    }

    #[tokio::test]
    async fn replay_and_frozen_halt_auto_allow_a_confirm_without_consulting_the_approver() {
        // `Replay`: a recorded run already passed its `confirm` gate.
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let record_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        record_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Record(
            crate::cassette::RecordScope::new(events.clone(), "sess"),
        ))));
        let live_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = CollectSink::default();
        execute_flow_with_composites(
            &record_store,
            &ex,
            "sess",
            &confirm_counted_ast(),
            &[],
            &mut sink,
        )
        .await
        .unwrap();
        let trace = events.run_trace("sess").unwrap();

        let deny = Arc::new(ScriptedApprover {
            calls: std::sync::atomic::AtomicUsize::new(0),
            choice: ApprovalChoice::Deny,
        });
        let replay_store = FlowStore::in_memory().unwrap();
        replay_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Replay(
            crate::cassette::ReplayTape::from_trace(&trace),
        ))));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let replay_ex = counting_tool_executor(deny.clone(), calls);
        let mut sink2 = CollectSink::default();
        execute_flow_with_composites(
            &replay_store,
            &replay_ex,
            "sess",
            &confirm_counted_ast(),
            &[],
            &mut sink2,
        )
        .await
        .expect("Replay auto-allows the confirm gate even with a deny approver installed");
        assert_eq!(
            deny.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the deny approver must never be consulted under Replay"
        );

        // `Frozen(Halt)`: the frozen world already passed its gates too.
        let deny2 = Arc::new(ScriptedApprover {
            calls: std::sync::atomic::AtomicUsize::new(0),
            choice: ApprovalChoice::Deny,
        });
        let frozen_store = FlowStore::in_memory().unwrap();
        frozen_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Frozen(
            crate::cassette::FrozenTape::hermetic(crate::cassette::ReplayTape::from_trace(&trace)),
        ))));
        let calls2 = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let frozen_ex = counting_tool_executor(deny2.clone(), calls2);
        let mut sink3 = CollectSink::default();
        execute_flow_with_composites(
            &frozen_store,
            &frozen_ex,
            "sess",
            &confirm_counted_ast(),
            &[],
            &mut sink3,
        )
        .await
        .expect("Frozen(Halt) auto-allows the confirm gate even with a deny approver installed");
        assert_eq!(
            deny2.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the deny approver must never be consulted under Frozen(Halt)"
        );
    }

    #[tokio::test]
    async fn frozen_live_and_resume_gate_a_confirm_through_the_real_deny_approver() {
        // `Frozen(Live)`: a miss can reach a real live dispatch, so the confirm gate MUST be real.
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let bridge = crate::cassette::RecordScope::new(events.clone(), "sess");
        let frozen_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        frozen_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Frozen(
            crate::cassette::FrozenTape::live_bridge(
                crate::cassette::ReplayTape::from_cells(vec![]),
                bridge,
            ),
        ))));
        let deny = Arc::new(ScriptedApprover {
            calls: std::sync::atomic::AtomicUsize::new(0),
            choice: ApprovalChoice::Deny,
        });
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ex = counting_tool_executor(deny.clone(), calls.clone());
        let mut sink = CollectSink::default();
        let err = execute_flow_with_composites(
            &frozen_store,
            &ex,
            "sess",
            &confirm_counted_ast(),
            &[],
            &mut sink,
        )
        .await
        .expect_err("a deny approver must deny the confirm gate under Frozen(Live)");
        assert!(err.to_string().to_lowercase().contains("confirm"), "{err}");
        assert_eq!(
            deny.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Frozen(Live) must consult the real approver — the :158 auto-allow must not leak here"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a denied confirm gate must never let the guarded op dispatch"
        );

        // `Resume`: the crash tail is live too — same posture.
        let tail_events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let tail = crate::cassette::RecordScope::new(tail_events.clone(), "sess");
        let resume_store = FlowStore::in_memory().unwrap();
        resume_store.set_cassette(Some(Arc::new(crate::cassette::CassetteScope::Resume(
            crate::cassette::ResumeTape::new(vec![], tail),
        ))));
        let deny2 = Arc::new(ScriptedApprover {
            calls: std::sync::atomic::AtomicUsize::new(0),
            choice: ApprovalChoice::Deny,
        });
        let calls2 = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resume_ex = counting_tool_executor(deny2.clone(), calls2.clone());
        let mut sink2 = CollectSink::default();
        execute_flow_with_composites(
            &resume_store,
            &resume_ex,
            "sess",
            &confirm_counted_ast(),
            &[],
            &mut sink2,
        )
        .await
        .expect_err("a deny approver must deny the confirm gate under Resume");
        assert_eq!(
            deny2.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "Resume must consult the real approver too"
        );
        assert_eq!(calls2.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

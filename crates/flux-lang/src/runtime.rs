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

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use flux_core::{Error, Usage};
use flux_spec::IntentSet;

use crate::ast::{
    DraftAst, FailureKind, Node, NodeId, PhysicalPlan, RunEvent, Stage, StepId, SymbolName,
    TypeRef, Value, ValueId, Visibility,
};
use crate::host::{ApprovalChoice, OpHost, OpOutcome};
use crate::opspec::OpCatalog;
use crate::program::CompositeOpDecl;
use crate::sink::FlowSink;
use crate::store::{DurableStore, SessionView, SymbolView, ValueStore};
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
    /// True when the failure was the host envelope **refusing to run** the op (see
    /// [`OpOutcome::denied`]) — surfaced as the fatal [`FlowError::Denied`] instead of the
    /// retryable runtime wrap, so `retry`/`loop`/composites never re-attempt a denial (L-21).
    pub denied: bool,
    /// The canonical value: bound to the symbol, spliced into `{{interpolation}}`, used for control
    /// flow (`when`/`return`). Deterministic execution works with this.
    pub content: String,
    /// The model-facing rendering (line-numbered read, diff, …). Equals `content` when the op set no
    /// distinct view. Surfaced to the sink/observation, never bound or interpolated.
    pub view: String,
}

/// The error a failed call surfaces as: a **denial** (the host envelope refused the op — see
/// [`CallOutcome::denied`]) keeps its structural identity as the fatal [`FlowError::Denied`] so the
/// reliability wrappers (`retry`, `loop`, the composite boundary) never re-attempt it; every other
/// failure is the transient [`FlowError::Runtime`] wrap, labelled with its call site.
fn call_failure(label: &str, outcome: &CallOutcome) -> FlowError {
    if outcome.denied {
        FlowError::Denied(outcome.content.clone())
    } else {
        FlowError::Runtime(format!("{label} failed: {}", outcome.content))
    }
}

/// `pub` (C-43): the cassette recorder must key cells with the EXACT hash [`execute_call`] stamps
/// on `StepStarted.input_hash` — one shared derivation, never a host-layer reimplementation (the
/// same rationale that made [`stmt_hash16`] public for A-16).
pub fn sha256_hex(s: &str) -> String {
    use std::fmt::Write;
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    // digest 0.11 dropped the `LowerHex` impl on the output array; encode by hand (same bytes,
    // same lowercase hex as the old `{:x}` — recorded cassette/checkpoint keys stay valid).
    h.finalize()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The durable identity of a flow for `checkpoint` scoping: the declared name (when present) **plus**
/// a stable content hash of its top-level body. The hash means an *edited* flow never fast-forwards
/// past changed statements; the name scopes two identical bodies apart. This is the ONE derivation
/// shared by [`execute_flow`] and [`resume_flow_named`], so a flow's run and resume agree on its
/// checkpoint keys (F17 — previously run keyed by name only and resume by hash only). `pub`
/// (A-45): the replay driver maps a recorded trace's `StatementCompleted.plan` keys back to their
/// `plan_source` plans through this same derivation — never a host-layer reimplementation.
pub fn flow_key(name: Option<&str>, body: &[Node]) -> String {
    let hash = sha256_hex(&serde_json::to_string(body).unwrap_or_default());
    match name {
        Some(n) if !n.trim().is_empty() => format!("{n}#{}", &hash[..16]),
        _ => format!("h:{}", &hash[..16]),
    }
}

/// A single top-level statement's content identity for the resumable-mode ledger:
/// `sha256(canonical JSON of the node)[..16]`. The statement is hashed through its typed [`Node`]
/// representation (not raw source text), so the result is insensitive to the source's key order and
/// formatting — a re-emission that reorders an object's keys or reformats whitespace still hits the
/// same `stmt_hash16` (`serde_json` maps serialize key-sorted, and a `Node`'s own fields serialize in
/// one fixed declared order regardless of how the original text ordered them).
///
/// `pub` (A-16): a loop host must compute this same identity BEFORE executing anything — to preview
/// the prospective skip prefix (for suffix-scoped approval + ✓/·-marked plan rendering) and to check
/// the denial re-emission guard against a halt's recorded `stmt` — so the one canonical derivation is
/// shared rather than re-implemented at the host layer.
pub fn stmt_hash16(node: &Node) -> String {
    let json = serde_json::to_string(node).unwrap_or_default();
    sha256_hex(&json)[..16].to_string()
}

/// A one-line, length-bounded summary of a value for the symbol table (never the raw bytes).
fn summarize(content: &str) -> String {
    // A JSON-array value (e.g. a `glob` result) summarizes by SHAPE: the planner composes on
    // "a list of N" (`each`/`merge`), which the first 80 chars of serialized JSON don't convey
    // (C-10).
    if content.trim_start().starts_with('[') {
        if let Ok(serde_json::Value::Array(items)) =
            serde_json::from_str::<serde_json::Value>(content.trim())
        {
            let preview: Vec<String> = items
                .iter()
                .take(3)
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            let more = if items.len() > 3 { ", …" } else { "" };
            return truncate_summary(&format!(
                "list ({}): {}{}",
                items.len(),
                preview.join(", "),
                more
            ));
        }
    }
    let first = content.lines().next().unwrap_or("").trim();
    let line = content
        .lines()
        .map(str::trim)
        .find(|line| !summary_noise_line(line))
        .unwrap_or(first);
    truncate_summary(line)
}

fn summary_noise_line(line: &str) -> bool {
    if line.is_empty() {
        return true;
    }
    line.trim_matches(|c| matches!(c, '{' | '}' | '[' | ']' | ','))
        .trim()
        .is_empty()
}

fn truncate_summary(line: &str) -> String {
    if line.chars().count() > 80 {
        let head: String = line.chars().take(77).collect();
        format!("{head}...")
    } else {
        line.to_string()
    }
}

fn op_summary_prefix(op: &str, input: &serde_json::Value) -> Option<String> {
    match op {
        "read" => {
            let path = input.get("path")?;
            if let Some(path) = path.as_str() {
                return Some(match (input.get("offset"), input.get("limit")) {
                    (Some(offset), Some(limit)) => format!("read {path} lines {offset}+{limit}"),
                    (Some(offset), None) => format!("read {path} from line {offset}"),
                    _ => format!("read {path}"),
                });
            }
            path.as_array()
                .map(|paths| format!("read {} paths", paths.len()))
        }
        "grep" => input
            .get("pattern")
            .and_then(|p| p.as_str())
            .map(|pattern| {
                let scope = input
                    .get("path")
                    .and_then(|p| p.as_str())
                    .or_else(|| input.get("glob").and_then(|p| p.as_str()))
                    .unwrap_or(".");
                format!("grep {pattern:?} in {scope}")
            }),
        _ => None,
    }
}

fn summarize_bound_result(op: &str, input: &serde_json::Value, content: &str) -> String {
    let summary = summarize(content);
    match op_summary_prefix(op, input) {
        Some(prefix) if summary.is_empty() => truncate_summary(&prefix),
        Some(prefix) => truncate_summary(&format!("{prefix}: {summary}")),
        None => summary,
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

#[derive(Debug, Clone)]
struct FrameBinding {
    vid: ValueId,
    ty: Option<String>,
    summary: String,
    visibility: Visibility,
}

/// A scoped symbol overlay for composite-op execution. Values and run events stay in the parent
/// store/session, but params and locals resolve only inside this frame.
struct FrameStore<'a> {
    parent: &'a dyn ValueStore,
    bindings: Mutex<HashMap<SymbolName, FrameBinding>>,
}

impl<'a> FrameStore<'a> {
    fn new(parent: &'a dyn ValueStore) -> Self {
        Self {
            parent,
            bindings: Mutex::new(HashMap::new()),
        }
    }

    fn seed_param(
        &self,
        session_id: &str,
        param: &crate::ast::Param,
        value: &serde_json::Value,
    ) -> Result<()> {
        let v = Value::from_json(value);
        let text = value_text(&v);
        let vid = self.parent.put_value(session_id, &v)?;
        self.bind(
            session_id,
            &param.name,
            &vid,
            Some(&param.ty.label()),
            &summarize(&text),
            Visibility::Hidden,
        )?;
        Ok(())
    }
}

impl ValueStore for FrameStore<'_> {
    fn put_value(&self, session_id: &str, value: &Value) -> flux_core::Result<ValueId> {
        self.parent.put_value(session_id, value)
    }

    fn get_value(&self, id: &ValueId) -> flux_core::Result<Option<Value>> {
        self.parent.get_value(id)
    }

    fn bind(
        &self,
        _session_id: &str,
        name: &SymbolName,
        vid: &ValueId,
        ty: Option<&str>,
        summary: &str,
        visibility: Visibility,
    ) -> flux_core::Result<()> {
        self.bindings.lock().unwrap().insert(
            name.clone(),
            FrameBinding {
                vid: vid.clone(),
                ty: ty.map(str::to_string),
                summary: summary.to_string(),
                visibility,
            },
        );
        Ok(())
    }

    fn resolve(&self, _session_id: &str, name: &SymbolName) -> flux_core::Result<Option<ValueId>> {
        Ok(self
            .bindings
            .lock()
            .unwrap()
            .get(name)
            .map(|b| b.vid.clone()))
    }

    fn append_event(&self, session_id: &str, event: &RunEvent) -> flux_core::Result<()> {
        self.parent.append_event(session_id, event)
    }

    fn view(&self, _session_id: &str) -> flux_core::Result<SessionView> {
        let mut symbols: Vec<_> = self
            .bindings
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, binding)| binding.visibility != Visibility::Hidden)
            .map(|(name, binding)| SymbolView {
                name: name.clone(),
                ty: binding.ty.clone(),
                summary: binding.summary.clone(),
                visibility: binding.visibility,
            })
            .collect();
        symbols.sort_by(|a, b| a.name.0.cmp(&b.name.0));
        Ok(SessionView { symbols })
    }

    fn binding(
        &self,
        _session_id: &str,
        name: &SymbolName,
    ) -> flux_core::Result<Option<SymbolView>> {
        Ok(self.bindings.lock().unwrap().get(name).map(|b| SymbolView {
            name: name.clone(),
            ty: b.ty.clone(),
            summary: b.summary.clone(),
            visibility: b.visibility,
        }))
    }

    fn as_durable(&self) -> Option<&dyn DurableStore> {
        self.parent.as_durable()
    }
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
    // The step id carries the op name, not just the input hash — two different ops over identical
    // input must not share an id in the audit trail (F20c).
    let step = StepId(format!("step_{op}_{}", &input_hash[..16]));

    store.append_event(
        session_id,
        &RunEvent::StepStarted {
            step: step.clone(),
            op: op.to_string(),
            input_hash,
        },
    )?;

    let summary_input = input.clone();
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
            denied: result.denied,
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
            &summarize_bound_result(op, &summary_input, &result.content),
            b.visibility,
        )?;
        // Post-bind host notification (A-20): lets dispatch-keyed host bookkeeping (e.g. the
        // redundant-read cache) name the symbol this exact call's result lives under.
        executor.call_bound(op, &summary_input, &b.name.0);
    }

    Ok(CallOutcome {
        value_id: Some(value_id),
        is_error: false,
        denied: false,
        content: result.content,
        view,
    })
}

async fn execute_composite_call(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    composite: &CompositeOpDecl,
    input: serde_json::Value,
    bind: Option<BindSpec<'_>>,
    sink: &mut dyn FlowSink,
) -> Result<CallOutcome> {
    let input_hash = sha256_hex(&serde_json::to_string(&input).unwrap_or_default());
    let step = StepId(format!("step_{}_{}", composite.name, &input_hash[..16]));
    store.append_event(
        session_id,
        &RunEvent::StepStarted {
            step: step.clone(),
            op: composite.name.clone(),
            input_hash,
        },
    )?;

    let frame = FrameStore::new(store);
    let args = input.as_object().ok_or_else(|| {
        FlowError::Runtime(format!(
            "composite op `{}` expected object input",
            composite.name
        ))
    })?;
    for param in &composite.params {
        let Some(value) = args.get(&param.name.0) else {
            let error = format!(
                "composite op `{}` missing required param `{}`",
                composite.name, param.name.0
            );
            store.append_event(
                session_id,
                &RunEvent::StepFailed {
                    step,
                    error: error.clone(),
                },
            )?;
            return Ok(CallOutcome {
                value_id: None,
                is_error: true,
                denied: false,
                content: error.clone(),
                view: error,
            });
        };
        frame.seed_param(session_id, param, value)?;
    }

    let outcome = match execute_flow(&frame, executor, session_id, &composite.body, sink).await {
        Ok(o) => o,
        Err(e) => {
            let error = e.to_string();
            store.append_event(
                session_id,
                &RunEvent::StepFailed {
                    step,
                    error: error.clone(),
                },
            )?;
            if e.is_fatal() {
                // Fatality survives the composite boundary: a denied `confirm`/op or a failed
                // `assert` inside a composite op keeps its structural identity instead of being
                // stringified into a retryable in-band error, so an enclosing `retry` will not
                // re-attempt it (F14). The StepFailed audit record above is still written.
                return Err(e);
            }
            return Ok(CallOutcome {
                value_id: None,
                is_error: true,
                denied: false,
                content: error.clone(),
                view: error,
            });
        }
    };
    if outcome.suspension.is_some() {
        let error = format!("composite op `{}` suspended on `await`", composite.name);
        store.append_event(
            session_id,
            &RunEvent::StepFailed {
                step,
                error: error.clone(),
            },
        )?;
        return Ok(CallOutcome {
            value_id: None,
            is_error: true,
            denied: false,
            content: error.clone(),
            view: error,
        });
    }

    let (value_id, content) = match &outcome.returned {
        Some(vid) => {
            let value = store.get_value(vid)?.ok_or_else(|| {
                FlowError::Runtime(format!(
                    "composite op `{}` returned dangling value {}",
                    composite.name, vid.0
                ))
            })?;
            (vid.clone(), value_text(&value))
        }
        None => {
            let content = outcome.result.clone();
            let vid = store.put_value(session_id, &Value::String(content.clone()))?;
            (vid, content)
        }
    };
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
            &summarize_bound_result(&composite.name, &input, &content),
            b.visibility,
        )?;
    }

    let view = if let Some(name) = &composite.meta.view {
        frame
            .resolve(session_id, &SymbolName(name.clone()))?
            .and_then(|vid| store.get_value(&vid).ok().flatten())
            .map(|v| value_text(&v))
            .unwrap_or_else(|| content.clone())
    } else if outcome.transcript.trim().is_empty() {
        content.clone()
    } else {
        format!("{}\n\n[return]\n{}", outcome.transcript, content)
    };

    Ok(CallOutcome {
        value_id: Some(value_id),
        is_error: false,
        denied: false,
        content,
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
    /// Set when the **resumable** execution mode ([`execute_flow_resumable`]) reified a top-level
    /// statement's failure instead of propagating it as `Err` — the model-facing halt a loop host
    /// feeds back for patch-and-continue (`docs/designs/multipass-agent-loop.md` Part 2). Always
    /// `None` for [`execute_flow`]/[`resume_flow`] (strict mode never sets this) and for a resumable
    /// run that completed without failing.
    pub failure: Option<PlanHalt>,
}

/// A top-level statement's failure, reified by [`execute_flow_resumable`] instead of propagated as
/// `Err`. Self-contained: it carries the plan's identity (`plan`, the [`flow_key`]) alongside the
/// failing statement's position (`node`), content identity (`stmt`, its `stmt_hash16`), the op it
/// dispatched (when derivable), how the failure classifies, and its message — everything a loop host
/// needs to build model-facing feedback without re-deriving anything from the flow body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanHalt {
    /// The top-level index of the statement that failed.
    pub node: NodeId,
    /// The failed statement's `stmt_hash16` (see [`stmt_hash16`]).
    pub stmt: String,
    /// The op the statement dispatched, when directly derivable (a bare `call` or a `$x = call(...)`
    /// bind); `None` for a composite/control-flow statement (`when`, `saga`, …) where no single op
    /// identifies the failure.
    pub op: Option<String>,
    /// How the failure classifies (mirrors [`crate::error::FlowError::is_fatal`] via
    /// [`FailureKind::is_fatal`]).
    pub kind: FailureKind,
    /// The failure's human-readable message (`FlowError`'s `Display`).
    pub message: String,
    /// The plan's `flow_key` — the identity a later [`ResumeLedger`] is scoped to.
    pub plan: String,
}

/// One completed top-level statement recorded for resume — the durable memory
/// [`execute_flow_resumable`] fast-forwards against. `value` is the value id the statement produced
/// (`None` for a statement that bound nothing), rehydrated into the statement's bind when the
/// statement is skipped on a later resumed run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// The statement's top-level index in the plan it was recorded against.
    pub node: NodeId,
    /// The statement's `stmt_hash16` (see [`stmt_hash16`]).
    pub stmt: String,
    pub value: Option<ValueId>,
}

/// The completed-statement memory a resumable run fast-forwards against. An edited re-emission of a
/// halted plan pairwise-compares its top-level statements' content hashes against `completed` (keyed
/// by top-level index, not vector position — a `checkpoint`/`await` node never ledgers, so a
/// completed run can have gaps) and skips the longest matching prefix from index 0 (see
/// [`execute_flow_resumable`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResumeLedger {
    pub completed: Vec<LedgerEntry>,
    /// The prior (halted) plan's `flow_key` this ledger was recorded against — informational only;
    /// the fast-forward itself is driven purely by per-statement content-hash comparison, so an
    /// edited-but-key-identical prior plan still matches statement-by-statement.
    pub prior_plan: String,
}

impl ResumeLedger {
    /// Fold a session's run-event log for the **open halt latch**: the last [`RunEvent::PlanHalted`]
    /// with no later [`RunEvent::PlanResumed`] for the same plan (mirrors
    /// [`crate::store::DurableStore::once_lookup`]'s fold pattern) — then gather that plan's
    /// [`RunEvent::StatementCompleted`] entries into a ready-to-resume ledger. `None` means there is
    /// nothing to resume: the session never halted, or the halt was already consumed.
    pub fn fold(events: &[RunEvent]) -> Option<Self> {
        let mut open: Option<&str> = None;
        for ev in events {
            match ev {
                RunEvent::PlanHalted { plan, .. } => open = Some(plan.as_str()),
                // `PlanResumed { plan, prior, .. }`: `plan` is the NEW (resuming) plan's key, `prior`
                // is the OLD (halted) plan's key it closes — the latch check compares `open` (the
                // open halt's OWN key) against `prior`, not `plan`. A bug here (comparing against
                // `plan` instead) would only ever clear the latch when the resuming plan happens to
                // be byte-identical to the one that halted (`plan == prior`) — the common
                // patch-and-continue case (an edited or unrelated re-emission) would leave a stale
                // latch open forever.
                RunEvent::PlanResumed { prior, .. } if open == Some(prior.as_str()) => open = None,
                _ => {}
            }
        }
        let plan = open?.to_string();
        let completed = events
            .iter()
            .filter_map(|ev| match ev {
                RunEvent::StatementCompleted {
                    plan: p,
                    node,
                    stmt,
                    value,
                    ..
                } if *p == plan => Some(LedgerEntry {
                    node: *node,
                    stmt: stmt.clone(),
                    value: value.clone(),
                }),
                _ => None,
            })
            .collect();
        Some(ResumeLedger {
            completed,
            prior_plan: plan,
        })
    }
}

impl FailureKind {
    /// Classify a resumable-mode top-level failure from the existing `FlowError` variants — L-21's
    /// `FlowError::Denied` and the `Core` wraps of `ConfirmDenied`/`AssertFailed` are the fatal set;
    /// everything else is the retryable `Runtime` default. Never a new failure mode: this only labels
    /// what [`FlowError::is_fatal`] already distinguishes, so a halt's `kind`/`is_fatal()` can never
    /// drift from it.
    fn classify(err: &FlowError) -> Self {
        match err {
            FlowError::Denied(_) => FailureKind::Denied,
            FlowError::Core(Error::ConfirmDenied(_)) => FailureKind::ConfirmDenied,
            FlowError::Core(Error::AssertFailed(_)) => FailureKind::AssertFailed,
            _ => FailureKind::Runtime,
        }
    }
}

/// The symbols a top-level statement directly binds runtime values to (plus each declared type, when
/// known) — used to rehydrate a skipped statement's recorded value back into scope on ledger
/// fast-forward, and to recognise (in the fast-forward guard) that a statement HAS a binding to lose.
/// Deliberately narrow (no recursion into nested bodies, unlike `analyze::collect_bound_symbols`):
/// only the statement's OWN binder(s) matter here, because a skipped statement's nested work never
/// ran again either. Returns a `Vec` because a `parallel` binds one symbol per branch (L-28); every
/// other kind binds at most one.
fn top_level_bind(node: &Node) -> Vec<(&SymbolName, Option<&TypeRef>)> {
    match node {
        Node::Bind { name, ty, .. } => vec![(name, ty.as_ref())],
        Node::Memo { name, ty, .. } => vec![(name, ty.as_ref())],
        Node::Seq { bind: Some(b), .. }
        | Node::Pipe { bind: Some(b), .. }
        | Node::Retry { bind: Some(b), .. }
        | Node::Loop { bind: Some(b), .. }
        | Node::Race { bind: Some(b), .. }
        | Node::Fallback { bind: Some(b), .. }
        | Node::Timeout { bind: Some(b), .. }
        | Node::Budget { bind: Some(b), .. }
        | Node::CapScope { bind: Some(b), .. }
        | Node::Scope { bind: Some(b), .. }
        | Node::Once { bind: Some(b), .. } => vec![(b, None)],
        Node::Each {
            collect: Some(c), ..
        } => vec![(c, None)],
        Node::Repeat {
            collect: Some(c), ..
        } => vec![(c, None)],
        // A `parallel` binds each branch's result to the branch name; every branch is a top-level
        // binder for rehydration/guard purposes (L-28).
        Node::Parallel { branches } => branches.iter().map(|b| (&b.name, None)).collect(),
        _ => Vec::new(),
    }
}

/// The op a top-level statement dispatched, when directly derivable — [`PlanHalt::op`]. `Some` for a
/// bare `call` or a `$x = call(...)` bind; `None` for a composite/control-flow statement (`when`,
/// `saga`, `retry`, …), where no single op identifies the failure.
fn top_level_op(node: &Node) -> Option<String> {
    match node {
        Node::Call { op, .. } => Some(op.clone()),
        Node::Bind { value, .. } => match value.as_ref() {
            Node::Call { op, .. } => Some(op.clone()),
            _ => None,
        },
        _ => None,
    }
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
    /// Whether this branch's structural-trace observations should be emitted (A-39) — captured from
    /// the parent sink's [`FlowSink::trace_structural`] before the branch futures are built, since a
    /// branch closure can't borrow the parent sink.
    trace: bool,
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
    fn trace_structural(&self) -> bool {
        self.trace
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

/// The **resumable** analog of [`execute_flow`]: a failing TOP-LEVEL statement is reified as
/// `Ok(FlowOutcome { failure: Some(PlanHalt{..}), transcript: <prefix> })` instead of propagating
/// `Err` — composites and nested bodies keep strict `Err` propagation and structural fatality (F14)
/// unchanged; reification happens ONLY at this top-level statement boundary. `ledger`, when given
/// (typically [`ResumeLedger::fold`] over the session's run-event log), fast-forwards the longest
/// content-hash-matching completed prefix — rehydrating each skipped statement's recorded value into
/// its bind — before executing from the first divergence. Every completed top-level statement is
/// ledgered ([`RunEvent::StatementCompleted`]), whether freshly dispatched or fast-forwarded past.
///
/// `execute_flow` itself is unchanged and unaffected — this is a separate opt-in entry point (see
/// `docs/designs/multipass-agent-loop.md` Part 2 for the full patch-and-continue semantics). The
/// caller (a loop host) owns finding/passing `ledger` and interpreting `outcome.failure`; this
/// function only does the mechanical fast-forward + reification.
pub async fn execute_flow_resumable(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    ast: &DraftAst,
    sink: &mut dyn FlowSink,
    ledger: Option<&ResumeLedger>,
) -> Result<FlowOutcome> {
    let fk = flow_key(ast.name.as_deref(), &ast.body);
    run_top_level_resumable(store, executor, session_id, &ast.body, &fk, ledger, sink).await
}

/// The binding symbol of the top-level `await` at `halt_node`, if it binds one — so a resumable
/// resume (`flow run --resume`) knows whether it must supply a value before advancing. `None` for a
/// bare `await` (no `$x =`) or a non-await node. (A-58 / F-015.)
pub fn awaited_binding(body: &[Node], halt_node: NodeId) -> Option<String> {
    match body.get(halt_node.0 as usize) {
        Some(Node::Await {
            binding: Some(b), ..
        }) => Some(b.0.clone()),
        _ => None,
    }
}

/// Bind a caller-supplied resume value to the halted top-level `await`'s binding, so a resumable
/// re-run — which fast-forwards *past* the await — finds the awaited symbol bound instead of failing
/// downstream with `unbound symbol` (A-58 / F-015). The value is coerced to the await's declared type
/// exactly as [`resume_flow`] does, then durably bound; the subsequent resumable run's fast-forward
/// leaves it in place. Returns the bound symbol name, or `None` if the await binds nothing (a bare
/// `await` needs no value). Errors if `halt_node` is not an `await`.
pub fn bind_resume_value(
    store: &dyn ValueStore,
    session_id: &str,
    body: &[Node],
    halt_node: NodeId,
    value: serde_json::Value,
) -> Result<Option<String>> {
    let Some(Node::Await {
        binding, as_type, ..
    }) = body.get(halt_node.0 as usize)
    else {
        return Err(FlowError::Runtime(format!(
            "resume value supplied but node {} is not an `await`",
            halt_node.0
        )));
    };
    let coerced = coerce_await_input(Value::from_json(&value), as_type);
    let vid = store.put_value(session_id, &coerced)?;
    match binding {
        Some(b) => {
            let ty_label = as_type.as_ref().map(TypeRef::label);
            store.bind(
                session_id,
                b,
                &vid,
                ty_label.as_deref(),
                &summarize(&value_text(&coerced)),
                Visibility::Visible,
            )?;
            Ok(Some(b.0.clone()))
        }
        None => Ok(None),
    }
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
    resume_flow_named(store, executor, session_id, None, body, at, input, sink).await
}

/// [`resume_flow`] with the flow's declared name, so a *named* flow's resumed run records its
/// checkpoints under the same `flow_key` (name + body hash) that [`execute_flow`] uses — run and
/// resume agree on keys (F17). A caller that did not persist the name passes `None` (equivalent to
/// [`resume_flow`]); the key then falls back to the body hash alone, which still never matches an
/// edited body.
#[allow(clippy::too_many_arguments)]
pub async fn resume_flow_named(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    name: Option<&str>,
    body: &[Node],
    at: crate::ast::NodeId,
    input: Value,
    sink: &mut dyn FlowSink,
) -> Result<FlowOutcome> {
    let fk = flow_key(name, body);
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
                failure: None,
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
                failure: None,
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
        failure: None,
    })
}

/// The resumable-mode top-level statement driver behind [`execute_flow_resumable`]. Mirrors
/// [`run_top_level`] (checkpoint fast-forward, `await` suspend, `return` unwind) with three
/// additions, active ONLY at this top-level statement boundary — composites and nested bodies keep
/// strict `Err` propagation and structural fatality (F14), unchanged:
///
/// 1. **Ledger fast-forward.** Walk `body` from index 0: a `checkpoint`/`await` node is a free
///    pass-through (neither ledgers, and `checkpoint` has its own separate durable cursor, handled
///    below); any other statement must have a matching [`LedgerEntry`] (looked up by top-level index,
///    tolerant of the gaps those pass-through nodes leave) whose `stmt` hash equals the CURRENT body's
///    statement at that index. The walk stops at the first statement with no matching entry — the
///    first divergence; everything from there re-runs. Each matched statement is rehydrated (its
///    recorded value rebound to its symbol, when it has one) and re-ledgered
///    (`StatementCompleted{skipped:true}`), and the halt latch is marked consumed (`PlanResumed`).
///    Composes with the checkpoint cursor as the max of the two: `start = ledger_end.max(checkpoint_next)`.
/// 2. **Reified halt.** A failing statement at or after `start` does NOT propagate `Err` — it is
///    classified into a [`PlanHalt`], `PlanHalted` is appended, and
///    `Ok(FlowOutcome { failure: Some(halt), transcript: <prefix> })` is returned instead.
/// 3. **Reified await (L-24).** A top-level `await` still suspends (`FlowOutcome::suspension`,
///    exactly as [`run_top_level`]) but ALSO opens a halt latch — `PlanHalt{kind: Awaiting}` —
///    over the same completed-prefix ledger, so a plan the model re-emits once the awaited input
///    arrives fast-forwards past the prefix instead of re-running it (design Part 2's loop-side
///    case of `checkpoint`∘`await`). The strict driver [`run_top_level`] is untouched: this is a
///    resumable-mode-only addition.
///
/// Every other top-level statement that completes is ledgered (`StatementCompleted{skipped:false}`).
#[allow(clippy::too_many_arguments)]
async fn run_top_level_resumable(
    store: &dyn ValueStore,
    executor: &dyn OpHost,
    session_id: &str,
    body: &[Node],
    flow_key: &str,
    ledger: Option<&ResumeLedger>,
    sink: &mut dyn FlowSink,
) -> Result<FlowOutcome> {
    let mut steps = 0usize;
    let mut transcript: Vec<String> = Vec::new();
    let mut last = String::new();

    // Index the ledger by top-level position (NOT vector position — `checkpoint`/`await` never
    // ledger, so a completed ledger can have gaps at those positions).
    let by_node: HashMap<u32, &LedgerEntry> = ledger
        .map(|l| l.completed.iter().map(|e| (e.node.0, e)).collect())
        .unwrap_or_default();

    // Walk from index 0 while the ledger keeps matching; `checkpoint`/`await` are a free pass-through
    // (they have no ledger entry of their own — `checkpoint`'s cursor is folded in separately below).
    // Divergence = first hash mismatch (or first statement missing a ledger entry): everything from
    // there re-runs.
    let mut ledger_end = 0usize;
    if ledger.is_some() {
        while ledger_end < body.len() {
            match &body[ledger_end] {
                Node::Checkpoint { .. } | Node::Await { .. } => ledger_end += 1,
                node => match by_node.get(&(ledger_end as u32)) {
                    Some(entry) if entry.stmt == stmt_hash16(node) => ledger_end += 1,
                    _ => break,
                },
            }
        }
    }

    // The durable checkpoint cursor (same mechanism `run_top_level` honors for a fresh run).
    let mut checkpoint_next = 0usize;
    if let Some(d) = store.as_durable() {
        if let Some(cp) = d.checkpoint_resume(session_id, flow_key)? {
            checkpoint_next = (cp.0 as usize + 1).min(body.len());
        }
    }

    let start = ledger_end.max(checkpoint_next);

    // Rehydrate the ledger-matched prefix (bounded by `ledger_end`, not `start` — a jump past
    // `ledger_end` driven by the checkpoint cursor alone rehydrates nothing, exactly like plain
    // `checkpoint` today) and re-ledger each skipped statement so a second halt's fast-forward never
    // needs to walk back further than this run.
    if let Some(l) = ledger {
        for (idx, node) in body.iter().enumerate().take(ledger_end) {
            let Some(entry) = by_node.get(&(idx as u32)) else {
                continue; // a pass-through checkpoint/await position — nothing to ledger here.
            };
            let binds = top_level_bind(node);
            if let Some(vid) = &entry.value {
                match store.get_value(vid)? {
                    Some(value) => {
                        let text = value_text(&value);
                        // Rebind the statement's OWN binder to the recorded value. A single-binder
                        // statement (`$x = …`, a `seq`/`retry`/… `bind`, an `each`/`repeat`
                        // `collect`) rebinds that symbol. A `parallel` records ONE value (its last
                        // branch's) yet binds several symbols, and each branch's binding is already
                        // durable in the store — so the single recorded value never overwrites them;
                        // its arm exists so the guard below still recognises it as a binder.
                        if let [(name, ty)] = binds.as_slice() {
                            let ty_label = ty.map(TypeRef::label);
                            store.bind(
                                session_id,
                                name,
                                vid,
                                ty_label.as_deref(),
                                &summarize(&text),
                                Visibility::Visible,
                            )?;
                        }
                        if !text.is_empty() {
                            last = text;
                        }
                    }
                    // The statement bound symbol(s) but its recorded value is gone from the value
                    // store — fast-forwarding past it would silently drop the binding and a later
                    // statement would die on "unbound symbol". Fail the resume LOUDLY instead (L-28).
                    // Unreachable on the in-session path (the value store is INSERT-only, never
                    // evicted); reachable only under a cross-store / fresh-store resume (L-25).
                    None if !binds.is_empty() => {
                        return Err(FlowError::Runtime(format!(
                            "cannot resume: statement {idx} bound {} but its recorded value `{}` is \
                             missing from the value store — the ledger and value store are out of sync",
                            binds
                                .iter()
                                .map(|(n, _)| format!("${}", n.0))
                                .collect::<Vec<_>>()
                                .join(", "),
                            vid.0,
                        )));
                    }
                    // A statement that produced a value but bound no symbol (a bare `call` whose
                    // result only fed `last`): nothing to rehydrate, nothing lost — carry on.
                    None => {}
                }
            }
            store.append_event(
                session_id,
                &RunEvent::StatementCompleted {
                    plan: flow_key.to_string(),
                    node: NodeId(idx as u32),
                    stmt: entry.stmt.clone(),
                    value: entry.value.clone(),
                    skipped: true,
                },
            )?;
            transcript.push(format!(
                "[fast-forward: statement {idx} already completed, skipped]"
            ));
        }
        store.append_event(
            session_id,
            &RunEvent::PlanResumed {
                plan: flow_key.to_string(),
                prior: l.prior_plan.clone(),
                skipped: ledger_end,
            },
        )?;
    }

    let mut i = start;
    while i < body.len() {
        if let Node::Checkpoint { label } = &body[i] {
            // Record the durable resume cursor, then continue past it (same as `run_top_level`).
            if let Some(d) = store.as_durable() {
                d.checkpoint_record(session_id, flow_key, label, NodeId(i as u32))?;
            }
            transcript.push(format!("[checkpoint {label}]"));
            i += 1;
            continue;
        }
        if let Node::Await { source, .. } = &body[i] {
            let node = NodeId(i as u32);
            store.append_event(
                session_id,
                &RunEvent::Awaiting {
                    run: crate::ast::RunId(format!("{session_id}:{i}")),
                    node,
                },
            )?;
            // L-24: fold the reified await into the SAME halt-latch machinery a top-level failure
            // uses — `PlanHalted{kind: Awaiting}` opens the latch (see `ResumeLedger::fold`) over
            // exactly the prefix already ledgered above (`StatementCompleted` per completed
            // statement), so the plan the model re-emits once the awaited input is available
            // (design Part 2's cross-turn `[resume context]`) fast-forwards past that prefix instead
            // of re-running it. `Awaiting` is never fatal and never matched by the denial
            // re-emission guard (`run_plan_dispatch`'s step 3), so this never blocks a re-emission —
            // and the ledger walk's `Node::Await` free-pass-through means a byte-identical `await`
            // at this position in the new plan costs nothing to skip over.
            let stmt = stmt_hash16(&body[i]);
            let halt = PlanHalt {
                node,
                stmt: stmt.clone(),
                op: None,
                kind: FailureKind::Awaiting,
                message: format!("plan paused: awaiting `{source}`"),
                plan: flow_key.to_string(),
            };
            store.append_event(
                session_id,
                &RunEvent::PlanHalted {
                    plan: flow_key.to_string(),
                    node,
                    stmt,
                    op: None,
                    kind: FailureKind::Awaiting,
                    error: halt.message.clone(),
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
                failure: Some(halt),
            });
        }

        let stmt = stmt_hash16(&body[i]);
        match exec_body(
            store,
            executor,
            session_id,
            std::slice::from_ref(&body[i]),
            sink,
            &mut steps,
            &mut transcript,
        )
        .await
        {
            Ok((blast, bvid, step)) => {
                store.append_event(
                    session_id,
                    &RunEvent::StatementCompleted {
                        plan: flow_key.to_string(),
                        node: NodeId(i as u32),
                        stmt,
                        value: bvid.clone(),
                        skipped: false,
                    },
                )?;
                if let Step::Return(vid) = step {
                    if let Some(v) = &vid {
                        store.append_event(
                            session_id,
                            &RunEvent::FlowReturned { value: v.clone() },
                        )?;
                    }
                    let result = if vid.is_some() { blast } else { last };
                    return Ok(FlowOutcome {
                        returned: vid,
                        result,
                        transcript: transcript.join("\n\n"),
                        steps,
                        suspension: None,
                        failure: None,
                    });
                }
                if !blast.is_empty() {
                    last = blast;
                }
            }
            Err(e) => {
                let kind = FailureKind::classify(&e);
                let op = top_level_op(&body[i]);
                let halt = PlanHalt {
                    node: NodeId(i as u32),
                    stmt: stmt.clone(),
                    op: op.clone(),
                    kind,
                    message: e.to_string(),
                    plan: flow_key.to_string(),
                };
                store.append_event(
                    session_id,
                    &RunEvent::PlanHalted {
                        plan: flow_key.to_string(),
                        node: NodeId(i as u32),
                        stmt,
                        op,
                        kind,
                        error: e.to_string(),
                    },
                )?;
                return Ok(FlowOutcome {
                    returned: None,
                    result: last,
                    transcript: transcript.join("\n\n"),
                    steps,
                    suspension: None,
                    failure: Some(halt),
                });
            }
        }
        i += 1;
    }

    Ok(FlowOutcome {
        returned: None,
        result: last,
        transcript: transcript.join("\n\n"),
        steps,
        suspension: None,
        failure: None,
    })
}

/// Coerce a resume `input` against the `await`'s declared `as_type` — lenient, like the type checker:
/// a string reply is parsed to a number/bool when the await asked for one; everything else is kept
/// verbatim (no hard failures — an un-coercible value flows through as-is).
/// A transcript-legible rendering of an op's view: an empty view (a command that succeeded with no
/// output — `rm`, a clean formatter, an empty match set) becomes an explicit success marker instead
/// of a blank line. Without it the loop feedback for a silent success reads exactly like "nothing
/// happened", and the model re-runs the already-succeeded op (A-05).
fn legible_view(view: &str) -> &str {
    if view.trim().is_empty() {
        "✓ ok (no output)"
    } else {
        view
    }
}

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
        failure: None,
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
                    // Pure value nodes (expr/fmt/jq/parse/var/lit/obj/list) evaluate through the ONE
                    // shared helper — the same path statement position uses, so bind and statement
                    // semantics can never diverge again (F13). They are side-effect-free and need no
                    // dispatch envelope.
                    if let Some(pe) = eval_pure_node(value.as_ref(), store, session_id)? {
                        let ty_label = ty.as_ref().map(TypeRef::label);
                        store.bind(
                            session_id,
                            name,
                            &pe.vid,
                            ty_label.as_deref(),
                            &summarize(&pe.text),
                            Visibility::Visible,
                        )?;
                        transcript.push(format!("[${} = {}]\n{}", name.0, pe.tag, pe.text));
                        last = pe.text;
                        last_value = Some(pe.vid);
                        continue;
                    }
                    if let Node::Thing { thing } = value.as_ref() {
                        // Resolve the external reference to an exact identity through the host's
                        // resolver (the deterministic default handles self-identifying selectors),
                        // record it in the run trace, and bind the resolved `Thing` value. Not part
                        // of the pure-node helper: resolution is async and host-mediated.
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
                    let Node::Call { op, args } = value.as_ref() else {
                        return Err(crate::FlowError::Runtime(
                            "execution can only bind the result of a `call`, `expr`, `fmt`, `jq`, `parse`, `peek`, `var` ($symbol), `lit`, or a `thing` reference".to_string(),
                        ));
                    };
                    let ty_label = ty.as_ref().map(TypeRef::label);
                    let bind = BindSpec {
                        name,
                        ty: ty_label.as_deref(),
                        visibility: Visibility::Visible,
                    };
                    // Emitted BEFORE dispatch so the trace line appears while the op (e.g. the
                    // planner) is running, not only after it returns (A-39).
                    trace_structural(
                        sink,
                        "loop.node",
                        serde_json::json!({"node": "call", "op": op, "bind": name.0}),
                    );
                    let outcome =
                        run_call(store, executor, session_id, op, args, Some(bind), sink).await?;
                    *steps += 1;
                    if outcome.is_error {
                        return Err(call_failure(&format!("step `{op}`"), &outcome));
                    }
                    // The model reasons over intermediate results → feed the model-facing VIEW
                    // (line-numbered read, diff, …). Control flow (`when`/`return`) stays canonical.
                    // Record EVERY node's view in the transcript so the round feedback surfaces all of
                    // a plan's reads, not just the last one. Oversized views are trimmed so one huge
                    // result can't blow the round's context budget (the canonical value is untouched).
                    let view = executor.trim_output(outcome.view.clone(), op);
                    transcript.push(format!("[${} = {op}]\n{}", name.0, legible_view(&view)));
                    last = outcome.view;
                    last_value = outcome.value_id;
                }
                Node::Call { op, args } => {
                    trace_structural(
                        sink,
                        "loop.node",
                        serde_json::json!({"node": "call", "op": op}),
                    );
                    let outcome =
                        run_call(store, executor, session_id, op, args, None, sink).await?;
                    *steps += 1;
                    if outcome.is_error {
                        return Err(call_failure(&format!("step `{op}`"), &outcome));
                    }
                    // The model reasons over intermediate results → feed the model-facing VIEW
                    // (line-numbered read, diff, …). Control flow (`when`/`return`) stays canonical.
                    // Oversized views are trimmed (canonical value untouched).
                    let view = executor.trim_output(outcome.view.clone(), op);
                    transcript.push(format!("[{op}]\n{}", legible_view(&view)));
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
                        store, session_id, name, purpose, &members, *budget, transcript, sink,
                    )?;
                    last = format!("ctx {}", name.0);
                    last_value = Some(vid);
                }
                Node::CtxAppend { ctx, add } => {
                    let vid = append_ctx(store, session_id, ctx, add, transcript, sink)?;
                    last = format!("ctx += {}", ctx.0);
                    last_value = Some(vid);
                }
                Node::Return { value } => {
                    let mut data = serde_json::json!({"node": "return"});
                    if let Some(l) = var_label(value) {
                        data["value"] = serde_json::Value::String(l);
                    }
                    trace_structural(sink, "loop.node", data);
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
                    let mut data = serde_json::json!({
                        "node": "when",
                        "branch": if take { "then" } else { "else" },
                    });
                    if let Some(l) = var_label(cond) {
                        data["cond"] = serde_json::Value::String(l);
                    }
                    trace_structural(sink, "loop.node", data);
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
                    }
                    // The branch's value is the node's value even when its text renders empty (F20b).
                    last_value = bvid;
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
                    for round in 0..*max {
                        // Otherwise-discarded round counter (A-39): 1-based, live-only.
                        trace_structural(
                            sink,
                            "loop.round",
                            serde_json::json!({"round": round + 1, "max": max}),
                        );
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
                                trace_structural(
                                    sink,
                                    "loop.node",
                                    serde_json::json!({
                                        "node": "repeat",
                                        "until_hit": true,
                                        "rounds": round + 1,
                                        "max": max,
                                    }),
                                );
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
                    // Same string-leaf re-parse rule as templates/`jq` (C-10): a symbol holding a
                    // JSON array (values are stored as JSON *strings* — e.g. a `glob` result) is a
                    // valid `each` source.
                    let list = jq_parse_input(eval_arg(source, store, session_id)?);
                    let serde_json::Value::Array(elems) = list else {
                        return Err(crate::FlowError::Runtime(
                            "`each` source must evaluate to a list".to_string(),
                        ));
                    };
                    // The item symbol is loop-scoped: save any outer binding of the same name so the
                    // loop variable doesn't clobber it past the loop (F20d). Restored on normal
                    // completion; with no prior binding the last item stays bound (the store has no
                    // unbind).
                    let saved_item_vid = store.resolve(session_id, item)?;
                    let saved_item_meta = store.binding(session_id, item)?;
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
                    // Restore the outer binding the loop variable shadowed (F20d), with its original
                    // metadata; done before `collect` binds so an explicit collect symbol wins.
                    if let Some(prev_vid) = &saved_item_vid {
                        match &saved_item_meta {
                            Some(m) => store.bind(
                                session_id,
                                item,
                                prev_vid,
                                m.ty.as_deref(),
                                &m.summary,
                                m.visibility,
                            )?,
                            None => bind_existing(store, session_id, item, prev_vid)?,
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
                            return Err(call_failure(&format!("pipe step `{op}`"), &outcome));
                        }
                        // Transcript views are trimmed to the host's output budget, like every
                        // other transcript push (F20a); the canonical value is untouched.
                        let view = executor.trim_output(outcome.view.clone(), op);
                        transcript.push(format!("[pipe {op}]\n{view}"));
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
                        return Err(call_failure(&format!("step `{op}`"), &outcome));
                    }
                    // Transcript views are trimmed to the host's output budget, like every other
                    // transcript push (F20a); the canonical value is untouched.
                    let view = executor.trim_output(outcome.view.clone(), op);
                    transcript.push(format!("[${} = memo {op}]\n{view}", name.0));
                    last = outcome.view;
                    last_value = outcome.value_id;
                }
                Node::Parallel { branches } => {
                    // Run each branch concurrently, each writing to its own buffering sink; after the
                    // join, merge the buffers/steps/transcripts into the real sink in DECLARATION
                    // order so concurrent output doesn't interleave. Every branch runs to completion
                    // (`join_all`, not `try_join_all`): a sibling's failure must not drop a completed
                    // branch's buffered output, dispatched-step count, or binding — the audit is
                    // merged first, then the first (declaration-order) error propagates (F15). Every
                    // op still dispatches through the same envelope.
                    // Captured before the branch futures are built (A-39): a branch closure can't
                    // borrow `sink`, so each branch's own `BufferSink` carries the parent's opt-in
                    // forward instead of defaulting to untraced.
                    let trace = sink.trace_structural();
                    let futs = branches.iter().map(|b| async move {
                        let mut buf = BufferSink {
                            trace,
                            ..Default::default()
                        };
                        trace_structural(
                            &mut buf,
                            "loop.node",
                            serde_json::json!({"node": "parallel.branch", "name": b.name.0.clone()}),
                        );
                        let mut s = 0usize;
                        let mut tr: Vec<String> = Vec::new();
                        let res = exec_body(
                            store, executor, session_id, &b.body, &mut buf, &mut s, &mut tr,
                        )
                        .await;
                        (b, buf, s, tr, res)
                    });
                    let results = futures::future::join_all(futs).await;
                    let mut first_err: Option<FlowError> = None;
                    for (b, buf, s, tr, res) in results {
                        buf.replay(&mut *sink);
                        *steps += s;
                        transcript.extend(tr);
                        match res {
                            Ok((text, lv, step)) => {
                                if let Step::Return(_) = step {
                                    return Err(FlowError::Runtime(
                                        "`return` is not allowed inside a `parallel` branch"
                                            .to_string(),
                                    ));
                                }
                                if let Some(vid) = lv {
                                    bind_existing(store, session_id, &b.name, &vid)?;
                                    last = text;
                                    last_value = Some(vid);
                                }
                            }
                            Err(e) => {
                                if first_err.is_none() {
                                    first_err = Some(e);
                                }
                            }
                        }
                    }
                    if let Some(e) = first_err {
                        return Err(e);
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
                                // Fatal errors (denied confirm, failed assert) must not be retried —
                                // propagate immediately. Checked structurally: the wrap points
                                // (`loop`, composite ops) preserve fatal identity, so this sees
                                // through nesting (F14).
                                if e.is_fatal() {
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
                                // A fatal error keeps its structural identity so an enclosing
                                // `retry` still sees it (F14); only transient failures get the
                                // `loop` wrap.
                                if e.is_fatal() {
                                    return Err(e);
                                }
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
                    // True first-wins concurrency with an honest audit (F16): branches run
                    // concurrently; the first *success* wins and the losers are cancelled — but
                    // every branch's dispatched-step count and transcript survive (each branch's
                    // state lives OUTSIDE its future, so a cancelled loser's already-dispatched
                    // work stays counted; an enclosing `budget` sees it — audit parity with the
                    // event log). All-branches-failed is a joined branch error, distinct from a
                    // timeout.
                    // A zero deadline can accommodate no work: `tokio::time::timeout` polls the
                    // inner future before the (already-elapsed) timer, so an immediately-ready
                    // branch would otherwise win a 0ms race. Short-circuit so the deadline holds.
                    if *timeout_ms == 0 {
                        return Err(FlowError::Runtime(format!(
                            "`race` timed out after {timeout_ms}ms with no successful branch"
                        )));
                    }
                    use futures::StreamExt;
                    let mut states: Vec<(BufferSink, usize, Vec<String>)> = branches
                        .iter()
                        .map(|_| (BufferSink::default(), 0usize, Vec::new()))
                        .collect();
                    let mut errs: Vec<String> = Vec::new();
                    // `winner` is `Option<(branch index, (text, last value, step))>`.
                    let winner = {
                        let mut pending: futures::stream::FuturesUnordered<_> = branches
                            .iter()
                            .zip(states.iter_mut())
                            .enumerate()
                            .map(|(idx, (b, (buf, s, tr)))| async move {
                                (
                                    idx,
                                    exec_body(store, executor, session_id, &b.body, buf, s, tr)
                                        .await,
                                )
                            })
                            .collect();
                        let remaining = std::time::Duration::from_millis(*timeout_ms);
                        tokio::time::timeout(remaining, async {
                            while let Some((idx, res)) = pending.next().await {
                                match res {
                                    Ok(v) => return Some((idx, v)),
                                    Err(e) => errs.push(format!("`{}`: {e}", branches[idx].name.0)),
                                }
                            }
                            None
                        })
                        .await
                        .ok()
                        .flatten()
                        // `pending` (and with it every loser future) is dropped here — the losers
                        // are cancelled, but their buffered state persists in `states`.
                    };
                    // Merge every branch's audit in declaration order: steps and transcript from ALL
                    // branches (a loser's dispatched work happened); sink output only from the
                    // winner (a cancelled loser's buffer may end mid-op).
                    let winner_idx = winner.as_ref().map(|(idx, _)| *idx);
                    for (idx, (buf, s, tr)) in states.into_iter().enumerate() {
                        *steps += s;
                        transcript.extend(tr);
                        if Some(idx) == winner_idx {
                            buf.replay(&mut *sink);
                        }
                    }
                    let Some((_, (blast, bvid, step))) = winner else {
                        // No branch succeeded: distinguish "every branch errored" (all failures
                        // arrived before the deadline) from an actual timeout.
                        if !branches.is_empty() && errs.len() == branches.len() {
                            return Err(FlowError::Runtime(format!(
                                "`race` failed: all {} branch(es) errored: {}",
                                branches.len(),
                                errs.join("; ")
                            )));
                        }
                        return Err(FlowError::Runtime(format!(
                            "`race` timed out after {timeout_ms}ms with no successful branch"
                        )));
                    };
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
                    // Sliding-window rate limit on **op dispatches** (budget-style counting, F18):
                    // before each body statement the keyed bucket must have room; after it, the
                    // statement's actual dispatches (the `steps` delta — the same counter `budget`
                    // uses) are recorded as timestamps. The bucket lives in the session store keyed
                    // by `name` (persists across turns; different names never share), and every
                    // load-evict-store cycle runs atomically under a process-wide lock so
                    // concurrent branches can't interleave a read-modify-write. Like `budget`, a
                    // single statement may still overshoot within itself — documented v1.
                    let bucket_key = SymbolName(format!("__throttle_bucket_{tname}"));
                    let mut tvid: Option<ValueId> = None;
                    for stmt in tbody {
                        let now_ms = now_millis();
                        let window_start = now_ms.saturating_sub(*window_ms);
                        let in_window = throttle_bucket_rmw(
                            store,
                            session_id,
                            &bucket_key,
                            window_start,
                            0,
                            0,
                        )?;
                        if in_window >= *max as usize {
                            return Err(FlowError::Runtime(format!(
                                "`throttle` limit of {max} per {window_ms}ms exceeded"
                            )));
                        }
                        let start = *steps;
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
                        let dispatched = (*steps).saturating_sub(start);
                        if dispatched > 0 {
                            let now_ms = now_millis();
                            let window_start = now_ms.saturating_sub(*window_ms);
                            throttle_bucket_rmw(
                                store,
                                session_id,
                                &bucket_key,
                                window_start,
                                now_ms,
                                dispatched,
                            )?;
                        }
                        if !blast.is_empty() {
                            last = blast;
                        }
                        if let Some(v) = svid {
                            tvid = Some(v);
                        }
                        if let Step::Return(v) = step {
                            return Ok((last, v.clone(), Step::Return(v)));
                        }
                    }
                    last_value = tvid;
                }
                Node::Debounce {
                    name: dname,
                    wait_ms,
                    body: dbody,
                } => {
                    // Keyed cross-turn coalescing (F19): each execution of this node is a *trigger*.
                    // A trigger records a unique token (embedding its timestamp) under the
                    // per-`name` key in the session store, waits `wait_ms`, then runs the body only
                    // if no NEWER trigger arrived meanwhile (its token is still current). A
                    // re-trigger within the window therefore resets it: the older trigger sees the
                    // newer token and coalesces (skips), and the newest trigger alone runs the body
                    // once the key has been quiet for `wait_ms`. State persists across turns via the
                    // session store — the same seam the throttle bucket uses; no new IO.
                    let debounce_key = SymbolName(format!("__debounce_last_{dname}"));
                    let token = debounce_record_trigger(store, session_id, &debounce_key)?;
                    if *wait_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(*wait_ms)).await;
                    }
                    if debounce_current_token(store, session_id, &debounce_key)?.as_deref()
                        != Some(token.as_str())
                    {
                        // A newer trigger superseded this one within the window — coalesce.
                        transcript.push(format!(
                            "[debounce {dname} coalesced — re-triggered within {wait_ms}ms]"
                        ));
                        continue;
                    }
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
                    let mut data = serde_json::json!({"node": "unless", "entered": take});
                    if let Some(l) = var_label(cond) {
                        data["cond"] = serde_json::Value::String(l);
                    }
                    trace_structural(sink, "loop.node", data);
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
                        }
                        // The body's value is the node's value even when its text is empty (F20b).
                        last_value = bvid;
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
                Node::Expr { .. } | Node::Fmt { .. } | Node::Jq { .. } | Node::Parse { .. } => {
                    // Pure computation — no IO, no approval gate. Evaluated through the SAME shared
                    // helper as bind position (F13: statement-position `jq` used to skip the
                    // string-stored-JSON re-parse the bind arm did, so the two diverged).
                    let pe = eval_pure_node(node, store, session_id)?
                        .expect("expr/fmt/jq/parse are pure value nodes");
                    transcript.push(format!("[{}]\n{}", pe.tag, pe.text));
                    last = pe.text;
                    last_value = Some(pe.vid);
                }
                Node::Match {
                    subject,
                    cases,
                    default,
                } => {
                    let subj = eval_arg(subject, store, session_id)?;
                    let mut branch: Option<&[Node]> = None;
                    let mut matched = false;
                    for case in cases {
                        if eval_arg(&case.value, store, session_id)? == subj {
                            branch = Some(&case.body);
                            matched = true;
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
                    let arm = if matched {
                        format!("case {}", serde_json::to_string(&subj).unwrap_or_default())
                    } else {
                        "default".to_string()
                    };
                    let mut trace_data = serde_json::json!({
                        "node": "match",
                        "value": truncate_json(&subj, 60),
                        "arm": arm,
                    });
                    if let Some(l) = var_label(subject) {
                        trace_data["subject"] = serde_json::Value::String(l);
                    }
                    trace_structural(sink, "loop.node", trace_data);
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
                    }
                    // The case's value is the node's value even when its text renders empty (F20b).
                    last_value = bvid;
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
                                return Err(call_failure(
                                    &format!("`route` selector `{op}`"),
                                    &outcome,
                                ));
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
                    }
                    // The case's value is the node's value even when its text renders empty (F20b).
                    last_value = bvid;
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
                Node::CapScope {
                    tools,
                    body: csbody,
                    bind,
                } => {
                    // Push → run body → ALWAYS pop, mirroring `Scope`'s acquire/body/finally RAII: the
                    // enforcement itself lives entirely in `executor.dispatch` (through `push_cap_scope`
                    // narrowing the host's cap-scope stack), so this node's only job is to open/close
                    // the window around its body — even when the body errors or returns early. The pop
                    // runs via `?`-transparent early return below, exactly like `finally` always running.
                    executor.push_cap_scope(tools).await;
                    let body_res = exec_body(
                        store,
                        executor,
                        session_id,
                        csbody,
                        &mut *sink,
                        &mut *steps,
                        &mut *transcript,
                    )
                    .await;
                    executor.pop_cap_scope().await;
                    let (blast, bvid, step) = body_res?;
                    if !blast.is_empty() {
                        last = blast;
                    }
                    last_value = bvid.clone();
                    if let (Some(name), Some(vid)) = (bind, &bvid) {
                        bind_existing(store, session_id, name, vid)?;
                    }
                    if let Step::Return(v) = step {
                        return Ok((last, v.clone(), Step::Return(v)));
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
                Node::Var { .. }
                | Node::Lit { .. }
                | Node::Thing { .. }
                | Node::Obj { .. }
                | Node::List { .. } => {
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
                // A denial is a deliberate refusal, not a falsy answer: reading it as `false`
                // would silently absorb it — and a `repeat`/`loop` `until` guard would re-dispatch
                // (re-prompt) the denied op every iteration (L-21). Any other failed condition op
                // stays falsy (the op ran and errored — "not confirmed").
                if outcome.denied {
                    return Err(FlowError::Denied(outcome.content));
                }
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

/// Emit a live-only structural-trace observation (`loop.round` / `loop.node`, A-39) when the sink
/// has opted in via [`FlowSink::trace_structural`] — checked first so an opted-out sink (the
/// default) pays no cost, not even the `data` allocation. Goes through `sink.observation` directly,
/// never `executor.observe`, so it never touches the value store, the run-event trail, or the
/// evidence log: it is purely a transient signal for whatever live sink is attached to this
/// execution. Unhandled structural kinds (`each`/`retry`/`try`/`route`/`race`/`seq`/`loop`) can
/// extend this trace with the same one-line call at their own emission site.
fn trace_structural(sink: &mut dyn FlowSink, kind: &str, data: serde_json::Value) {
    if !sink.trace_structural() {
        return;
    }
    sink.observation(&flux_evidence::Observation::new(
        kind,
        flux_evidence::Phase::Turn,
        data,
    ));
}

/// A short `$name` label for a plain-var expression — used to annotate a `loop.node` trace line with
/// what a cond/subject/return operated on. `None` when the expression isn't a simple var reference
/// (e.g. `when !done`, `return "literal"`), so the trace line omits the field rather than inventing a
/// label for it.
fn var_label(n: &Node) -> Option<String> {
    if let Node::Var { name } = n {
        Some(format!("${}", name.0))
    } else {
        None
    }
}

/// The JSON text of `v`, truncated (char-safe) to `max` characters — keeps a `loop.node` `match`
/// trace line legible even when the subject is a long string/object (A-39).
fn truncate_json(v: &serde_json::Value, max: usize) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.chars().count() <= max {
        s
    } else {
        let mut truncated: String = s.chars().take(max).collect();
        truncated.push('…');
        truncated
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
    let composite = executor.catalog().composite(op);
    let outcome = if let Some(composite) = composite {
        execute_composite_call(store, executor, session_id, &composite, input, bind, sink).await?
    } else {
        execute_call(store, executor, session_id, op, input, bind).await?
    };
    // Surface the model-facing VIEW (numbered read, diff, …) to the sink — what the model/user sees.
    // The canonical `outcome.content` remains what control flow and interpolation use.
    sink.tool_result(
        op,
        &OpOutcome {
            content: outcome.view.clone(),
            view: None,
            is_error: outcome.is_error,
            denied: outcome.denied,
        },
    );
    Ok(outcome)
}

/// A pure value node evaluated to its canonical text, stored value, and transcript tag.
struct PureEval {
    /// The transcript tag describing the node (e.g. `expr a + b`, `jq .k`, `fmt`, `lit`).
    tag: String,
    /// The canonical text of the produced value.
    text: String,
    /// The stored value id — a fresh immutable value, or the aliased source's id for `var`.
    vid: ValueId,
}

/// Evaluate a **pure value node** — `expr` / `fmt` / `jq` / `parse` / `var` / `lit` / `obj` /
/// `list` — to its canonical text and stored value. This is the ONE eval path shared by bind
/// position (`$x = jq(...)`) and statement position (bare `jq(...)`), so the two can never diverge
/// again (F13: the statement-position `jq` arm skipped the string-stored-JSON re-parse the bind arm
/// did, so `jq` over an op result errored as a statement but worked as a bind).
///
/// Returns `Ok(None)` for any non-pure node so callers fall through to their own handling. No op
/// dispatch, no approval gate — `thing` (host resolver, async) and `call` are NOT handled here.
fn eval_pure_node(
    node: &Node,
    store: &dyn ValueStore,
    session_id: &str,
) -> Result<Option<PureEval>> {
    let (tag, text, existing_vid) = match node {
        Node::Expr { formula, vars } => {
            let resolved = resolve_expr_vars(vars, store, session_id)?;
            let val = eval_expr_value(formula, &resolved)?;
            // A scalar number/bool result keeps its JSON type (`$ok = expr($a > $b)` binds the
            // boolean, `$n = expr($x + 1)` the number), mirroring the `Lit`/`Jq` arms (F-008).
            // Strings and lists stay canonical text; a non-finite number falls back to text too.
            let existing_vid = match &val {
                ExprVal::Num(n) => serde_json::Number::from_f64(*n)
                    .map(|num| {
                        store.put_value(
                            session_id,
                            &Value::from_json(&serde_json::Value::Number(num)),
                        )
                    })
                    .transpose()?,
                ExprVal::Bool(b) => Some(
                    store.put_value(session_id, &Value::from_json(&serde_json::Value::Bool(*b)))?,
                ),
                _ => None,
            };
            (format!("expr {formula}"), val.as_text(), existing_vid)
        }
        Node::Fmt { template } => (
            "fmt".to_string(),
            interpolate_str(template, store, session_id),
            None,
        ),
        Node::Jq {
            path,
            input,
            optional,
        } => {
            // Op results are stored as JSON *strings* (the canonical `content`), so a string input
            // that is really JSON is parsed first — this is what lets a flow pull
            // `.kind`/`.transcript` out of a `plan`/`run_plan` result.
            let jv = jq_parse_input(eval_arg(input, store, session_id)?);
            let result = eval_jq_path(path, &jv, *optional)?;
            // A scalar number/bool field extract keeps its JSON type (`$n = $obj.n` binds the
            // number 42, not `"42"`), mirroring the `Lit` arm — so a rebuilt object stays typed
            // and typed `match` arms / structured output see the real value (F-008). Strings,
            // `null` (the null→`""` truthiness idiom), and object/array stay canonical
            // JSON-as-string.
            let existing_vid = match &result {
                serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {
                    Some(store.put_value(session_id, &Value::from_json(&result))?)
                }
                _ => None,
            };
            (format!("jq {path}"), lit_text(&result), existing_vid)
        }
        Node::Parse {
            value: inner,
            as_type,
        } => {
            let jv = eval_arg(inner, store, session_id)?;
            let text = coerce_parse(&jv, as_type)?;
            // A numeric/bool parse target binds the typed value (`parse($x, i64)` → 42, not "42"),
            // mirroring the other pure-node binds (F-008). `coerce_parse` already returns canonical
            // text that is a JSON number / `true` / `false` for these targets, so recover the type
            // from it; `string`/`json`/unknown stay canonical text.
            let existing_vid = match as_type.as_str() {
                "f64" | "i64" | "bool" => serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .filter(|v| v.is_number() || v.is_boolean())
                    .map(|v| store.put_value(session_id, &Value::from_json(&v)))
                    .transpose()?,
                _ => None,
            };
            (format!("parse {as_type}"), text, existing_vid)
        }
        Node::Var { name: src } => {
            // Alias a symbol: `$b = $a` shares `$a`'s current ValueId. Pure — values are immutable,
            // so sharing the id is sound (the body-level twin of the optimizer's `Stage::Alias`).
            let vid = store
                .resolve(session_id, src)?
                .ok_or_else(|| FlowError::Runtime(format!("unbound symbol ${}", src.0)))?;
            let text = store
                .get_value(&vid)?
                .map(|v| value_text(&v))
                .unwrap_or_default();
            (format!("${}", src.0), text, Some(vid))
        }
        Node::Peek { name } => {
            // A **soft** read of a named symbol — like `var`, but tolerates an unbound name (→ empty
            // string) instead of erroring, so a flow can probe durable state and bind the result
            // (F-009: `$prev = peek(last_result)`). Shares the resolved value id when the symbol is
            // bound; an unbound peek falls through to a fresh empty-string value below.
            match store.resolve(session_id, name)? {
                Some(vid) => {
                    let text = store
                        .get_value(&vid)?
                        .map(|v| value_text(&v))
                        .unwrap_or_default();
                    (format!("peek ${}", name.0), text, Some(vid))
                }
                None => ("peek".to_string(), String::new(), None),
            }
        }
        Node::Lit { value } => {
            // A literal: `5`, `"hi"`, `[1,2,3]`, `{"a":1}`, with `{{sym}}` interpolation honored to
            // match `eval_arg`. A bare scalar **number or boolean** binds its natural JSON type
            // (`$n = 1` → the number 1, `$ok = false` → the boolean) so `match` arms and structured
            // output see the typed value, not `"1"`/`"false"` (F-008). Strings, `null`, and
            // object/array literals keep the canonical JSON-as-string `Value::String` — the shape op
            // results take, that op-arg marshaling depends on (see `lit_value`) and that the null→`""`
            // truthiness idiom relies on.
            let jv = interpolate(value, store, session_id);
            let existing_vid = match &jv {
                serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {
                    Some(store.put_value(session_id, &Value::from_json(&jv))?)
                }
                _ => None,
            };
            ("lit".to_string(), lit_text(&jv), existing_vid)
        }
        Node::Obj { .. } | Node::List { .. } => {
            // Record/list constructor — assembles a JSON value from its sub-expressions (the
            // analyzer guarantees every leaf is a pure value node). Canonical JSON-as-string.
            let jv = eval_template(node, store, session_id)?;
            ("template".to_string(), lit_text(&jv), None)
        }
        _ => return Ok(None),
    };
    let vid = match existing_vid {
        Some(v) => v,
        None => store.put_value(session_id, &Value::String(text.clone()))?,
    };
    Ok(Some(PureEval { tag, text, vid }))
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
        // A record/list constructor is a valid call argument (and `each` source): assemble it now.
        Node::Obj { .. } | Node::List { .. } => eval_template(node, store, session_id),
        other => Err(FlowError::Runtime(format!(
            "unsupported call argument `{}` — only `lit`, `var` ($symbol), and `obj`/`list` template \
             nodes are valid call args. To use a computed string (fmt/expr/jq/parse), `bind` it to a \
             symbol first, then pass that symbol as a `var` arg.",
            node_kind(other)
        ))),
    }
}

/// Evaluate a **pure value template** to a JSON value: `Obj`/`List` recurse through their
/// sub-expressions; leaves are the pure value nodes (`var`/`lit` resolved via [`eval_arg`], plus
/// `jq`/`expr`/`fmt`). No op dispatch, no IO — the analyzer rejects any impure (call/control) leaf,
/// so none reaches here. This is what `Obj`/`List` binds, `return { … }`, and template call-args
/// resolve through.
fn eval_template(
    node: &Node,
    store: &dyn ValueStore,
    session_id: &str,
) -> Result<serde_json::Value> {
    use serde_json::Value as J;
    match node {
        Node::Obj { fields } => {
            let mut map = serde_json::Map::with_capacity(fields.len());
            for (k, v) in fields {
                map.insert(k.clone(), eval_template(v, store, session_id)?);
            }
            Ok(J::Object(map))
        }
        Node::List { items } => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(eval_template(it, store, session_id)?);
            }
            Ok(J::Array(out))
        }
        Node::Jq {
            path,
            input,
            optional,
        } => {
            // Field access / sub-path: keep the raw extracted JSON value (objects/arrays nest as-is),
            // unlike the bind arm which text-ifies it.
            let jv = jq_parse_input(eval_arg(input, store, session_id)?);
            eval_jq_path(path, &jv, *optional)
        }
        Node::Expr { formula, vars } => {
            let resolved = resolve_expr_vars(vars, store, session_id)?;
            Ok(match eval_expr_value(formula, &resolved)? {
                ExprVal::Num(n) => J::from(n),
                ExprVal::Bool(b) => J::Bool(b),
                ExprVal::Str(s) => J::String(s),
                ExprVal::List(items) => J::Array(items),
                ExprVal::Obj(map) => J::Object(map),
            })
        }
        Node::Fmt { template } => Ok(J::String(interpolate_str(template, store, session_id))),
        Node::Parse {
            value: inner,
            as_type,
        } => {
            // `parse` is a pure coercion, so it composes as a template leaf too (F-012). Recover its
            // typed result so a numeric/bool/json coercion nests as its JSON scalar/shape (like the
            // `jq` arm keeps raw JSON), while a `string` coercion stays a string.
            let jv = eval_arg(inner, store, session_id)?;
            let text = coerce_parse(&jv, as_type)?;
            match as_type.as_str() {
                "f64" | "i64" | "bool" | "json" => {
                    Ok(serde_json::from_str(&text).unwrap_or(J::String(text)))
                }
                _ => Ok(J::String(text)),
            }
        }
        // `var`/`lit` resolve through eval_arg; any other (impure) node errors there too. A symbol
        // holding a JSON object/array (stored as its JSON *string*) is re-parsed so it nests as a
        // structure rather than a quoted string; scalars (incl. strings) pass through unchanged.
        _ => Ok(jq_parse_input(eval_arg(node, store, session_id)?)),
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

    // A lone object argument is the named input map — pass it straight through. This is the
    // canonical multi-param form under named-args semantics.
    if let [serde_json::Value::Object(_)] = args.as_slice() {
        return Ok(args.into_iter().next().unwrap());
    }

    let n_params = sig.required_params.len() + sig.optional_params.len();
    match args.len() {
        0 => Ok(serde_json::Value::Object(serde_json::Map::new())),
        1 => {
            // Single bare value: bind to the sole required param (the ergonomic sugar:
            // `read("x")`, `grep("TODO")`) when there's exactly one required, else the sole param.
            // For an untyped catalog (n_params == 0) the op accepts a whole-input value at runtime —
            // pass it through as the whole input so untyped single-arg calls keep working.
            if n_params == 0 {
                return Ok(args.into_iter().next().unwrap());
            }
            let pname = if sig.required_params.len() == 1 {
                sig.required_params.first().cloned()
            } else {
                sig.required_params
                    .first()
                    .or(sig.optional_params.first())
                    .cloned()
            };
            let pname = pname.ok_or_else(|| {
                FlowError::Runtime(format!("op `{op}` has no parameters to bind"))
            })?;
            let mut input = serde_json::Map::new();
            input.insert(pname, args.into_iter().next().unwrap());
            Ok(serde_json::Value::Object(input))
        }
        _ => {
            // Deprecated positional form (2+ bare args): bind required-then-optional by catalog
            // order. The analyzer rejects this for *new* plans; this fallback exists so a legacy
            // stored plan still executes rather than failing mid-flight after side effects.
            let order: Vec<String> = sig
                .required_params
                .iter()
                .chain(sig.optional_params.iter())
                .cloned()
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
    }
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
        Node::Call { op, args } => {
            let outcome = run_call(store, executor, session_id, op, args, None, sink).await?;
            *steps += 1;
            if outcome.is_error {
                return Err(call_failure(&format!("return step `{op}`"), &outcome));
            }
            Ok((outcome.content, outcome.value_id))
        }
        // Any other pure value node — `lit`, `obj`/`list` templates, and now `parse`/`jq`/`expr`/`fmt`
        // — composes as a direct return exactly as it does in bind position (F-012). Delegating to the
        // shared pure-node evaluator keeps the two positions consistent and gives `return`s the F-008
        // scalar typing for free.
        other => match eval_pure_node(other, store, session_id)? {
            Some(pe) => Ok((pe.text, Some(pe.vid))),
            None => Err(FlowError::Runtime(format!(
                "unsupported return expression `{}`",
                node_kind(other)
            ))),
        },
    }
}

// L-46: expr engine extracted to `crate::expr` — re-import the types and functions.
use crate::expr::{eval_expr_value, format_number, ExprVal};

/// Resolve an `expr` node's `vars` map (only `Lit`/`Var` nodes) to typed [`ExprVal`]s.
fn resolve_expr_vars(
    vars: &std::collections::BTreeMap<String, Box<Node>>,
    store: &dyn ValueStore,
    session_id: &str,
) -> Result<std::collections::BTreeMap<String, ExprVal>> {
    vars.iter()
        .map(|(k, v)| {
            // L-35: op results are stored as JSON *strings* (the same F13 quirk `jq_parse_input`
            // exists for) — a string-stored array/object is unwrapped to its native JSON shape
            // before typing, so `len($rs_files)` on a `glob(...)` result sees a real `ExprVal::List`
            // instead of the array's stringified text.
            let raw = jq_parse_input(eval_arg(v, store, session_id)?);
            Ok((k.clone(), ExprVal::from_json(&raw)))
        })
        .collect()
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

/// Extract `path` from `value`, real-`jq` style. The `optional` flag selects the missing-data policy
/// (L-53):
///
/// - `optional == true` (LENIENT — the default for a model-/host-emitted `jq`, and for native
///   `$x.field?` sugar): traversal through MISSING data — an absent key, an out-of-range index, a
///   field access on a non-object — yields `null` rather than erroring, and cascades (`.a.b.c` on an
///   object that only has `.a` bottoms out at `null`). This is the battle-tested leniency real agent
///   turns depend on — s_362 turn 17535 died on `jq(".transcript", $x)` fatally discarding a
///   fully-gathered turn's evidence, which this mode prevents.
/// - `optional == false` (STRICT — the default for native `$x.field` sugar): the same misses are a
///   loud error (`jq_access_error`), so a typo'd field name fails fast instead of silently reading
///   empty. A dotted numeric segment (`.0`) indexes a list.
///
/// In BOTH modes a PRESENT key whose value is `null` returns `null` (never an error — the
/// present-but-optional idiom, e.g. `run_plan`'s `failure`), and a genuinely MALFORMED path
/// (unmatched `[`, a non-numeric bracket index) is a syntax error that always errors loudly.
fn eval_jq_path(
    path: &str,
    value: &serde_json::Value,
    optional: bool,
) -> Result<serde_json::Value> {
    use serde_json::Value as J;
    let trimmed = path.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        return Ok(value.clone());
    }
    let mut cur = value.clone();
    // Split on `.` and handle `[n]` inside each segment.
    for raw_seg in trimmed.split('.') {
        let seg = raw_seg.trim();
        if seg.is_empty() {
            continue;
        }
        // Segment may be `key[0][1]` — split on `[`.
        let mut parts = seg.splitn(2, '[');
        let key = parts.next().unwrap_or("");
        if !key.is_empty() {
            cur = match &cur {
                // A PRESENT key returns its value (even `null` — that is the load-bearing
                // present-but-optional idiom, e.g. `run_plan`'s `failure`, and is never an error).
                // An ABSENT key is missing data: `null` when `optional`, else a loud error (L-53) so
                // a typo'd field name fails fast instead of silently reading empty.
                J::Object(map) => match map.get(key) {
                    Some(v) => v.clone(),
                    None if optional => J::Null,
                    None => {
                        return Err(jq_access_error(
                            path,
                            &format!("has no field `{key}`{}", present_keys_hint(map)),
                        ))
                    }
                },
                // Dotted numeric index into a list: `$nums.0` is the first element (L-53). A
                // non-numeric field name on a list, or an out-of-range index, is missing data.
                J::Array(arr) => match key.parse::<usize>() {
                    Ok(idx) => match arr.get(idx) {
                        Some(v) => v.clone(),
                        None if optional => J::Null,
                        None => {
                            return Err(jq_access_error(
                                path,
                                &format!(
                                    "index {idx} is past the end of a {}-element list",
                                    arr.len()
                                ),
                            ))
                        }
                    },
                    Err(_) if optional => J::Null,
                    Err(_) => {
                        return Err(jq_access_error(
                            path,
                            &format!(
                                "cannot read field `{key}` of a list (use an index like `.0`)"
                            ),
                        ))
                    }
                },
                _ if optional => J::Null,
                other => {
                    return Err(jq_access_error(
                        path,
                        &format!("cannot read field `{key}` of {}", json_type_name(other)),
                    ))
                }
            };
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
                cur = match &cur {
                    J::Array(arr) => match arr.get(idx) {
                        Some(v) => v.clone(),
                        None if optional => J::Null,
                        None => {
                            return Err(jq_access_error(
                                path,
                                &format!(
                                    "index [{idx}] is past the end of a {}-element list",
                                    arr.len()
                                ),
                            ))
                        }
                    },
                    _ if optional => J::Null,
                    other => {
                        return Err(jq_access_error(
                            path,
                            &format!("cannot index [{idx}] into {}", json_type_name(other)),
                        ))
                    }
                };
                bracket = bracket[end + 1..].to_string();
            }
        }
    }
    Ok(cur)
}

/// A strict field/index access hit missing data (L-53). The message names the path and points at the
/// `?` opt-out so a flow that legitimately reads an optional field has a clear fix.
fn jq_access_error(path: &str, detail: &str) -> FlowError {
    FlowError::Core(Error::Other(format!(
        "field access `{}` {detail}. If the field may be absent, mark the access optional with `?` \
         (e.g. `$x.field?`) to read `null` instead of erroring.",
        path.trim()
    )))
}

/// A human-readable JSON type name for a field-access error message.
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "an object",
    }
}

/// A short "present keys" hint for a missing-field error — the first few keys actually on the object.
fn present_keys_hint(map: &serde_json::Map<String, serde_json::Value>) -> String {
    if map.is_empty() {
        return " (the object is empty)".to_string();
    }
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let shown = keys.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
    let more = if keys.len() > 8 { ", …" } else { "" };
    format!(" (present: {shown}{more})")
}

/// Render a stored value as text (a string value is itself; anything else is its compact JSON).
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(&other.to_json()).unwrap_or_default(),
    }
}

/// Render a literal JSON value as text (a JSON string is itself; `null` is the empty string —
/// matching [`ExprVal::from_json`]'s same choice and the `""`-is-absent idiom the language already
/// uses for an optional field, e.g. `$plan.settled` — anything else is compact JSON).
///
/// A-17: this null-to-`""` mapping matters beyond cosmetics. `json_truthy`'s direct-JSON arm
/// already treats `Value::Null` as falsy, but a `jq`-extracted field is textified through here
/// before it is ever tested — without this arm, `null` round-tripped to the literal text `"null"`,
/// which `json_truthy`'s string rule reads as TRUTHY (only `""`/`"false"`/`"0"` are falsy text), so
/// `$x = $obj.field` (a present-but-null field, e.g. `run_plan`'s optional `failure`) silently
/// became an always-true condition. See `jq_of_a_present_null_field_reads_falsy`.
fn lit_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The canonical stored [`Value`] for a host-supplied JSON literal — exactly what the interpreter
/// binds for a flow-body `lit` node (`lit_text` semantics: a JSON string is itself, `null` is the
/// empty string, anything else its compact JSON text), wrapped as the JSON-as-string
/// [`Value::String`] every op result takes.
///
/// This is the one canonicalization a host must apply when injecting a value from *outside* the
/// flow (D-67: `FlowStore::seed` stores seeded inputs through here), so an injected `$name` is
/// indistinguishable from a literal-bound one everywhere downstream — same text, same arg
/// marshaling. Storing the raw structural value instead is observably different: a lone seeded
/// *object* argument would pass through as the op's whole input, where a `lit`-bound one
/// string-wraps under the op's sole required param.
pub fn lit_value(v: &serde_json::Value) -> Value {
    Value::String(lit_text(v))
}

/// Wall-clock milliseconds since the Unix epoch (0 if the clock is before it).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One **atomic** read-modify-write on a throttle bucket (F18): under a process-wide lock, load the
/// keyed timestamp list from the session store, evict entries older than `window_start`, append
/// `add` copies of `now_ms` (the statement's dispatch count; `add == 0` is a pure read), store the
/// result back, and return the number of in-window entries as of the load. Atomicity is
/// process-local — the store seam offers no compare-and-swap, and one interpreter's concurrent
/// branches are the scope the language owns.
fn throttle_bucket_rmw(
    store: &dyn ValueStore,
    session_id: &str,
    key: &SymbolName,
    window_start: u64,
    now_ms: u64,
    add: usize,
) -> Result<usize> {
    static BUCKET_LOCK: Mutex<()> = Mutex::new(());
    let _guard = BUCKET_LOCK.lock().unwrap();
    let mut times: Vec<u64> = match store.resolve(session_id, key)? {
        Some(vid) => match store.get_value(&vid)? {
            Some(Value::String(s)) => serde_json::from_str::<Vec<u64>>(&s).unwrap_or_default(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    times.retain(|&t| t >= window_start);
    let in_window = times.len();
    if add > 0 {
        for _ in 0..add {
            times.push(now_ms);
        }
        let times_json = serde_json::to_string(&times).unwrap_or_default();
        let vid = store.put_value(session_id, &Value::String(times_json))?;
        store.bind(session_id, key, &vid, None, "", Visibility::Hidden)?;
    }
    Ok(in_window)
}

/// Record a `debounce` trigger (F19): bind a process-unique token (timestamp + sequence) under the
/// per-name key in the session store and return it. A later
/// [`debounce_current_token`] comparison tells the trigger whether it is still the newest.
fn debounce_record_trigger(
    store: &dyn ValueStore,
    session_id: &str,
    key: &SymbolName,
) -> Result<String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let token = format!(
        "{}:{}",
        now_millis(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let vid = store.put_value(session_id, &Value::String(token.clone()))?;
    store.bind(session_id, key, &vid, None, "", Visibility::Hidden)?;
    Ok(token)
}

/// The most recent `debounce` trigger token for `key`, if any.
fn debounce_current_token(
    store: &dyn ValueStore,
    session_id: &str,
    key: &SymbolName,
) -> Result<Option<String>> {
    Ok(match store.resolve(session_id, key)? {
        Some(vid) => store.get_value(&vid)?.map(|v| value_text(&v)),
        None => None,
    })
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
/// kept in priority order (visibility tier, then declared order) while their cumulative rendered size
/// fits the budget; the rest are dropped and recorded as a [`RunEvent::CtxShrunk`]. Packing is
/// **drop-and-continue**: a member that doesn't fit in the remaining budget is dropped and packing
/// continues with the next, so a single oversized early member never evicts the smaller members after
/// it (the s_251 death spiral). The retained values are materialized into a labelled `content` field,
/// so a consuming op gets the bounded payload rather than only the members' symbol names.
#[allow(clippy::too_many_arguments)] // the sink (A-63) rides alongside the existing packing inputs
fn build_ctx(
    store: &dyn ValueStore,
    session_id: &str,
    name: &SymbolName,
    purpose: &Option<String>,
    members: &[SymbolName],
    budget: Option<u64>,
    transcript: &mut Vec<String>,
    sink: &mut dyn FlowSink,
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

    // KF4/L-56: gate Private/Hidden members out of the pack by default before any budgeting runs.
    // Being named in `include:` is a reference, not a permission grant (bullet 4) — a `Ctx` node has
    // no mechanism (yet) to grant one, so the boundary here is always empty and every Private/Hidden
    // member is excluded, full stop. Uses the shared [`crate::context_slice`] engine so the exclusion
    // is audited (an observation), not just silently dropped.
    let members = {
        let mut required = crate::context_slice::RequiredSymbols::default();
        let mut candidates = Vec::with_capacity(members.len());
        for sym in &members {
            required.require_whole(&sym.0);
            let visibility = store
                .binding(session_id, sym)?
                .map(|b| b.visibility)
                .unwrap_or(Visibility::Visible);
            candidates.push(crate::context_slice::Candidate {
                name: sym.0.clone(),
                flags: crate::context_slice::SymbolFlags {
                    visibility,
                    secret_derived: false,
                    policy_denied: false,
                },
                text: String::new(), // gating only here — sizing is the char-budget pass below
            });
        }
        let (kept, gate_record) = crate::context_slice::slice_context(
            &required,
            &std::collections::BTreeSet::new(),
            &candidates,
            &crate::context_slice::Boundary::none(),
            None,
            None,
        );
        if !gate_record.excluded.is_empty() {
            let excluded_names: Vec<String> = gate_record
                .excluded
                .iter()
                .map(|(n, _)| n.clone())
                .collect();
            transcript.push(format!(
                "[ctx {}] excluded gated member(s) (private/hidden): {excluded_names:?}",
                name.0
            ));
            sink.observation(&flux_evidence::Observation::new(
                "context.sliced",
                flux_evidence::Phase::Turn,
                serde_json::json!({
                    "ctx": name.0,
                    "excluded": gate_record
                        .excluded
                        .iter()
                        .map(|(n, r)| serde_json::json!({ "name": n, "reason": format!("{r:?}") }))
                        .collect::<Vec<_>>(),
                }),
            ));
        }
        let kept: std::collections::HashSet<&str> = kept.iter().map(|s| s.as_str()).collect();
        members
            .into_iter()
            .filter(|s| kept.contains(s.0.as_str()))
            .collect::<Vec<_>>()
    };

    // Materialize each bound member exactly as a consuming model sees it. Every section owns its
    // trailing separator, so the sum of `sizes` is the exact final payload length; an unbound member
    // contributes nothing (a pack tolerates a not-yet-resolved reference rather than erroring).
    let rendered: Vec<Option<String>> = members
        .iter()
        .map(|sym| -> Result<Option<String>> {
            Ok(match store.resolve(session_id, sym)? {
                Some(vid) => store
                    .get_value(&vid)?
                    .map(|v| format!("## ${}\n{}\n\n", sym.0, value_text(&v))),
                None => None,
            })
        })
        .collect::<Result<_>>()?;
    let sizes: Vec<usize> = rendered
        .iter()
        .map(|entry| entry.as_ref().map_or(0, |text| text.chars().count()))
        .collect();
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
        // Shrink by visibility tier then declared order. We pack the priority-ordered members into the
        // budget **drop-and-continue** style: a member that doesn't fit in the remaining budget is
        // skipped (dropped) and packing continues with the next — so a single oversized early member
        // never evicts the smaller members after it (the s_251 death spiral: a 493k-char evidence bind
        // declared before the code reads triggered a hard `break` and starved `ai.reason` of every
        // member that would have fit in the leftover). A pinned member is still never dropped to make
        // room for a plainer one: the stable sort preserves declared order within a tier, and we walk
        // highest tier first, so the only way a pinned member is dropped is if it doesn't fit on its
        // own — no rank inversion.
        let mut order: Vec<usize> = (0..members.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(ranks[i]));
        keep = vec![false; members.len()];
        let mut running = 0usize;
        for &i in &order {
            if running + sizes[i] <= b as usize {
                running += sizes[i];
                keep[i] = true;
            } else {
                // Drop this oversized member and keep packing the rest (do NOT `break`).
                continue;
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
            // Surface the shrinkage on the live sink too (A-63) — the transcript line above is
            // model-facing only, so a plain CLI run never saw that context was dropped. One
            // observation per shrink event; silent when nothing is dropped.
            sink.observation(&flux_evidence::Observation::new(
                "context.shrunk",
                flux_evidence::Phase::Turn,
                serde_json::json!({
                    "ctx": name.0,
                    "dropped": dropped.len(),
                    "total": kept.len() + dropped.len(),
                }),
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
    let content = rendered
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .filter_map(|(_, entry)| entry.as_deref())
        .collect::<String>();
    debug_assert!(
        budget.is_none_or(|b| content.chars().count() <= b as usize),
        "context payload must fit its declared character budget"
    );
    fields.insert("content".to_string(), Value::String(content));
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
    sink: &mut dyn FlowSink,
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
        store, session_id, ctx, &purpose, &members, budget, transcript, sink,
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
        Node::CapScope { .. } => "cap_scope",
        Node::Obj { .. } => "obj",
        Node::List { .. } => "list",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::ast::{Branch, Node, RunEvent, SymbolName, Value, Visibility};
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
            semantic_effects: Vec::new(),
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

    #[test]
    fn summarize_skips_delimiter_only_prefix_lines() {
        assert_eq!(
            summarize("}\n    fn useful_context() {}\n"),
            "fn useful_context() {}"
        );
        assert_eq!(
            summarize("{\n  \"channels\": [\n    {}\n"),
            "\"channels\": ["
        );
    }

    #[test]
    fn summarize_bound_read_result_includes_source_hint() {
        let summary = summarize_bound_result(
            "read",
            &json!({ "path": "src/lib.rs", "offset": 40, "limit": 20 }),
            "}\nfn target() {}\n",
        );
        assert_eq!(summary, "read src/lib.rs lines 40+20: fn target() {}");
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
        // Each value renders to 48 chars (label + body + separator); budget 110 fits exactly two.
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
            Some(110),
            &mut transcript,
            &mut BufferSink::default(),
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
            &mut BufferSink::default(),
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
            &mut BufferSink::default(),
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

    /// KF4/L-56 bullet 4: a `Private`/`Hidden` member is never included in a context pack, budget or
    /// no budget — being named in `include:` is a reference, not a permission grant, and `ctx` has no
    /// mechanism to grant one. The exclusion is audited via a `context.sliced` observation, and a
    /// `Visible` member alongside the gated ones is unaffected.
    #[test]
    fn ctx_excludes_private_and_hidden_members_by_default() {
        let store = MemStore::new();
        let sid = "s";
        let put = |name: &str, val: &str, vis: Visibility| {
            let vid = store.put_value(sid, &Value::String(val.into())).unwrap();
            store
                .bind(sid, &SymbolName(name.into()), &vid, None, val, vis)
                .unwrap();
        };
        put("secret", "classified", Visibility::Private);
        put("scratch", "internal", Visibility::Hidden);
        put("ok", "fine", Visibility::Visible);

        let mut transcript = Vec::new();
        let mut sink = BufferSink::default();
        let members = vec![
            SymbolName("secret".into()),
            SymbolName("scratch".into()),
            SymbolName("ok".into()),
        ];
        let vid = build_ctx(
            &store,
            sid,
            &SymbolName("pack".into()),
            &None,
            &members,
            None, // no budget: the gate applies regardless
            &mut transcript,
            &mut sink,
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
        assert_eq!(
            kept,
            vec!["ok".to_string()],
            "private/hidden members excluded, the visible one kept"
        );

        let Value::String(content) = &fields["content"] else {
            panic!("ctx materializes its retained values into a content string")
        };
        assert!(
            content.contains("fine"),
            "visible value reaches the payload: {content}"
        );
        assert!(
            !content.contains("classified"),
            "private value leaked: {content}"
        );
        assert!(
            !content.contains("internal"),
            "hidden value leaked: {content}"
        );

        let obs = observations(sink);
        assert!(
            obs.iter().any(|(k, _)| k == "context.sliced"),
            "the gating exclusion is audited even though nothing was budget-shrunk: {obs:?}"
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
        put("keep", "z".repeat(40), Visibility::Pinned); // 52 rendered chars
        put("low", "y".repeat(40), Visibility::Visible); // 51 rendered chars
        let mut t = Vec::new();
        let members = vec![SymbolName("keep".into()), SymbolName("low".into())];
        build_ctx(
            &store,
            sid,
            &SymbolName("p".into()),
            &None,
            &members,
            Some(120),
            &mut t,
            &mut BufferSink::default(),
        )
        .unwrap();

        // A higher-priority (pinned) member that overflows the budget evicts the lower-priority `low`.
        put("big", "q".repeat(50), Visibility::Pinned); // 61 rendered chars
        let v2 = append_ctx(
            &store,
            sid,
            &SymbolName("p".into()),
            &[SymbolName("big".into())],
            &mut t,
            &mut BufferSink::default(),
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

    /// A single oversized early member must not drop every smaller member after it. The s_251 death
    /// spiral: a 493k-char `session_evidence` bind declared before the code reads triggered a hard
    /// `break`, so the 11k/24k/12k code reads that would have fit in the remaining ~159k budget were
    /// all dropped — starving `ai.reason` of the very evidence the flow had just gathered.
    #[test]
    fn ctx_pack_keeps_small_members_after_an_oversized_one() {
        let store = MemStore::new();
        let sid = "s";
        let put = |name: &str, val: String, vis: Visibility| {
            let vid = store.put_value(sid, &Value::String(val.clone())).unwrap();
            store
                .bind(sid, &SymbolName(name.into()), &vid, None, &val, vis)
                .unwrap();
        };
        // Mirror the s_251 pack shape, scaled down: a tiny status bind, a tiny recent-commits bind,
        // then one oversized evidence bind, then several modest code reads that fit the leftover.
        // Each member renders with a short `## $name` label and a blank-line separator.
        put("status", "ok".into(), Visibility::Visible);
        put("recent", "commits".into(), Visibility::Visible);
        put("evidence", "e".repeat(200), Visibility::Visible); // oversized vs budget
        put("code_ops", "o".repeat(20), Visibility::Visible);
        put("code_db", "d".repeat(20), Visibility::Visible);
        put("code_sql", "q".repeat(20), Visibility::Visible);

        let mut t = Vec::new();
        // Budget 160 fits every small labelled section; evidence doesn't fit. The old hard `break`
        // at `evidence` dropped itself AND the
        // three code reads that would have fit; after the fix only `evidence` is dropped (drop-and-
        // continue) and the three code reads survive in the leftover ~87 chars of budget.
        let members = vec![
            SymbolName("status".into()),
            SymbolName("recent".into()),
            SymbolName("evidence".into()),
            SymbolName("code_ops".into()),
            SymbolName("code_db".into()),
            SymbolName("code_sql".into()),
        ];
        let vid = build_ctx(
            &store,
            sid,
            &SymbolName("analysis_pack".into()),
            &Some("diagnose".into()),
            &members,
            Some(160),
            &mut t,
            &mut BufferSink::default(),
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
        assert!(
            kept.contains(&"status".to_string()),
            "small leading members kept"
        );
        assert!(
            kept.contains(&"recent".to_string()),
            "small leading members kept"
        );
        assert!(
            !kept.contains(&"evidence".to_string()),
            "the oversized member is dropped (it doesn't fit)"
        );
        // The core assertion: the members after the oversized one survive.
        assert!(
            kept.contains(&"code_ops".to_string()),
            "small member after the oversized one is kept (drop-and-continue)"
        );
        assert!(
            kept.contains(&"code_db".to_string()),
            "small member after the oversized one is kept (drop-and-continue)"
        );
        assert!(
            kept.contains(&"code_sql".to_string()),
            "small member after the oversized one is kept (drop-and-continue)"
        );
        // The shrink event still records the oversized member as dropped.
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
            vec!["evidence".to_string()],
            "only the oversized member is dropped; the small later members survive"
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
                    semantic_effects: Vec::new(),
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

    /// L-53: the whole-flow scheduler's optimized execution is observationally equivalent to
    /// sequential execution — same bound values AND the same user-visible event ordering (the
    /// order floor keeps stage emission in program order; parallel stages replay their buffered
    /// events in that same order).
    #[tokio::test]
    async fn scheduled_plan_matches_sequential_values_and_trace_order() {
        struct RwCat;
        impl OpCatalog for RwCat {
            fn lookup(&self, name: &str) -> Option<OpSignature> {
                let (effects, idem) = match name {
                    "read" => (
                        vec![flux_spec::Effect::Read],
                        flux_spec::Idempotency::NonIdempotent,
                    ),
                    "write" => (
                        vec![flux_spec::Effect::Write],
                        flux_spec::Idempotency::NonIdempotent,
                    ),
                    _ => return None,
                };
                Some(OpSignature {
                    name: name.into(),
                    description: String::new(),
                    effects,
                    risk: flux_spec::Risk::Low,
                    idempotency: idem,
                    required_params: vec!["x".into()],
                    optional_params: Vec::new(),
                    param_types: Default::default(),
                    semantic_effects: Vec::new(),
                })
            }
        }
        struct RwHost(RwCat);
        #[async_trait::async_trait]
        impl OpHost for RwHost {
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

        /// Records the user-visible op event stream.
        #[derive(Default)]
        struct RecSink {
            calls: Vec<String>,
        }
        impl FlowSink for RecSink {
            fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
                self.calls.push(format!("{name}:{input}"));
            }
        }

        let call = |op: &str, arg: serde_json::Value| Node::Call {
            op: op.into(),
            args: vec![flow_lit(arg)],
        };
        let bind_call = |name: &str, op: &str, arg: serde_json::Value| Node::Bind {
            name: SymbolName(name.into()),
            value: Box::new(call(op, arg)),
            ty: None,
            effect: None,
        };
        // Reads feeding a template, a read-only `when` child block, a plain read, a write fence,
        // then a post-fence read; the final bind consumes everything top-level (nothing is dead).
        let body = vec![
            // two independent reads…
            bind_call("x1", "read", json!("f1")),
            bind_call("x2", "read", json!("f2")),
            // …consumed by an object template (reads $x1/$x2 → scheduled after them)
            Node::Bind {
                name: SymbolName("t".into()),
                value: Box::new(Node::Obj {
                    fields: [
                        ("x".to_string(), Box::new(flow_var("x1"))),
                        ("y".to_string(), Box::new(flow_var("x2"))),
                    ]
                    .into_iter()
                    .collect(),
                }),
                ty: None,
                effect: None,
            },
            // a read-only `when` child block (call in the condition AND in the body)
            Node::When {
                cond: Box::new(call("read", json!("flag"))),
                then: vec![bind_call("w", "read", json!("f3"))],
                otherwise: vec![],
            },
            // an independent plain read
            bind_call("c", "read", json!("f4")),
            // write (hard fence)
            call("write", json!("out")),
            // a post-fence read
            bind_call("d", "read", json!("f5")),
            // final consumer keeps every top-level symbol live
            bind_call("r", "read", json!("{{t}}{{c}}{{d}}")),
        ];

        let host = RwHost(RwCat);
        let hir = crate::ast::HirFlow {
            body: body.clone(),
            ..Default::default()
        };
        let plan = crate::optimize::optimize(&hir, &RwCat);
        // The scheduler found real parallelism: template + when + plain read share a stage.
        assert!(
            plan.stages
                .iter()
                .any(|s| matches!(s, Stage::Parallel(ids) if ids.len() == 3)),
            "expected a 3-node parallel stage, got {:?}",
            plan.stages
        );

        let store_plan = MemStore::new();
        let mut rec_plan = RecSink::default();
        execute_plan(&store_plan, &host, "s", &body, &plan, &mut rec_plan)
            .await
            .unwrap();

        let store_flow = MemStore::new();
        let ast = DraftAst {
            body: body.clone(),
            ..Default::default()
        };
        let mut rec_flow = RecSink::default();
        execute_flow(&store_flow, &host, "s", &ast, &mut rec_flow)
            .await
            .unwrap();

        // Identical user-visible op event ordering…
        assert_eq!(
            rec_plan.calls, rec_flow.calls,
            "optimized trace must equal the sequential trace"
        );
        // …and identical bound values.
        for sym in ["x1", "x2", "t", "c", "d", "r"] {
            let vp = store_plan
                .resolve("s", &SymbolName(sym.into()))
                .unwrap()
                .and_then(|id| store_plan.get_value(&id).unwrap());
            let vf = store_flow
                .resolve("s", &SymbolName(sym.into()))
                .unwrap()
                .and_then(|id| store_flow.get_value(&id).unwrap());
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
                    semantic_effects: Vec::new(),
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
                    semantic_effects: Vec::new(),
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
                    semantic_effects: Vec::new(),
                })
            };
            match name {
                "pick" => mk(&["label"]),
                "echo" => mk(&["v"]),
                "boom" | "slow" | "nap" | "flaky" => mk(&[]),
                _ => None,
            }
        }
    }

    /// A host that records the (op, first-arg) of each dispatch so a test can assert *which* branch
    /// ran. `pick` echoes its `label` (a `route` selector), `echo` echoes its `v`, `boom` errors,
    /// `slow` sleeps 150ms before succeeding (to exercise `timeout`/`race`), and `nap` sleeps 400ms
    /// (a `race` loser that outlives the winner). Approval is configurable so `confirm` fatality can
    /// be exercised (`denying()`), with the request count recorded.
    struct CfHost {
        cat: CfCat,
        log: Mutex<Vec<String>>,
        calls: AtomicUsize,
        approve: bool,
        approvals: AtomicUsize,
    }
    impl CfHost {
        fn new() -> Self {
            Self::with_approval(true)
        }
        fn denying() -> Self {
            Self::with_approval(false)
        }
        fn with_approval(approve: bool) -> Self {
            CfHost {
                cat: CfCat,
                log: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
                approve,
                approvals: AtomicUsize::new(0),
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
                "nap" => {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    ("nap".to_string(), OpOutcome::ok("nap done"))
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
            self.approvals.fetch_add(1, Ordering::SeqCst);
            if self.approve {
                ApprovalChoice::Allow
            } else {
                ApprovalChoice::Deny
            }
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

    fn flow_bind(name: &str, value: Node) -> Node {
        Node::Bind {
            name: SymbolName(name.into()),
            value: Box::new(value),
            ty: None,
            effect: None,
        }
    }

    #[tokio::test]
    async fn a_literal_value_can_be_bound_directly() {
        // `$x = 5 / "hi" / [1,2,3] / {"a":1}` — a `Lit` is a legal bind value. Scalar number/boolean
        // literals bind their natural JSON type (F-008); strings and object/array literals keep the
        // canonical JSON-as-string form.
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("n", flow_lit(json!(5))),
                flow_bind("s", flow_lit(json!("hi"))),
                flow_bind("xs", flow_lit(json!([1, 2, 3]))),
                flow_bind("obj", flow_lit(json!({"a": 1}))),
                Node::Return {
                    value: Box::new(flow_var("xs")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(out.result, "[1,2,3]");

        let text = |name: &str| {
            let vid = store
                .resolve("s", &SymbolName(name.into()))
                .unwrap()
                .unwrap();
            value_text(&store.get_value(&vid).unwrap().unwrap())
        };
        assert_eq!(text("n"), "5");
        assert_eq!(
            text("s"),
            "hi",
            "a JSON string binds to its inner text, like `fmt`"
        );
        assert_eq!(text("obj"), r#"{"a":1}"#);
    }

    #[tokio::test]
    async fn binding_a_var_aliases_the_same_value() {
        // `$b = $a` rebinds to the same immutable value — the body-level twin of CSE's `Stage::Alias`.
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("a", echo("hello")),
                flow_bind("b", flow_var("a")),
                Node::Return {
                    value: Box::new(flow_var("b")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(out.result, "hello");
        let a = store
            .resolve("s", &SymbolName("a".into()))
            .unwrap()
            .unwrap();
        let b = store
            .resolve("s", &SymbolName("b".into()))
            .unwrap()
            .unwrap();
        assert_eq!(a, b, "the alias shares the source's ValueId");
    }

    #[tokio::test]
    async fn a_lit_bind_interpolates_curly_symbols() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("name", flow_lit(json!("world"))),
                flow_bind("greeting", flow_lit(json!("hello {{name}}"))),
                Node::Return {
                    value: Box::new(flow_var("greeting")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(out.result, "hello world");
    }

    #[tokio::test]
    async fn aliasing_an_unbound_symbol_errors() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![flow_bind("b", flow_var("missing"))],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let err = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unbound symbol $missing"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn a_record_template_assembles_from_fields_jq_and_nested_lists() {
        // `return { intent: $x.intent, dest: $x.slots.dest, ok: true, items: [1, 2] }` — the headline
        // win: a record built from field-access + literals + a nested list. Wire form is exactly what
        // the model emits as JSON (`{"kind":"obj","fields":{…}}`).
        let host = CfHost::new();
        let store = MemStore::new();
        let template: Node = serde_json::from_value(json!({
            "kind": "obj",
            "fields": {
                "intent": {"kind": "jq", "path": ".intent", "input": {"kind": "var", "name": "x"}},
                "dest":   {"kind": "jq", "path": ".slots.dest", "input": {"kind": "var", "name": "x"}},
                "ok":     {"kind": "lit", "value": true},
                "items":  {"kind": "list", "items": [
                    {"kind": "lit", "value": 1},
                    {"kind": "lit", "value": 2}
                ]}
            }
        }))
        .unwrap();
        let ast = DraftAst {
            body: vec![
                flow_bind(
                    "x",
                    flow_lit(json!({"intent": "book", "slots": {"dest": "NYC"}})),
                ),
                flow_bind("r", template),
                Node::Return {
                    value: Box::new(flow_var("r")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let got: serde_json::Value = serde_json::from_str(&out.result).unwrap();
        assert_eq!(
            got,
            json!({"intent": "book", "dest": "NYC", "ok": true, "items": [1, 2]})
        );
    }

    #[tokio::test]
    async fn a_var_holding_an_object_nests_as_a_structure() {
        // `{ wrap: $x }` where `$x` holds a JSON object nests the object, not a quoted string.
        let host = CfHost::new();
        let store = MemStore::new();
        let template: Node = serde_json::from_value(json!({
            "kind": "obj",
            "fields": { "wrap": {"kind": "var", "name": "x"} }
        }))
        .unwrap();
        let ast = DraftAst {
            body: vec![
                flow_bind("x", flow_lit(json!({"a": 1}))),
                flow_bind("r", template),
                Node::Return {
                    value: Box::new(flow_var("r")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let got: serde_json::Value = serde_json::from_str(&out.result).unwrap();
        assert_eq!(got, json!({"wrap": {"a": 1}}));
    }

    #[tokio::test]
    async fn a_bare_template_is_not_an_executable_statement() {
        // A record/list is a value, not a statement — a bare one (not a bind/return/arg) is rejected.
        let host = CfHost::new();
        let store = MemStore::new();
        let bare: Node =
            serde_json::from_value(json!({"kind": "list", "items": [{"kind": "lit", "value": 1}]}))
                .unwrap();
        let ast = DraftAst {
            body: vec![bare],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let err = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("bare value is not an executable"),
            "got: {err}"
        );
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
    async fn scalar_binds_keep_their_json_type_for_match_and_output() {
        // F-008: `$n = 1` / `$ok = false` bind the typed number/boolean (not `"1"`/`"false"`), so a
        // `match` over them hits the typed arm and structured output carries the scalar type; `$s = "1"`
        // (explicit quotes) stays a string. Before the fix every scalar bind stringified and the typed
        // arms fell through to `default`.
        let match_on = |subj: &str, case: serde_json::Value, hit: &str, miss: &str| Node::Match {
            subject: Box::new(flow_var(subj)),
            cases: vec![MatchCase {
                value: flow_lit(case),
                body: vec![echo(hit)],
            }],
            default: vec![echo(miss)],
        };
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("n", flow_lit(json!(1))),
                flow_bind("ok", flow_lit(json!(false))),
                flow_bind("s", flow_lit(json!("1"))),
                match_on("n", json!(1), "num-1", "num-default"),
                match_on("ok", json!(false), "bool-false", "bool-default"),
                // `$s = "1"` is a string, so it does NOT equal the numeric case → default.
                match_on("s", json!(1), "s-is-number", "s-is-string"),
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            host.marks(),
            vec!["num-1", "bool-false", "s-is-string"],
            "typed scalar binds hit the typed match arms; an explicit-quote string stays a string"
        );

        let val = |name: &str| {
            let vid = store
                .resolve("s", &SymbolName(name.into()))
                .unwrap()
                .unwrap();
            store.get_value(&vid).unwrap().unwrap()
        };
        assert!(
            matches!(val("n"), Value::Number(_)),
            "$n = 1 binds a number"
        );
        assert!(
            matches!(val("ok"), Value::Bool(false)),
            "$ok = false binds a boolean"
        );
        assert!(
            matches!(val("s"), Value::String(_)),
            "$s = \"1\" stays a string"
        );
    }

    #[tokio::test]
    async fn object_field_extract_preserves_number_and_bool() {
        // F-008 (residual): extracting a scalar field from a bound object keeps its JSON type —
        // `$a = $obj.a` binds the boolean/number, not `"true"`/`"42"` — so re-assembling the fields
        // into an object (`return {a:$a,b:$b,n:$n}`) yields real bool/number. Before the fix the
        // field-extract bind stringified every value, so the object came back
        // `{"a":"true","b":"false","n":"42"}` (the exact retest symptom).
        let host = CfHost::new();
        let store = MemStore::new();
        let obj_field = |name: &str| Node::Jq {
            path: format!(".{name}"),
            optional: false,
            input: Box::new(flow_var("obj")),
        };
        let ast = DraftAst {
            body: vec![
                flow_bind("obj", flow_lit(json!({"a": true, "b": false, "n": 42}))),
                flow_bind("a", obj_field("a")),
                flow_bind("b", obj_field("b")),
                flow_bind("n", obj_field("n")),
                Node::Return {
                    value: Box::new(Node::Obj {
                        fields: ["a", "b", "n"]
                            .into_iter()
                            .map(|k| (k.to_string(), Box::new(flow_var(k))))
                            .collect(),
                    }),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();

        // Store-level: each extracted field kept its JSON scalar type (the load-bearing assertion).
        let val = |name: &str| {
            let vid = store
                .resolve("s", &SymbolName(name.into()))
                .unwrap()
                .unwrap();
            store.get_value(&vid).unwrap().unwrap()
        };
        assert!(
            matches!(val("a"), Value::Bool(true)),
            "$obj.a binds a boolean"
        );
        assert!(
            matches!(val("b"), Value::Bool(false)),
            "$obj.b binds a boolean"
        );
        assert!(
            matches!(val("n"), Value::Number(_)),
            "$obj.n binds a number"
        );

        // The rebuilt object is typed JSON, not stringified scalars.
        let got: serde_json::Value = serde_json::from_str(&out.result).unwrap();
        assert_eq!(
            got,
            json!({"a": true, "b": false, "n": 42}),
            "rebuilt object keeps real bool/number types: {}",
            out.result
        );
    }

    #[tokio::test]
    async fn expr_scalar_bind_preserves_number_and_bool() {
        // F-008 (siblings): `$ok = expr($a > $b)` binds the boolean and `$sum = expr($a + $b)` the
        // number — not `"true"`/`"8"` — so typed `match` arms fire and structured output stays typed,
        // matching the `Lit`/`Jq` binds. A string-producing expr stays a string.
        let host = CfHost::new();
        let store = MemStore::new();
        let expr = |formula: &str| Node::Expr {
            formula: formula.into(),
            vars: [
                ("a".to_string(), Box::new(flow_var("a"))),
                ("b".to_string(), Box::new(flow_var("b"))),
            ]
            .into_iter()
            .collect(),
        };
        let ast = DraftAst {
            body: vec![
                flow_bind("a", flow_lit(json!(5))),
                flow_bind("b", flow_lit(json!(3))),
                flow_bind("ok", expr("a > b")),
                flow_bind("sum", expr("a + b")),
                flow_bind(
                    "label",
                    Node::Expr {
                        formula: "upper('hi')".into(),
                        vars: Default::default(),
                    },
                ),
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let val = |name: &str| {
            let vid = store
                .resolve("s", &SymbolName(name.into()))
                .unwrap()
                .unwrap();
            store.get_value(&vid).unwrap().unwrap()
        };
        assert!(
            matches!(val("ok"), Value::Bool(true)),
            "$a > $b binds a boolean"
        );
        assert!(
            matches!(val("sum"), Value::Number(_)),
            "$a + $b binds a number"
        );
        assert!(
            matches!(val("label"), Value::String(_)),
            "a string-valued expr stays a string"
        );
    }

    #[tokio::test]
    async fn parse_scalar_bind_preserves_number_and_bool() {
        // F-008 (siblings): `parse($s, i64)` / `parse($s, bool)` bind the typed number/boolean, not
        // `"42"`/`"true"`, so a parsed value re-assembles typed; `parse($s, string)` stays a string.
        let host = CfHost::new();
        let store = MemStore::new();
        let parse = |sym: &str, ty: &str| Node::Parse {
            value: Box::new(flow_var(sym)),
            as_type: ty.into(),
        };
        let ast = DraftAst {
            body: vec![
                flow_bind("sn", flow_lit(json!("42"))),
                flow_bind("sb", flow_lit(json!("true"))),
                flow_bind("n", parse("sn", "i64")),
                flow_bind("ok", parse("sb", "bool")),
                flow_bind("str", parse("sn", "string")),
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let val = |name: &str| {
            let vid = store
                .resolve("s", &SymbolName(name.into()))
                .unwrap()
                .unwrap();
            store.get_value(&vid).unwrap().unwrap()
        };
        assert!(
            matches!(val("n"), Value::Number(_)),
            "parse(_, i64) binds a number"
        );
        assert!(
            matches!(val("ok"), Value::Bool(true)),
            "parse(_, bool) binds a boolean"
        );
        assert!(
            matches!(val("str"), Value::String(_)),
            "parse(_, string) stays a string"
        );
    }

    #[tokio::test]
    async fn parse_composes_as_a_direct_return() {
        // F-012: `parse` works in bind position; it now also composes as a direct return, like every
        // other pure node — previously this errored with "unsupported return expression `parse`".
        let host = CfHost::new();
        let body = vec![Node::Return {
            value: Box::new(Node::Parse {
                value: Box::new(flow_lit(json!("42"))),
                as_type: "i64".into(),
            }),
        }];
        let out = run(&host, body).await.unwrap();
        assert_eq!(out.result, "42");
    }

    #[tokio::test]
    async fn peek_is_bindable_as_a_soft_read() {
        // F-009: `$prev = peek(other)` reads the symbol softly — its value when bound, empty when not
        // — and binds it. Previously peek was statement-only and a peek-valued bind errored with
        // "execution can only bind the result of a `call`…".
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("x", flow_lit(json!("hello"))),
                Node::Bind {
                    name: SymbolName("seen".into()),
                    value: Box::new(Node::Peek {
                        name: SymbolName("x".into()),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: SymbolName("missing".into()),
                    value: Box::new(Node::Peek {
                        name: SymbolName("nope".into()),
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Return {
                    value: Box::new(flow_var("seen")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            out.result, "hello",
            "peek binds the current value of the symbol"
        );
        let missing = store
            .resolve("s", &SymbolName("missing".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            value_text(&store.get_value(&missing).unwrap().unwrap()),
            "",
            "an unbound peek binds an empty string, not an error"
        );
    }

    #[tokio::test]
    async fn ctx_shrink_emits_an_observation_for_the_cli() {
        // A-63: when a budgeted context pack drops members, a `context.shrunk` observation is emitted
        // on the sink so a plain CLI run can surface it — the transcript line alone was model-facing.
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("a", flow_lit(json!("x".repeat(40)))),
                flow_bind("b", flow_lit(json!("y".repeat(40)))),
                Node::Ctx {
                    name: SymbolName("pack".into()),
                    purpose: Some("demo".into()),
                    include: vec![SymbolName("a".into()), SymbolName("b".into())],
                    exclude: vec![],
                    budget: Some(60), // one ~42-char member fits; the second is dropped
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let obs = observations(sink);
        assert!(
            obs.iter().any(|(k, v)| k == "context.shrunk"
                && v["dropped"] == serde_json::json!(1)
                && v["total"] == serde_json::json!(2)),
            "a budgeted shrink emits a context.shrunk observation: {obs:?}"
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

    // ---- L-22: resumable mode — reified halts + statement ledger + prefix fast-forward ----

    #[tokio::test]
    async fn halted_plan_reifies_failure_with_prefix_transcript() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![echo("a"), echo("b"), call("boom", vec![])],
            ..Default::default()
        };
        let mut sink = BufferSink::default();

        let out = execute_flow_resumable(&store, &host, "s", &ast, &mut sink, None)
            .await
            .unwrap();

        assert_eq!(
            host.marks(),
            vec!["a", "b", "boom"],
            "the prefix dispatched, then the failing statement dispatched too (it failed, not skipped)"
        );
        let halt = out
            .failure
            .expect("a top-level failure is reified, not returned as Err");
        assert_eq!(halt.node, NodeId(2));
        assert_eq!(halt.kind, FailureKind::Runtime);
        assert!(!halt.kind.is_fatal());
        assert!(halt.message.contains("boom"), "{}", halt.message);
        assert!(
            out.transcript.contains("[echo]"),
            "the prefix's transcript survives the halt: {}",
            out.transcript
        );

        // The ledger recorded the completed prefix; the failed statement is NOT ledgered as
        // completed — only as the halt.
        let events = store.events("s");
        let completed: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::StatementCompleted { node, skipped, .. } => Some((*node, *skipped)),
                _ => None,
            })
            .collect();
        assert_eq!(completed, vec![(NodeId(0), false), (NodeId(1), false)]);
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::PlanHalted { node, .. } if *node == NodeId(2))));
    }

    #[test]
    fn stmt_hash_is_canonical_across_reemission() {
        let a: Node = serde_json::from_str(
            r#"{"kind":"call","op":"echo","args":[{"kind":"lit","value":"hi"}]}"#,
        )
        .unwrap();
        // Same statement, reordered keys + extra whitespace — must hash identically: the node is
        // hashed through its typed `Node` representation, which serializes fields in one fixed
        // declared order regardless of the source text's order.
        let b: Node = serde_json::from_str(
            r#"{
                "args": [ { "value": "hi", "kind": "lit" } ],
                "op": "echo",
                "kind": "call"
            }"#,
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(stmt_hash16(&a), stmt_hash16(&b));

        // A genuinely different statement hashes differently.
        let c: Node = serde_json::from_str(
            r#"{"kind":"call","op":"echo","args":[{"kind":"lit","value":"bye"}]}"#,
        )
        .unwrap();
        assert_ne!(stmt_hash16(&a), stmt_hash16(&c));
    }

    #[tokio::test]
    async fn resumed_plan_skips_hash_matched_prefix_and_rehydrates_values() {
        let host = CfHost::new();
        let store = MemStore::new();

        let bind_a = flow_bind("a", echo("hello"));
        let bind_b = flow_bind("b", echo("world"));

        let halted_ast = DraftAst {
            body: vec![bind_a.clone(), bind_b.clone(), call("boom", vec![])],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let halted = execute_flow_resumable(&store, &host, "s1", &halted_ast, &mut sink, None)
            .await
            .unwrap();
        assert!(halted.failure.is_some());
        assert_eq!(host.marks(), vec!["hello", "world", "boom"]);

        // Build the resume ledger from the session's own event log — the real fold, not hand-rolled.
        let ledger = ResumeLedger::fold(&store.events("s1")).expect("an open halt latch exists");
        assert_eq!(ledger.completed.len(), 2);

        // Resume under a DIFFERENT session — its symbol table has never bound $a/$b, so if the
        // fast-forward skips statements 0/1 without rehydration, `$a`/`$b` would be unresolved and
        // the new statement below would error. `MemStore` values are content-addressed independent
        // of session (exactly like the production store), so the ledger's recorded `ValueId`s are
        // still reachable — proving REHYDRATION, not incidental same-session persistence, is what
        // makes them resolvable again.
        assert!(
            store
                .resolve("s2", &SymbolName("a".into()))
                .unwrap()
                .is_none(),
            "the resumed session has not bound $a yet"
        );

        // Re-emit: keep the first two statements byte-identical; replace the failing one with a
        // statement that references BOTH rehydrated symbols — proving they were actually rebound,
        // not just skipped over.
        let resumed_ast = DraftAst {
            body: vec![
                bind_a,
                bind_b,
                Node::Bind {
                    name: SymbolName("c".into()),
                    value: Box::new(Node::Fmt {
                        template: "{a}-{b}".into(),
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let resumed =
            execute_flow_resumable(&store, &host, "s2", &resumed_ast, &mut sink2, Some(&ledger))
                .await
                .unwrap();

        assert!(
            resumed.failure.is_none(),
            "the divergent statement succeeds: {:?}",
            resumed.failure
        );
        assert_eq!(
            host.marks(),
            vec!["hello", "world", "boom"],
            "the matched prefix did not re-dispatch on resume"
        );
        let c = store
            .resolve("s2", &SymbolName("c".into()))
            .unwrap()
            .expect("$c is bound");
        let v = store.get_value(&c).unwrap().unwrap();
        assert_eq!(
            value_text(&v),
            "hello-world",
            "rehydrated $a/$b fed the new statement"
        );

        // Skipped statements re-ledger with skipped:true, so a second halt's fast-forward would
        // have a self-contained ledger without walking back further than this run.
        let events = store.events("s2");
        let skips: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::StatementCompleted {
                    node,
                    skipped: true,
                    ..
                } => Some(*node),
                _ => None,
            })
            .collect();
        assert_eq!(skips, vec![NodeId(0), NodeId(1)]);
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::PlanResumed { skipped, .. } if *skipped == 2)));
    }

    /// L-28: a resumed run whose ledgered value can't be looked up in the (fresh / cross-store) value
    /// store must fail LOUDLY naming the lost statement, not silently skip the rebind and later die on
    /// an "unbound symbol". Simulates L-25's cross-store resume by resuming against a *fresh* store
    /// that never held the recorded values.
    #[tokio::test]
    async fn resume_with_missing_ledger_value_is_a_hard_error() {
        let host = CfHost::new();
        let store1 = MemStore::new();

        let bind_a = flow_bind("a", echo("hello"));
        let bind_b = flow_bind("b", echo("world"));
        let halted_ast = DraftAst {
            body: vec![bind_a.clone(), bind_b.clone(), call("boom", vec![])],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let halted = execute_flow_resumable(&store1, &host, "s1", &halted_ast, &mut sink, None)
            .await
            .unwrap();
        assert!(halted.failure.is_some());

        let ledger = ResumeLedger::fold(&store1.events("s1")).expect("an open halt latch exists");
        assert_eq!(ledger.completed.len(), 2);

        // Resume against a FRESH store that never stored the ledgered values — `get_value` returns
        // `None` for $a/$b, so the fast-forward cannot rehydrate them.
        let fresh = MemStore::new();
        let resumed_ast = DraftAst {
            body: vec![
                bind_a,
                bind_b,
                Node::Bind {
                    name: SymbolName("c".into()),
                    value: Box::new(Node::Fmt {
                        template: "{a}-{b}".into(),
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let err =
            execute_flow_resumable(&fresh, &host, "s2", &resumed_ast, &mut sink2, Some(&ledger))
                .await
                .expect_err("a missing rehydration value must be a hard resume error");
        let msg = err.to_string();
        assert!(
            msg.contains("statement 0") && msg.contains("$a"),
            "the hard resume error must name the lost statement and its symbol, got: {msg}"
        );
    }

    /// L-28: `top_level_bind` gains a `parallel` arm — a `parallel` statement directly binds ALL its
    /// branch names, so the fast-forward guard sees it as a binder (and a future cross-store resume
    /// can rehydrate its branches) instead of treating it as a no-op.
    #[test]
    fn top_level_bind_covers_parallel_branches() {
        let node = Node::Parallel {
            branches: vec![
                crate::ast::Branch {
                    name: "left".into(),
                    body: vec![echo("l")],
                },
                crate::ast::Branch {
                    name: "right".into(),
                    body: vec![echo("r")],
                },
            ],
        };
        let binds: Vec<String> = top_level_bind(&node)
            .into_iter()
            .map(|(n, _)| n.0.clone())
            .collect();
        assert_eq!(binds, vec!["left".to_string(), "right".to_string()]);
    }

    /// L-28: the parallel arm feeds the missing-value guard — a fast-forwarded `parallel` whose
    /// recorded value is absent from the resumed store is a hard error (not a silent skip that drops
    /// every branch binding).
    #[tokio::test]
    async fn resume_with_missing_parallel_value_is_a_hard_error() {
        let host = CfHost::new();
        let store1 = MemStore::new();

        let par = Node::Parallel {
            branches: vec![
                crate::ast::Branch {
                    name: "left".into(),
                    body: vec![echo("l")],
                },
                crate::ast::Branch {
                    name: "right".into(),
                    body: vec![echo("r")],
                },
            ],
        };
        let halted_ast = DraftAst {
            body: vec![par.clone(), call("boom", vec![])],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let halted = execute_flow_resumable(&store1, &host, "s1", &halted_ast, &mut sink, None)
            .await
            .unwrap();
        assert!(halted.failure.is_some());

        let ledger = ResumeLedger::fold(&store1.events("s1")).expect("an open halt latch exists");
        assert_eq!(
            ledger.completed.len(),
            1,
            "the parallel completed before boom"
        );

        let fresh = MemStore::new();
        let resumed_ast = DraftAst {
            body: vec![par, call("boom", vec![])],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let err =
            execute_flow_resumable(&fresh, &host, "s2", &resumed_ast, &mut sink2, Some(&ledger))
                .await
                .expect_err("a missing parallel rehydration value must be a hard resume error");
        let msg = err.to_string();
        assert!(
            msg.contains("statement 0") && (msg.contains("$left") || msg.contains("$right")),
            "the hard resume error must name the lost parallel statement and a branch symbol, got: {msg}"
        );
    }

    /// Regression (found by A-16's loop-host tests): `ResumeLedger::fold`'s latch-closing check must
    /// compare the open halt's key against `PlanResumed::prior` (the OLD plan it resumes), not
    /// `PlanResumed::plan` (the NEW plan produced by resuming) — those two keys are almost always
    /// different (an edited or unrelated re-emission), so comparing against `plan` would leave the
    /// latch open FOREVER after any resume except a byte-identical one. A single-session, no-store
    /// fold check (independent of the loop-host's own `open_halted_plan`, which merely delegates to
    /// this fold) proves the fix at its source.
    #[tokio::test]
    async fn fold_clears_the_latch_after_a_non_identical_resume() {
        let host = CfHost::new();
        let store = MemStore::new();

        let halted_ast = DraftAst {
            body: vec![call("boom", vec![])],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let halted = execute_flow_resumable(&store, &host, "s", &halted_ast, &mut sink, None)
            .await
            .unwrap();
        assert!(halted.failure.is_some());
        let ledger = ResumeLedger::fold(&store.events("s")).expect("the latch is open");

        // An entirely UNRELATED plan (no statement in common) — the common "corrected re-emission"
        // shape, since a fix almost always changes the plan's content hash.
        let unrelated_ast = DraftAst {
            body: vec![flow_bind("c", echo("hi"))],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let resumed = execute_flow_resumable(
            &store,
            &host,
            "s",
            &unrelated_ast,
            &mut sink2,
            Some(&ledger),
        )
        .await
        .unwrap();
        assert!(resumed.failure.is_none());

        assert!(
            ResumeLedger::fold(&store.events("s")).is_none(),
            "the latch must be consumed by the resume even though the new plan shares nothing with \
             the one that halted"
        );
    }

    #[tokio::test]
    async fn divergent_prefix_statement_reruns() {
        let host = CfHost::new();
        let store = MemStore::new();

        let halted_ast = DraftAst {
            body: vec![
                flow_bind("a", echo("hello")),
                flow_bind("b", echo("world")),
                call("boom", vec![]),
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow_resumable(&store, &host, "s", &halted_ast, &mut sink, None)
            .await
            .unwrap();
        assert_eq!(host.marks(), vec!["hello", "world", "boom"]);

        let ledger = ResumeLedger::fold(&store.events("s")).unwrap();

        // Re-emit with the SECOND statement edited: only the first statement's hash still matches,
        // so the fast-forward stops there — the edited statement and everything after it re-run,
        // but the FIRST statement's side effect is not repeated.
        let resumed_ast = DraftAst {
            body: vec![
                flow_bind("a", echo("hello")),
                flow_bind("b", echo("WORLD")),
                flow_bind("c", echo("done")),
            ],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let out =
            execute_flow_resumable(&store, &host, "s", &resumed_ast, &mut sink2, Some(&ledger))
                .await
                .unwrap();

        assert!(out.failure.is_none());
        assert_eq!(
            host.marks(),
            vec!["hello", "world", "boom", "WORLD", "done"],
            "statement 0 (unchanged) did not re-dispatch; statement 1 (edited) and everything \
             after it re-ran"
        );
    }

    #[tokio::test]
    async fn ledger_fast_forward_composes_with_checkpoint_cursor_as_max_of_both() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            name: Some("phased".into()),
            body: vec![
                flow_bind("a", echo("alpha")),           // 0
                flow_bind("extra", echo("beta")),        // 1
                Node::Checkpoint { label: "p1".into() }, // 2
                flow_bind("c", echo("gamma")),           // 3
                call("boom", vec![]),                    // 4
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow_resumable(&store, &host, "s", &ast, &mut sink, None)
            .await
            .unwrap();
        assert_eq!(host.marks(), vec!["alpha", "beta", "gamma", "boom"]);

        // A ledger that only "knows about" statement 0 (as if folded from an OLDER halt, before the
        // checkpoint was ever reached) — but the checkpoint cursor recorded during the run above is
        // FURTHER along, so it must win (max of both): statement 1 must NOT re-dispatch even though
        // the ledger alone would have stopped fast-forwarding right after statement 0.
        let ledger = ResumeLedger {
            completed: vec![LedgerEntry {
                node: NodeId(0),
                stmt: stmt_hash16(&ast.body[0]),
                value: store.resolve("s", &SymbolName("a".into())).unwrap(),
            }],
            prior_plan: flow_key(ast.name.as_deref(), &ast.body),
        };

        let mut sink2 = BufferSink::default();
        let out = execute_flow_resumable(&store, &host, "s", &ast, &mut sink2, Some(&ledger))
            .await
            .unwrap();

        assert!(out.failure.is_some());
        assert_eq!(
            host.marks(),
            vec!["alpha", "beta", "gamma", "boom", "gamma", "boom"],
            "the checkpoint's cursor (past statement 2) wins over the ledger's shorter match: \
             statement 1 does not re-dispatch"
        );
    }

    #[tokio::test]
    async fn resumable_saga_failure_compensates_then_halts_without_ledgering_as_completed() {
        use crate::ast::SagaStep;
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Saga {
                steps: vec![
                    SagaStep {
                        body: vec![echo("s1")],
                        undo: vec![echo("r1")],
                    },
                    SagaStep {
                        body: vec![call("boom", vec![])],
                        undo: vec![echo("r2")],
                    },
                ],
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow_resumable(&store, &host, "s", &ast, &mut sink, None)
            .await
            .unwrap();

        assert_eq!(
            host.marks(),
            vec!["s1", "boom", "r1"],
            "the failed step's undo does not run; the completed step's undo compensates"
        );
        let halt = out
            .failure
            .expect("the saga statement's failure is reified at the top level");
        assert_eq!(halt.node, NodeId(0));

        // The saga statement never ledgers as completed — it failed, even though it compensated.
        let events = store.events("s");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunEvent::StatementCompleted { .. })),
            "a failing saga statement is never ledgered as completed"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, RunEvent::PlanHalted { node, .. } if *node == NodeId(0))));
    }

    #[tokio::test]
    async fn resumable_once_behavior_is_unaffected() {
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
        execute_flow_resumable(&store, &host, "s", &ast, &mut sink, None)
            .await
            .unwrap();
        assert_eq!(host.marks(), vec!["sent"]);

        // A second resumable run in the SAME session skips the side effect exactly like strict mode.
        let mut sink2 = BufferSink::default();
        execute_flow_resumable(&store, &host, "s", &ast, &mut sink2, None)
            .await
            .unwrap();
        assert_eq!(
            host.marks(),
            vec!["sent"],
            "once's at-most-once guarantee holds under resumable mode too"
        );
    }

    #[tokio::test]
    async fn resumable_success_matches_strict_mode_result_and_transcript() {
        let body = vec![echo("a"), echo("b")];

        let host1 = CfHost::new();
        let store1 = MemStore::new();
        let ast1 = DraftAst {
            body: body.clone(),
            ..Default::default()
        };
        let mut sink1 = BufferSink::default();
        let strict = execute_flow(&store1, &host1, "s", &ast1, &mut sink1)
            .await
            .unwrap();

        let host2 = CfHost::new();
        let store2 = MemStore::new();
        let ast2 = DraftAst {
            body,
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let resumable = execute_flow_resumable(&store2, &host2, "s", &ast2, &mut sink2, None)
            .await
            .unwrap();

        assert_eq!(strict.result, resumable.result);
        assert_eq!(strict.transcript, resumable.transcript);
        assert_eq!(strict.steps, resumable.steps);
        assert!(resumable.failure.is_none());
        assert_eq!(host1.marks(), host2.marks());
    }

    #[test]
    fn failure_kind_classification_matches_flow_error_is_fatal() {
        let denied = FlowError::Denied("nope".into());
        let confirm_denied = FlowError::Core(Error::ConfirmDenied("no".into()));
        let assert_failed = FlowError::Core(Error::AssertFailed("bad".into()));
        let runtime = FlowError::Runtime("boom".into());

        for err in [&denied, &confirm_denied, &assert_failed, &runtime] {
            assert_eq!(
                FailureKind::classify(err).is_fatal(),
                err.is_fatal(),
                "{err}"
            );
        }
        assert_eq!(FailureKind::classify(&denied), FailureKind::Denied);
        assert_eq!(
            FailureKind::classify(&confirm_denied),
            FailureKind::ConfirmDenied
        );
        assert_eq!(
            FailureKind::classify(&assert_failed),
            FailureKind::AssertFailed
        );
        assert_eq!(FailureKind::classify(&runtime), FailureKind::Runtime);
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
    async fn resumable_binds_a_supplied_await_value_and_completes() {
        // F-015 / A-58: the resumable driver (behind `flow run --resume`) fast-forwards *past* the
        // halted await. Without a bound value the post-await statement dies with `unbound symbol
        // $reply`; `bind_resume_value` supplying the payload lets it complete.
        let host = CfHost::new();
        let store = MemStore::new();
        let body = vec![
            echo("a"),
            await_node(Some("reply"), "user_input", None),
            Node::Bind {
                name: SymbolName("greeting".into()),
                value: Box::new(Node::Fmt {
                    template: "hi {reply}".into(),
                }),
                ty: None,
                effect: None,
            },
            Node::Return {
                value: Box::new(flow_var("greeting")),
            },
        ];
        let ast = DraftAst {
            body: body.clone(),
            ..Default::default()
        };

        // First run halts on the await (resumable reifies it onto `failure`, kind Awaiting).
        let mut sink = BufferSink::default();
        let halted = execute_flow_resumable(&store, &host, "s", &ast, &mut sink, None)
            .await
            .unwrap();
        let susp = halted.suspension.expect("suspended on the await");
        assert_eq!(susp.node, crate::ast::NodeId(1));

        // Supply the awaited payload, then resume through the SAME resumable driver with the ledger.
        let bound =
            bind_resume_value(&store, "s", &body, susp.node, serde_json::json!("Ada")).unwrap();
        assert_eq!(bound.as_deref(), Some("reply"));
        let ledger = ResumeLedger::fold(&store.events("s")).expect("an open halt latch exists");
        let mut sink2 = BufferSink::default();
        let out = execute_flow_resumable(&store, &host, "s", &ast, &mut sink2, Some(&ledger))
            .await
            .unwrap();
        assert!(
            out.suspension.is_none() && out.failure.is_none(),
            "the resumed run completed"
        );
        assert_eq!(
            out.result, "hi Ada",
            "the post-await statement saw the bound reply"
        );
    }

    #[tokio::test]
    async fn awaited_binding_reports_the_symbol_a_resume_must_supply() {
        // A-58: `awaited_binding` lets the CLI refuse a value-less resume, naming the symbol.
        let value_await = vec![await_node(Some("reply"), "user_input", None)];
        assert_eq!(
            awaited_binding(&value_await, crate::ast::NodeId(0)).as_deref(),
            Some("reply")
        );
        let bare_await = vec![await_node(None, "signal", None)];
        assert_eq!(awaited_binding(&bare_await, crate::ast::NodeId(0)), None);
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

    /// L-35: `len` over an `ExprVal::List` counts ELEMENTS, mirroring flux-tools'
    /// `len_counts_arrays_and_strings` (cognition.rs) at the `expr` evaluator layer. `len` on a
    /// string is unchanged (char count).
    #[test]
    fn expr_len_counts_list_elements_not_stringified_chars() {
        assert_eq!(
            ev(
                "len(xs)",
                &[(
                    "xs",
                    ExprVal::List(vec![json!(1), json!(2), json!(3), json!(4)])
                )]
            ),
            ExprVal::Num(4.0)
        );
        assert_eq!(ev("len('hello')", &[]), ExprVal::Num(5.0));
    }

    /// L-35: `ExprVal::List` truthiness (non-empty) and `as_text` (compact JSON) are pinned so a
    /// bound list participates in `when`/`fmt`/string concatenation predictably.
    #[test]
    fn expr_list_truthy_and_as_text() {
        assert!(ExprVal::List(vec![json!(1)]).truthy());
        assert!(!ExprVal::List(vec![]).truthy());
        assert_eq!(
            ExprVal::List(vec![json!(1), json!("a")]).as_text(),
            "[1,\"a\"]"
        );
        assert_eq!(ExprVal::List(vec![]).as_text(), "[]");
    }

    // ---- L-17: runtime semantics hardening (F13–F20) ----

    /// F13: op results/literals are stored as JSON *strings*; the bind arm re-parsed them before
    /// `jq` but the statement arm did not, so `$y = jq(".a", $x)` worked while the same `jq` as a
    /// bare statement errored. One shared pure-node eval path now serves both positions.
    #[tokio::test]
    async fn statement_position_jq_parses_string_stored_json_like_bind_position() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("x", flow_lit(json!({"a": 1}))),
                Node::Jq {
                    path: ".a".into(),
                    optional: false,
                    input: Box::new(flow_var("x")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            out.result, "1",
            "statement-position jq parses the string-stored JSON"
        );
        assert!(
            out.transcript.contains("[jq .a]"),
            "statement transcript tag unchanged: {}",
            out.transcript
        );
    }

    /// A-17: a field that is PRESENT but JSON `null` (e.g. `run_plan`'s optional `failure`, host-
    /// normalized to `null` on a round that didn't halt) must read FALSY once bound through `jq` and
    /// tested by `when`/`unless` — before `lit_text`'s null-to-`""` fix, `null` round-tripped to the
    /// literal text `"null"`, which `json_truthy`'s string rule reads as truthy (only `""`/`"false"`/
    /// `"0"` are falsy text), so a dotted `$obj.field` access on an absent-but-present field silently
    /// became an always-true condition — exactly what would have broken the agent loop's `when
    /// $failure` revise-routing (`docs/designs/multipass-agent-loop.md` Part 2).
    #[tokio::test]
    async fn jq_of_a_present_null_field_reads_falsy() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("obj", flow_lit(json!({"failure": null}))),
                flow_bind(
                    "failure",
                    Node::Jq {
                        path: ".failure".into(),
                        optional: false,
                        input: Box::new(flow_var("obj")),
                    },
                ),
                Node::When {
                    cond: Box::new(flow_var("failure")),
                    then: vec![flow_bind("answer", flow_lit(json!("truthy")))],
                    otherwise: vec![flow_bind("answer", flow_lit(json!("falsy")))],
                },
                Node::Return {
                    value: Box::new(flow_var("answer")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            out.result, "falsy",
            "a present-but-null field must read falsy, not truthy: {}",
            out.transcript
        );
    }

    /// L-36: real `jq` yields `null` for a path that traverses MISSING data — s_362 turn 17535's
    /// finalization plan did `jq(".transcript", $x)` on an object lacking the key and the whole
    /// (fully-gathered) turn died on `Error::Other`. A missing key must propagate `null`, not abort
    /// the flow — `lit_text` already maps `Value::Null` to `""`, so the bind renders empty.
    #[tokio::test]
    async fn jq_on_a_missing_key_yields_null_not_a_fatal_error() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("obj", flow_lit(json!({"a": 1}))),
                flow_bind(
                    "missing",
                    Node::Jq {
                        path: ".b".into(),
                        // Directly-constructed (host/model) `jq` stays lenient — this is the L-36
                        // missing-key-yields-null behavior real agent turns depend on.
                        optional: true,
                        input: Box::new(flow_var("obj")),
                    },
                ),
                Node::Return {
                    value: Box::new(flow_var("missing")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap_or_else(|e| panic!("a missing jq key must not be fatal: {e}"));
        assert_eq!(out.result, "", "missing key renders as null (empty text)");

        // Chained traversal through an absent branch cascades to null at every hop, not just the
        // first miss (`.a.b.c` on an object that only has `.a` and nothing under it).
        let ast2 = DraftAst {
            body: vec![
                flow_bind("obj", flow_lit(json!({"a": 1}))),
                flow_bind(
                    "deep",
                    Node::Jq {
                        path: ".a.b.c".into(),
                        optional: true,
                        input: Box::new(flow_var("obj")),
                    },
                ),
                Node::Return {
                    value: Box::new(flow_var("deep")),
                },
            ],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let out2 = execute_flow(&store, &host, "s2", &ast2, &mut sink2)
            .await
            .unwrap_or_else(|e| panic!("chained missing traversal must not be fatal: {e}"));
        assert_eq!(
            out2.result, "",
            "chained missing traversal cascades to null"
        );
    }

    /// L-53: at the native `$a.b` sugar surface a MISSING field is now a loud error (strict), so a
    /// typo'd field name fails fast instead of silently reading empty. The `$a.b?` opt-out restores
    /// the lenient missing-key-yields-null behavior.
    #[tokio::test]
    async fn dollar_dot_sugar_strict_errors_on_missing_and_optional_yields_null() {
        let host = CfHost::new();
        let store = MemStore::new();

        // Strict `$obj.b` on a missing field is a loud error.
        let strict = crate::parse::parse(
            "flow f\n  $obj = {\"a\":1}\n  $missing = $obj.b\n  return $missing\n",
        )
        .unwrap();
        let err = execute_flow(&store, &host, "s", &strict, &mut BufferSink::default())
            .await
            .expect_err("strict `$obj.b` on a missing field must error");
        let msg = err.to_string();
        assert!(
            msg.contains("no field `b`") && msg.contains('?'),
            "the error should name the missing field and the `?` opt-out: {msg}"
        );

        // Optional `$obj.b?` on a missing field reads null (empty text), as before.
        let opt = crate::parse::parse(
            "flow f\n  $obj = {\"a\":1}\n  $missing = $obj.b?\n  return $missing\n",
        )
        .unwrap();
        let out = execute_flow(&store, &host, "s2", &opt, &mut BufferSink::default())
            .await
            .unwrap_or_else(|e| panic!("`$obj.b?` optional access must not be fatal: {e}"));
        assert_eq!(
            out.result, "",
            "missing field via `$a.b?` optional sugar renders as null (empty text)"
        );
    }

    /// L-36: a genuinely MALFORMED path — not missing data — must still error loudly (unmatched
    /// `[`, a non-numeric index). Only absent keys/indices become `null`.
    #[tokio::test]
    async fn jq_malformed_path_syntax_still_errors() {
        let host = CfHost::new();
        let store = MemStore::new();
        let unmatched = DraftAst {
            body: vec![
                flow_bind("obj", flow_lit(json!({"a": [1, 2, 3]}))),
                flow_bind(
                    "bad",
                    Node::Jq {
                        path: ".a[0".into(),
                        optional: true,
                        input: Box::new(flow_var("obj")),
                    },
                ),
                Node::Return {
                    value: Box::new(flow_var("bad")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let err = execute_flow(&store, &host, "s", &unmatched, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unmatched"),
            "unmatched `[` still errors: {err}"
        );

        let bad_index = DraftAst {
            body: vec![
                flow_bind("obj", flow_lit(json!({"a": [1, 2, 3]}))),
                flow_bind(
                    "bad",
                    Node::Jq {
                        path: ".a[x]".into(),
                        optional: true,
                        input: Box::new(flow_var("obj")),
                    },
                ),
                Node::Return {
                    value: Box::new(flow_var("bad")),
                },
            ],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let err2 = execute_flow(&store, &host, "s2", &bad_index, &mut sink2)
            .await
            .unwrap_err();
        assert!(
            err2.to_string().contains("invalid index"),
            "non-numeric index still errors: {err2}"
        );
    }

    /// L-53: dotted numeric access `$nums.0` indexes into a list (previously it read a non-existent
    /// object key `"0"` and silently returned empty). Out-of-range is missing data: a loud error
    /// under strict access, `null` under the `?` opt-out.
    #[tokio::test]
    async fn dotted_numeric_access_indexes_a_list() {
        let host = CfHost::new();
        let store = MemStore::new();

        // `$nums.0` → the first element.
        let first =
            crate::parse::parse("flow f\n  $nums = [10, 20, 30]\n  $x = $nums.0\n  return $x\n")
                .unwrap();
        let out = execute_flow(&store, &host, "s", &first, &mut BufferSink::default())
            .await
            .expect("`$nums.0` indexes the list");
        assert_eq!(out.result, "10", "`$nums.0` is the first element");

        // Out-of-range strict access errors loudly.
        let oob =
            crate::parse::parse("flow f\n  $nums = [10, 20, 30]\n  $x = $nums.5\n  return $x\n")
                .unwrap();
        let err = execute_flow(&store, &host, "s2", &oob, &mut BufferSink::default())
            .await
            .expect_err("out-of-range strict index must error");
        assert!(
            err.to_string().contains("past the end"),
            "out-of-range names the problem: {err}"
        );

        // Out-of-range optional access reads null (empty text).
        let oob_opt =
            crate::parse::parse("flow f\n  $nums = [10, 20, 30]\n  $x = $nums.5?\n  return $x\n")
                .unwrap();
        let out2 = execute_flow(&store, &host, "s3", &oob_opt, &mut BufferSink::default())
            .await
            .expect("`$nums.5?` optional is not fatal");
        assert_eq!(out2.result, "", "out-of-range optional reads empty");
    }

    /// L-35: op results are stored as JSON *strings* (the same F13 quirk `jq` re-parses for) — a
    /// symbol bound from an op call whose canonical content is JSON array text (e.g. `glob(...)`)
    /// must count as ELEMENTS through `expr`'s `len`, not the character count of the stringified
    /// JSON. s_362: `len($rs_files)` on a 232-path glob result returned 8542 (the stringified
    /// array's char count) because `ExprVal` had no `List` variant.
    #[tokio::test]
    async fn expr_len_over_a_bound_op_result_counts_array_elements_not_chars() {
        let host = CfHost::new();
        let store = MemStore::new();
        let files = json!(["a.rs", "b.rs", "c.rs"]).to_string();
        let count_expr: Node = serde_json::from_value(json!({
            "kind": "expr",
            "formula": "len(files)",
            "vars": {"files": {"kind": "var", "name": "files"}}
        }))
        .unwrap();
        let ast = DraftAst {
            body: vec![
                flow_bind("files", echo(&files)),
                flow_bind("count", count_expr),
                Node::Return {
                    value: Box::new(flow_var("count")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            out.result, "3",
            "len() must count array elements, not the stringified JSON's characters: {}",
            out.transcript
        );
    }

    /// F14: a denied `confirm` nested in `loop` nested in `retry` is fatal — the `loop` wrap
    /// preserves its structural identity and `retry` propagates it without another attempt.
    #[tokio::test]
    async fn denied_confirm_inside_loop_inside_retry_is_not_retried() {
        let host = CfHost::denying();
        let body = vec![Node::Retry {
            max: 3,
            backoff: None,
            delay_ms: Some(0),
            body: vec![Node::Loop {
                for_ms: 10_000,
                every_ms: 0,
                until: None,
                body: vec![Node::Confirm {
                    message: "wipe it".into(),
                    risk: Some("high".into()),
                    body: vec![echo("never")],
                }],
                bind: None,
            }],
            bind: None,
        }];
        let err = run(&host, body).await.unwrap_err();
        assert!(err.to_string().contains("confirm denied"), "got: {err}");
        assert!(
            err.is_fatal(),
            "fatal identity survives loop+retry wrapping"
        );
        assert_eq!(
            host.approvals.load(Ordering::SeqCst),
            1,
            "the denied confirm was asked exactly once — not retried"
        );
        assert!(host.marks().is_empty(), "the confirmed body never ran");
    }

    /// L-21: a host whose envelope **denies** every dispatch (the policy / permission-rule /
    /// approval class, marked via [`OpOutcome::denial`]) — the op never runs. Distinct from `boom`
    /// (an op that runs and fails, which stays retryable).
    struct DenyEnvelopeHost {
        cat: CfCat,
        dispatches: AtomicUsize,
    }
    impl DenyEnvelopeHost {
        fn new() -> Self {
            Self {
                cat: CfCat,
                dispatches: AtomicUsize::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl OpHost for DenyEnvelopeHost {
        async fn dispatch(&self, op: &str, _input: serde_json::Value) -> OpOutcome {
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            OpOutcome::denial(format!("`{op}` denied by policy (fs.read on File)"))
        }
        fn catalog(&self) -> &dyn OpCatalog {
            &self.cat
        }
        async fn request_approval(&self, _l: &str, _i: &flux_spec::IntentSet) -> ApprovalChoice {
            ApprovalChoice::Allow
        }
        fn trim_output(&self, view: String, _op: &str) -> String {
            view
        }
    }

    /// L-21: a *policy-denied op* is fatal exactly like the denied `confirm` above — the denial is
    /// typed (`FlowError::Denied`, host-marked via `OpOutcome::denied`), survives the `loop` wrap,
    /// and `retry` propagates it without a second attempt. Before, the denial was an in-band error
    /// string and the whole nest re-attempted it.
    #[tokio::test]
    async fn policy_denied_op_is_not_retried_inside_loop() {
        let host = DenyEnvelopeHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: None,
                delay_ms: Some(0),
                body: vec![Node::Loop {
                    for_ms: 10_000,
                    every_ms: 0,
                    until: None,
                    body: vec![flow_bind("x", echo("never"))],
                    bind: None,
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let err = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.is_fatal(),
            "a denial is fatal through loop+retry: {err}"
        );
        assert!(
            err.to_string().contains("denied by policy"),
            "the denial message survives: {err}"
        );
        assert_eq!(
            host.dispatches.load(Ordering::SeqCst),
            1,
            "the denied op was dispatched exactly once — never re-attempted"
        );
    }

    /// L-21: a denial in **condition position** propagates as the fatal error instead of reading as
    /// `false` — otherwise a `repeat until denied()` guard re-dispatches (re-prompts) the denied op
    /// every iteration, and a `when denied()` silently absorbs the refusal.
    #[tokio::test]
    async fn denied_op_in_condition_position_propagates_not_false() {
        let host = DenyEnvelopeHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Repeat {
                max: 5,
                until: Some(Box::new(call("echo", vec![flow_lit(json!("done?"))]))),
                body: vec![Node::Bind {
                    name: SymbolName("i".into()),
                    value: Box::new(flow_lit(json!(1))),
                    ty: None,
                    effect: None,
                }],
                collect: None,
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let err = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.is_fatal(), "a denied guard is fatal: {err}");
        assert_eq!(
            host.dispatches.load(Ordering::SeqCst),
            1,
            "the denied guard was not re-evaluated across iterations"
        );
    }

    /// F14: fatality survives the composite-op boundary — a denied `confirm` inside a composite is
    /// NOT stringified into a retryable in-band error.
    #[tokio::test]
    async fn denied_confirm_inside_composite_op_is_not_retried() {
        struct CompositeCat(CompositeOpDecl);
        impl OpCatalog for CompositeCat {
            fn lookup(&self, name: &str) -> Option<OpSignature> {
                if name == self.0.name {
                    Some(sig(name, &[], &[]))
                } else {
                    CfCat.lookup(name)
                }
            }
            fn composite(&self, name: &str) -> Option<CompositeOpDecl> {
                (name == self.0.name).then(|| self.0.clone())
            }
        }
        struct CompositeDenyHost {
            cat: CompositeCat,
            approvals: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl OpHost for CompositeDenyHost {
            async fn dispatch(&self, _op: &str, _input: serde_json::Value) -> OpOutcome {
                OpOutcome::ok("ok")
            }
            fn catalog(&self) -> &dyn OpCatalog {
                &self.cat
            }
            async fn request_approval(
                &self,
                _label: &str,
                _intents: &flux_spec::IntentSet,
            ) -> ApprovalChoice {
                self.approvals.fetch_add(1, Ordering::SeqCst);
                ApprovalChoice::Deny
            }
            fn trim_output(&self, view: String, _op: &str) -> String {
                view
            }
        }

        let decl = CompositeOpDecl {
            name: "danger_op".into(),
            params: vec![],
            returns: None,
            meta: Default::default(),
            body: DraftAst {
                body: vec![Node::Confirm {
                    message: "danger".into(),
                    risk: None,
                    body: vec![echo("never")],
                }],
                ..Default::default()
            },
        };
        let host = CompositeDenyHost {
            cat: CompositeCat(decl),
            approvals: AtomicUsize::new(0),
        };
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: None,
                delay_ms: Some(0),
                body: vec![call("danger_op", vec![])],
                bind: None,
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let err = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("confirm denied"), "got: {err}");
        assert_eq!(
            host.approvals.load(Ordering::SeqCst),
            1,
            "not re-attempted through the composite boundary"
        );
        // The composite boundary still records the failure in the audit trail.
        assert!(store
            .events("s")
            .iter()
            .any(|e| matches!(e, RunEvent::StepFailed { .. })));
    }

    /// F14: a failed `assert` inside `loop` inside `retry` aborts on the first attempt.
    #[tokio::test]
    async fn failed_assert_inside_loop_is_not_retried() {
        let host = CfHost::new();
        let body = vec![Node::Retry {
            max: 3,
            backoff: None,
            delay_ms: Some(0),
            body: vec![
                echo("attempt"),
                Node::Loop {
                    for_ms: 10_000,
                    every_ms: 0,
                    until: None,
                    body: vec![Node::Assert {
                        cond: Box::new(flow_lit(json!(false))),
                        message: Some("invariant broke".into()),
                    }],
                    bind: None,
                },
            ],
            bind: None,
        }];
        let err = run(&host, body).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("assertion failed: invariant broke"),
            "got: {err}"
        );
        assert_eq!(
            host.marks(),
            vec!["attempt"],
            "exactly one attempt — a failed assert is not retried"
        );
    }

    /// `retry` re-attempts transient failures (with backoff) and binds the succeeding attempt.
    #[tokio::test]
    async fn retry_retries_transient_failures_then_succeeds() {
        struct FlakyHost {
            cat: CfCat,
            calls: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl OpHost for FlakyHost {
            async fn dispatch(&self, _op: &str, _input: serde_json::Value) -> OpOutcome {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    OpOutcome::error("transient")
                } else {
                    OpOutcome::ok("recovered")
                }
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
        let host = FlakyHost {
            cat: CfCat,
            calls: AtomicUsize::new(0),
        };
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Retry {
                max: 3,
                backoff: Some("exponential".into()),
                delay_ms: Some(0),
                body: vec![flow_bind("r", call("flaky", vec![]))],
                bind: Some(SymbolName("out".into())),
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            host.calls.load(Ordering::SeqCst),
            3,
            "two transient failures then a success"
        );
        assert_eq!(out.result, "recovered");
        assert!(
            store
                .resolve("s", &SymbolName("out".into()))
                .unwrap()
                .is_some(),
            "the retry bound the succeeding attempt's value"
        );
    }

    /// `retry` exhausts its attempts on persistent failure and reports the last error.
    #[tokio::test]
    async fn retry_exhausts_attempts_and_reports_the_last_error() {
        let host = CfHost::new();
        let body = vec![Node::Retry {
            max: 2,
            backoff: None,
            delay_ms: Some(0),
            body: vec![call("boom", vec![])],
            bind: None,
        }];
        let err = run(&host, body).await.unwrap_err();
        assert!(
            err.to_string().contains("`retry` exhausted 2 attempt(s)"),
            "got: {err}"
        );
        assert_eq!(host.marks(), vec!["boom", "boom"]);
    }

    /// F15: when a `parallel` branch fails, the completed branches' buffered sink output, dispatched
    /// steps, transcript, and bindings are merged (declaration order) before the error propagates.
    #[tokio::test]
    async fn parallel_sibling_failure_preserves_completed_branch_audit() {
        let host = CfHost::new();
        let store = MemStore::new();
        let par = Node::Parallel {
            branches: vec![
                crate::ast::Branch {
                    name: SymbolName("a".into()),
                    body: vec![echo("okA")],
                },
                crate::ast::Branch {
                    name: SymbolName("b".into()),
                    body: vec![call("boom", vec![])],
                },
            ],
        };
        // Wrapped in `try` so the flow completes and the merged audit is observable on the outcome.
        let ast = DraftAst {
            body: vec![Node::Try {
                body: vec![par],
                catch: Some(SymbolName("e".into())),
                handler: vec![],
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            out.steps, 2,
            "both branches' dispatches counted (completed + failed)"
        );
        assert!(
            out.transcript.contains("okA"),
            "completed branch's transcript survives the sibling failure: {}",
            out.transcript
        );
        assert!(
            sink.events
                .iter()
                .any(|ev| matches!(ev, SinkEvent::ToolResult(n, _) if n == "echo")),
            "completed branch's buffered sink output was replayed"
        );
        assert!(
            store
                .resolve("s", &SymbolName("a".into()))
                .unwrap()
                .is_some(),
            "completed branch's binding happened before the error propagated"
        );
        let evid = store
            .resolve("s", &SymbolName("e".into()))
            .unwrap()
            .unwrap();
        assert!(
            value_text(&store.get_value(&evid).unwrap().unwrap()).contains("boom"),
            "the branch error itself propagated"
        );
    }

    /// F16: all branches failing before the deadline is a joined branch error naming each branch's
    /// failure — distinct from the timeout message.
    #[tokio::test]
    async fn race_all_branches_failed_is_a_joined_error_distinct_from_timeout() {
        let host = CfHost::new();
        let body = vec![Node::Race {
            timeout_ms: 5_000,
            bind: None,
            branches: vec![
                crate::ast::Branch {
                    name: SymbolName("a".into()),
                    body: vec![call("boom", vec![])],
                },
                crate::ast::Branch {
                    name: SymbolName("b".into()),
                    body: vec![call("boom", vec![])],
                },
            ],
        }];
        let err = run(&host, body).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("all 2 branch(es) errored"), "got: {msg}");
        assert!(
            msg.contains("`a`") && msg.contains("`b`") && msg.contains("boom"),
            "names the branch errors: {msg}"
        );
        assert!(
            !msg.contains("timed out"),
            "an all-fail is not reported as a timeout: {msg}"
        );
    }

    /// F16: a losing branch's dispatched steps stay in the returned step count and transcript, so
    /// an enclosing `budget` counts the work that actually happened.
    #[tokio::test]
    async fn race_losing_branch_dispatches_stay_counted() {
        let host = CfHost::new();
        // Loser: dispatches `echo l1` (completes), then naps past the winner's finish (cancelled
        // mid-nap). Winner: `slow` succeeds at ~150ms.
        let body = vec![Node::Race {
            timeout_ms: 10_000,
            bind: Some(SymbolName("winner".into())),
            branches: vec![
                crate::ast::Branch {
                    name: SymbolName("loser".into()),
                    body: vec![echo("l1"), call("nap", vec![])],
                },
                crate::ast::Branch {
                    name: SymbolName("fast".into()),
                    body: vec![flow_bind("w", call("slow", vec![]))],
                },
            ],
        }];
        let out = run(&host, body).await.unwrap();
        assert_eq!(out.result, "slow done", "the successful branch won");
        assert_eq!(
            out.steps, 2,
            "the loser's completed dispatch is counted alongside the winner's"
        );
        assert!(
            out.transcript.contains("l1"),
            "the loser's dispatched step stays in the transcript: {}",
            out.transcript
        );
    }

    /// F17: the flow key includes the body hash, so an EDITED body (same declared name) does not
    /// fast-forward past a stale checkpoint.
    #[tokio::test]
    async fn edited_flow_body_does_not_fast_forward_past_stale_checkpoint() {
        let host = CfHost::new();
        let store = MemStore::new();
        let v1 = DraftAst {
            name: Some("phased".into()),
            body: vec![
                echo("step1"),
                Node::Checkpoint { label: "p1".into() },
                echo("step2"),
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &v1, &mut sink)
            .await
            .unwrap();
        assert_eq!(host.marks(), vec!["step1", "step2"]);

        // Same declared name, edited body: no fast-forward — the changed prefix re-runs.
        let v2 = DraftAst {
            name: Some("phased".into()),
            body: vec![
                echo("stepX"),
                Node::Checkpoint { label: "p1".into() },
                echo("step2"),
            ],
            ..Default::default()
        };
        execute_flow(&store, &host, "s", &v2, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            host.marks(),
            vec!["step1", "step2", "stepX", "step2"],
            "the edited body's prefix re-runs instead of fast-forwarding"
        );
    }

    /// F17: a named flow's run and resume derive the SAME flow key, so a checkpoint recorded during
    /// a resumed run fast-forwards a later fresh run.
    #[tokio::test]
    async fn named_flow_run_and_resume_agree_on_checkpoint_keys() {
        let host = CfHost::new();
        let store = MemStore::new();
        let body = vec![
            echo("a"),
            await_node(Some("reply"), "user_input", None),
            Node::Checkpoint { label: "c1".into() },
            echo("b"),
        ];
        let ast = DraftAst {
            name: Some("named".into()),
            body: body.clone(),
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let susp = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap()
            .suspension
            .unwrap();

        // Resume WITH the declared name: the post-await checkpoint records under the named key.
        let mut sink2 = BufferSink::default();
        resume_flow_named(
            &store,
            &host,
            "s",
            Some("named"),
            &body,
            susp.node,
            Value::String("hi".into()),
            &mut sink2,
        )
        .await
        .unwrap();
        assert_eq!(host.marks(), vec!["a", "b"]);

        // A fresh run of the same named flow sees the resumed run's checkpoint: it fast-forwards
        // past the whole prefix (including the await) instead of suspending again.
        let mut sink3 = BufferSink::default();
        let out3 = execute_flow(&store, &host, "s", &ast, &mut sink3)
            .await
            .unwrap();
        assert!(
            out3.suspension.is_none(),
            "run and resume agree on the flow key — no re-suspend"
        );
        assert_eq!(
            host.marks(),
            vec!["a", "b", "b"],
            "fast-forwarded past the checkpointed prefix"
        );
    }

    /// F18: `throttle` counts actual op DISPATCHES per sliding window (budget-style), not
    /// throttle-node entries, and the keyed bucket persists across runs in the same session.
    #[tokio::test]
    async fn throttle_counts_op_dispatches_within_the_window() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Throttle {
                name: "t1".into(),
                max: 2,
                window_ms: 60_000,
                body: vec![echo("one"), echo("two"), echo("three")],
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let err = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("`throttle` limit of 2"),
            "got: {err}"
        );
        assert_eq!(
            host.marks(),
            vec!["one", "two"],
            "the over-limit dispatch never ran"
        );

        // Cross-run persistence: a fresh run of a same-named throttle in the same session is
        // still saturated.
        let ast2 = DraftAst {
            body: vec![Node::Throttle {
                name: "t1".into(),
                max: 2,
                window_ms: 60_000,
                body: vec![echo("four")],
            }],
            ..Default::default()
        };
        let mut sink2 = BufferSink::default();
        let err2 = execute_flow(&store, &host, "s", &ast2, &mut sink2)
            .await
            .unwrap_err();
        assert!(err2.to_string().contains("throttle"), "got: {err2}");
        assert_eq!(
            host.marks(),
            vec!["one", "two"],
            "no further dispatch admitted in the window"
        );
    }

    /// F19: two rapid triggers of the same debounce key coalesce — only the newest runs the body,
    /// once the key has been quiet for `wait_ms`. State lives in the session store (cross-turn).
    #[tokio::test]
    async fn debounce_coalesces_a_retrigger_within_the_window() {
        let host = CfHost::new();
        let store = MemStore::new();
        let mk = || DraftAst {
            body: vec![Node::Debounce {
                name: "d1".into(),
                wait_ms: 250,
                body: vec![echo("ran")],
            }],
            ..Default::default()
        };
        let (ast1, ast2) = (mk(), mk());
        let mut sink1 = BufferSink::default();
        let mut sink2 = BufferSink::default();
        let (out1, out2) =
            tokio::join!(execute_flow(&store, &host, "s", &ast1, &mut sink1), async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                execute_flow(&store, &host, "s", &ast2, &mut sink2).await
            });
        let (out1, out2) = (out1.unwrap(), out2.unwrap());
        assert_eq!(
            host.marks(),
            vec!["ran"],
            "the body ran exactly once for two rapid triggers"
        );
        assert!(
            out1.transcript.contains("coalesced"),
            "the superseded trigger reports the coalesce: {}",
            out1.transcript
        );
        assert!(
            out2.transcript.contains("[echo]"),
            "the newest trigger ran the body: {}",
            out2.transcript
        );
    }

    /// F20a: `pipe` and `memo` transcript pushes go through `executor.trim_output` like every other
    /// transcript path; the canonical bound value stays untouched.
    #[tokio::test]
    async fn pipe_and_memo_transcript_views_are_trimmed_to_the_output_budget() {
        struct TrimHost(CfCat);
        #[async_trait::async_trait]
        impl OpHost for TrimHost {
            async fn dispatch(&self, _op: &str, input: serde_json::Value) -> OpOutcome {
                OpOutcome::ok(
                    input
                        .get("v")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
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
                if view.len() > 8 {
                    format!("{}…[trim]", &view[..8])
                } else {
                    view
                }
            }
        }
        let host = TrimHost(CfCat);
        let store = MemStore::new();
        let long = "abcdefghijklmnopqrstuvwxyz";
        let ast = DraftAst {
            body: vec![
                Node::Pipe {
                    steps: vec![echo(long)],
                    bind: None,
                },
                Node::Memo {
                    name: SymbolName("m".into()),
                    value: Box::new(echo(long)),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert!(
            out.transcript.matches("…[trim]").count() >= 2,
            "both pipe and memo transcript views are trimmed: {}",
            out.transcript
        );
        assert!(
            !out.transcript.contains(long),
            "the oversized view does not reach the transcript"
        );
        // The canonical value is untouched — the memo bound the full content.
        let m = store
            .resolve("s", &SymbolName("m".into()))
            .unwrap()
            .unwrap();
        assert_eq!(value_text(&store.get_value(&m).unwrap().unwrap()), long);
    }

    /// F20b: a `when` branch that binds an EMPTY string still surfaces its value id as the node's
    /// last value, so an enclosing `seq … bind` binds it instead of nothing.
    #[tokio::test]
    async fn branch_value_propagates_even_when_its_text_is_empty() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Seq {
                body: vec![Node::When {
                    cond: Box::new(flow_lit(json!(true))),
                    then: vec![flow_bind("e", flow_lit(json!("")))],
                    otherwise: vec![],
                }],
                bind: Some(SymbolName("out".into())),
            }],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let out_vid = store.resolve("s", &SymbolName("out".into())).unwrap();
        assert!(
            out_vid.is_some(),
            "the empty-text branch value still propagates to the seq bind"
        );
        assert_eq!(
            value_text(&store.get_value(&out_vid.unwrap()).unwrap().unwrap()),
            ""
        );
    }

    /// F20c: step ids carry the op name, not just the input hash — two different ops over identical
    /// input must not share an id in the audit trail.
    #[tokio::test]
    async fn step_ids_include_the_op_name() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![echo("x")],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let step = store
            .events("s")
            .into_iter()
            .find_map(|e| match e {
                RunEvent::StepStarted { step, .. } => Some(step),
                _ => None,
            })
            .expect("a StepStarted event");
        assert!(
            step.0.contains("echo"),
            "the step id names the op: {}",
            step.0
        );
    }

    /// F20d: the `each` item symbol is loop-scoped — an outer binding of the same name is restored
    /// after the loop instead of being clobbered by the last item.
    #[tokio::test]
    async fn each_item_binding_restores_the_outer_symbol_after_the_loop() {
        let host = CfHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![
                flow_bind("item", flow_lit(json!("outer"))),
                Node::Each {
                    source: Box::new(flow_lit(json!(["x", "y"]))),
                    item: SymbolName("item".into()),
                    body: vec![echo("{{item}}")],
                    collect: None,
                    flat: false,
                },
                Node::Return {
                    value: Box::new(flow_var("item")),
                },
            ],
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(
            host.marks(),
            vec!["x", "y"],
            "the loop variable was bound per iteration"
        );
        assert_eq!(
            out.result, "outer",
            "the outer binding is restored past the loop"
        );
    }

    // ---- A-39: flag-gated structural trace of the outer agent loop ----

    /// The op catalog for [`TraceHost`]: two zero-arg ops, `plan` and `check_done`.
    struct TraceCat;
    impl OpCatalog for TraceCat {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            matches!(name, "plan" | "check_done").then(|| OpSignature {
                name: name.into(),
                description: String::new(),
                effects: Vec::new(),
                risk: flux_spec::Risk::Low,
                idempotency: flux_spec::Idempotency::Idempotent,
                required_params: Vec::new(),
                optional_params: Vec::new(),
                param_types: Default::default(),
                semantic_effects: Vec::new(),
            })
        }
    }

    /// A host used only to drive the structural-trace tests: `plan` always succeeds (echoing a fixed
    /// string), and `check_done` reports `"false"` the first time it's dispatched and `"true"` every
    /// time after — letting a `repeat ... until $check_done()` guard trip deterministically on a known
    /// round without the test needing to see the (currently discarded) loop counter itself.
    struct TraceHost {
        cat: TraceCat,
        checks: AtomicUsize,
    }
    impl TraceHost {
        fn new() -> Self {
            TraceHost {
                cat: TraceCat,
                checks: AtomicUsize::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl OpHost for TraceHost {
        async fn dispatch(&self, op: &str, _input: serde_json::Value) -> OpOutcome {
            match op {
                "check_done" => {
                    let n = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
                    OpOutcome::ok(if n >= 2 { "true" } else { "false" })
                }
                _ => OpOutcome::ok("planned"),
            }
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

    /// An agent-loop-shaped body: a bind-of-call (`$plan = plan()`), a `match` that falls through to
    /// its `default` arm, and a `when` whose condition is truthy — wrapped in a `repeat 2 until
    /// check_done()`, followed by a top-level `return`.
    fn trace_body() -> Vec<Node> {
        vec![
            flow_bind("kind", flow_lit(json!("chat"))),
            Node::Repeat {
                max: 2,
                until: Some(Box::new(call("check_done", vec![]))),
                body: vec![
                    flow_bind("plan", call("plan", vec![])),
                    Node::Match {
                        subject: Box::new(flow_var("kind")),
                        cases: vec![crate::ast::MatchCase {
                            value: flow_lit(json!("other")),
                            body: vec![],
                        }],
                        default: vec![Node::Peek {
                            name: SymbolName("kind".into()),
                        }],
                    },
                    Node::When {
                        cond: Box::new(flow_var("kind")),
                        then: vec![],
                        otherwise: vec![],
                    },
                ],
                collect: None,
            },
            Node::Return {
                value: Box::new(flow_var("plan")),
            },
        ]
    }

    /// The observation `(kind, data)` pairs a [`BufferSink`] recorded, in order.
    fn observations(sink: BufferSink) -> Vec<(String, serde_json::Value)> {
        sink.events
            .into_iter()
            .filter_map(|e| match e {
                SinkEvent::Observation(o) => Some((o.kind, o.data)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn structural_trace_emits_rounds_branches_calls_and_return_when_sink_opts_in() {
        let host = TraceHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: trace_body(),
            ..Default::default()
        };
        let mut sink = BufferSink {
            trace: true,
            ..Default::default()
        };
        let out = execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        assert_eq!(out.result, "planned");

        let obs = observations(sink);
        let kinds: Vec<&str> = obs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "loop.round",
                "loop.node", // call: plan
                "loop.node", // match: default
                "loop.node", // when: then
                "loop.round",
                "loop.node", // call: plan
                "loop.node", // match: default
                "loop.node", // when: then
                "loop.node", // repeat: until-hit
                "loop.node", // return
            ],
            "unexpected trace shape: {obs:?}"
        );

        assert_eq!(obs[0].1, json!({"round": 1, "max": 2}));
        assert_eq!(
            obs[1].1,
            json!({"node": "call", "op": "plan", "bind": "plan"})
        );
        assert_eq!(
            obs[2].1,
            json!({"node": "match", "subject": "$kind", "value": "\"chat\"", "arm": "default"})
        );
        assert_eq!(
            obs[3].1,
            json!({"node": "when", "cond": "$kind", "branch": "then"})
        );
        assert_eq!(obs[4].1, json!({"round": 2, "max": 2}));
        assert_eq!(
            obs[5].1,
            json!({"node": "call", "op": "plan", "bind": "plan"})
        );
        assert_eq!(
            obs[6].1,
            json!({"node": "match", "subject": "$kind", "value": "\"chat\"", "arm": "default"})
        );
        assert_eq!(
            obs[7].1,
            json!({"node": "when", "cond": "$kind", "branch": "then"})
        );
        assert_eq!(
            obs[8].1,
            json!({"node": "repeat", "until_hit": true, "rounds": 2, "max": 2})
        );
        assert_eq!(obs[9].1, json!({"node": "return", "value": "$plan"}));
    }

    /// The zero-default-output invariant, pinned at the emission gate: the identical flow run with a
    /// default (opted-out) sink emits NO trace observations at all.
    #[tokio::test]
    async fn structural_trace_is_silent_by_default() {
        let host = TraceHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: trace_body(),
            ..Default::default()
        };
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let obs = observations(sink);
        assert!(
            obs.is_empty(),
            "a default sink must never see a trace observation: {obs:?}"
        );
    }

    #[tokio::test]
    async fn structural_trace_emits_one_parallel_branch_event_per_branch_in_order() {
        let host = TraceHost::new();
        let store = MemStore::new();
        let ast = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: SymbolName("left".into()),
                        body: vec![],
                    },
                    Branch {
                        name: SymbolName("right".into()),
                        body: vec![],
                    },
                ],
            }],
            ..Default::default()
        };
        let mut sink = BufferSink {
            trace: true,
            ..Default::default()
        };
        execute_flow(&store, &host, "s", &ast, &mut sink)
            .await
            .unwrap();
        let obs = observations(sink);
        let branch_names: Vec<&str> = obs
            .iter()
            .filter(|(k, _)| k == "loop.node")
            .filter_map(|(_, d)| d.get("name").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(
            branch_names,
            vec!["left", "right"],
            "each branch's parallel.branch event fires exactly once, in declaration order: {obs:?}"
        );
    }

    // -- D-60: DraftAst::scoped -----------------------------------------------------------------

    /// A minimal `OpHost` with a real cap-scope stack — mirroring `flux_runtime::Executor`'s
    /// enforcement (narrow-on-push intersection, membership check at dispatch) — just enough to
    /// prove `DraftAst::scoped` actually restricts dispatch once the flow it wraps runs, without
    /// pulling the whole runtime crate into a flux-lang-only test.
    struct CapScopeHost {
        cat: MockCatalog,
        scopes: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl OpHost for CapScopeHost {
        async fn dispatch(&self, op: &str, _input: serde_json::Value) -> OpOutcome {
            if let Some(scope) = self.scopes.lock().unwrap().last() {
                if !scope.iter().any(|t| t == op) {
                    return OpOutcome::denial(format!(
                        "`{op}` denied by capability scope (not in the active with_tools allowlist)"
                    ));
                }
            }
            OpOutcome::ok(format!("ran {op}"))
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
        async fn push_cap_scope(&self, tools: &[String]) {
            let mut stack = self.scopes.lock().unwrap();
            let effective: Vec<String> = match stack.last() {
                Some(outer) => tools
                    .iter()
                    .filter(|t| outer.contains(t))
                    .cloned()
                    .collect(),
                None => tools.to_vec(),
            };
            stack.push(effective);
        }
        async fn pop_cap_scope(&self) {
            self.scopes.lock().unwrap().pop();
        }
    }

    fn cap_scope_host() -> CapScopeHost {
        CapScopeHost {
            cat: MockCatalog(vec![sig("read", &[], &[]), sig("write", &[], &[])]),
            scopes: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn draft_ast_scoped_allows_an_op_inside_and_denies_one_outside_the_allowlist() {
        let host = cap_scope_host();

        // `read` is inside the with_tools(["read"]) allowlist `.scoped` installs — the flow runs.
        let allowed = DraftAst {
            body: vec![Node::Call {
                op: "read".into(),
                args: vec![],
            }],
            ..Default::default()
        }
        .scoped(["read"]);
        let store = MemStore::new();
        let mut sink = BufferSink::default();
        execute_flow(&store, &host, "s-allowed", &allowed, &mut sink)
            .await
            .expect("an op inside the allowlist must run");

        // `write` is outside it — the same dispatch gate must deny it.
        let denied = DraftAst {
            body: vec![Node::Call {
                op: "write".into(),
                args: vec![],
            }],
            ..Default::default()
        }
        .scoped(["read"]);
        let store2 = MemStore::new();
        let mut sink2 = BufferSink::default();
        let err = execute_flow(&store2, &host, "s-denied", &denied, &mut sink2)
            .await
            .unwrap_err();
        assert!(
            matches!(err, FlowError::Denied(_)),
            "an op outside the allowlist must be denied, got: {err:?}"
        );
    }
}

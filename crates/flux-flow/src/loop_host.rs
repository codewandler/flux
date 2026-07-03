//! `EngineLoopHost` — the engine-side implementation of the reflexive [`LoopHost`] capability that lets
//! the agent loop be written *in flux-lang*. It turns two hidden Rust control-flow moves into two
//! ordinary, audited ops:
//!
//! - **`plan(feedback)`** re-enters the planner ([`compile_turn`]) — the model produces a graph, exactly
//!   as it does for a normal turn. The model stays the planner, never the runtime.
//! - **`run_plan(plan)`** re-enters the interpreter ([`execute_flow`]) over the **same** [`Executor`] and
//!   the **same** session/store, so every inner op still traverses authorization → approval → guarded IO,
//!   and inner symbols/trace land in the one session the next `plan` will see.
//!
//! The no-bypass envelope therefore holds **recursively**: a plan that runs a plan that runs a plan… is
//! the same safety surface at every level, hard-capped by [`MAX_REENTRY_DEPTH`] (the shipped `flux-app`
//! `SpawnOp` depth-guard pattern).
//!
//! **Construction is cyclic.** The host needs the executor it is installed on (to re-enter it), but that
//! executor's [`ToolContext`] holds the host — a cycle. [`install`](EngineLoopHost::install) breaks it
//! with [`Arc::new_cyclic`] + a `Weak<Executor>`, mirroring `flux-app`'s `Engine`.
//!
//! **The sink is shared, not buffered.** `execute_flow` takes a `&mut dyn AgentSink`, but an inner run
//! happens *inside* an outer op — the outer sink is already borrowed. [`SharedSink`] is a proxy over an
//! `Arc<Mutex<dyn AgentSink>>` that locks per write (never across an `.await`), so the outer turn and
//! every inner `run_plan` stream into the **same** surface live, with sub-steps interleaved.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::AgentSink;
use flux_core::{Error, Message, Result, Usage};
use flux_provider::Provider;
use flux_runtime::{CompositeRegisterRequest, CompositeRegistrar, Executor, LoopHost, ToolResult};

use crate::ast::{DraftAst, FailureKind, Node, NodeId};
use crate::compile::{
    compile_turn, parse_completion, render_completion, Brief, CompileOptions, Completion, Phase,
    TurnOutput,
};
use crate::composites::{prepare_registration, CompositeScope, DynamicComposites};
use crate::registry::OpRegistry;
use crate::runtime::execute_flow_resumable_with_composites;
use crate::state::{FlowStore, OpenHalt};

/// Hard cap on reflexive reentry (flow → `run_plan` → flow → …). Mirrors `flux-app`'s `MAX_SPAWN_DEPTH`:
/// a plan that recursively runs plans is stopped here rather than blowing the stack.
const MAX_REENTRY_DEPTH: u32 = 16;

/// The per-turn state the reflexive ops need: which session to run in, the project/identity prompt to
/// seed the planner with, and the live sink. Swapped each turn via [`EngineLoopHost::set_turn`] so a
/// single host (installed once at engine construction) serves every turn — the session and sink vary,
/// the planner and executor do not.
struct TurnCtx {
    session_id: String,
    base_system: Option<String>,
    sink: Arc<Mutex<dyn AgentSink>>,
    /// This turn's evidence-gated advertised op set (A-04). `Some` ⇒ the planner catalog is
    /// restricted to these names and a model-emitted plan naming a hidden op is rejected at
    /// compile; `None` ⇒ unrestricted (tests / hosts that opt out of gating).
    advertised: Option<std::collections::HashSet<String>>,
    /// Where this turn's plan attempts are durably recorded (C-14): the engine's event store plus
    /// the turn id `begin_turn` minted. `None` for hosts without a store (tests, the pre-authored
    /// `flow run` path); a negative turn id is skipped inside `record_plan_attempt`.
    audit: Option<(Arc<flux_events::EventStore>, i64)>,
}

/// Retry-breaker thresholds. Without a guard the agent loop can replay a byte-identical failing (or
/// no-progress) `run_plan` round all the way to the iteration cap (~25), burning ~140 s for nothing.
/// We count consecutive identical `run_plan` transcripts and act: escalate the feedback, then stop.
const STALL_ESCALATE: u32 = 2; // 2nd identical round → inject a stronger "stop repeating" directive
const STALL_STOP: u32 = 4; // 4th identical round → end the turn honestly instead of looping

/// Default max chars of `run_plan` transcript handed back into the next planner round. Per-tool output
/// trimming happens earlier, but the loop feedback is the concatenated plan transcript and can still
/// become large enough to crowd out useful context.
const DEFAULT_LOOP_FEEDBACK_CAP: usize = 40_000;

fn loop_feedback_cap() -> usize {
    std::env::var("FLUX_LOOP_FEEDBACK_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_LOOP_FEEDBACK_CAP)
}

fn cap_loop_feedback_with_cap(transcript: String, cap: usize) -> String {
    if cap == 0 {
        return transcript;
    }
    let total = transcript.chars().count();
    if total <= cap {
        return transcript;
    }
    let kept: String = transcript.chars().take(cap).collect();
    let omitted = total - cap;
    format!(
        "{kept}\n[loop-feedback truncated: {omitted} of {total} chars omitted - reference stored \
         symbols or narrow follow-up reads for exact bytes]"
    )
}

fn cap_loop_feedback(transcript: String) -> String {
    cap_loop_feedback_with_cap(transcript, loop_feedback_cap())
}

fn transcript_hash(transcript: &str) -> String {
    let mut h = Sha256::new();
    h.update(transcript.as_bytes());
    format!("{:x}", h.finalize())
}

/// Render the host-carried [`Brief`] as the text prepended to a planner feedback message (design
/// Part 1 — "the brief is host-carried per-turn and prepended to feedback"). Kept short and
/// machine-legible: the model already saw the fuller `brief` framing in the phase contract.
fn format_brief(b: &Brief) -> String {
    if b.needs.is_empty() {
        format!("[brief] goal: {}", b.goal)
    } else {
        format!(
            "[brief] goal: {}; still need: {}",
            b.goal,
            b.needs.join("; ")
        )
    }
}

fn deterministic_failure_key(transcript: &str) -> Option<String> {
    for line in transcript.lines() {
        if let Some((_prefix, rest)) = line.split_once("error: the argument '") {
            if let Some((flag, tail)) = rest.split_once('\'') {
                if tail.contains("cannot be used multiple times") {
                    return Some(format!("cargo.duplicate_arg:{flag}"));
                }
            }
        }
        if let Some((_prefix, path)) = line.split_once("edit: `old_string` not found in ") {
            let path = path.trim();
            if !path.is_empty() {
                return Some(format!("edit.old_string_not_found:{path}"));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// A-16 — loop-host resume policy + structured feedback contract
// (`docs/designs/multipass-agent-loop.md` Part 2, patch-and-continue)
// ---------------------------------------------------------------------------

/// Mirrors `flux-lang`'s `execute_flow_resumable` ledger-matching walk (its `ledger_end` scan) —
/// duplicated here, over just the AST + ledger, because `run_plan` needs the answer BEFORE executing
/// anything: to scope suffix-only approval (the user is never re-prompted for already-completed
/// writes) and to mark the `flow.plan` observation's ✓-done / ·-to-run lines. A `checkpoint`/`await`
/// node is a free pass-through; any other statement needs a matching ledger entry at that index whose
/// [`crate::runtime::stmt_hash16`] equals the current body's node there. Deliberately ignores the
/// separate durable `checkpoint` cursor (a session-scoped store read, not a pure function of
/// `body`+`ledger`) — the real run may therefore skip MORE than this preview, never less, so a
/// suffix-scoped approval never under-covers a mutating op that will actually run.
fn prospective_skip_len(body: &[Node], ledger: &crate::runtime::ResumeLedger) -> usize {
    let by_node: std::collections::HashMap<u32, &crate::runtime::LedgerEntry> =
        ledger.completed.iter().map(|e| (e.node.0, e)).collect();
    let mut i = 0usize;
    while i < body.len() {
        let matched = match &body[i] {
            Node::Checkpoint { .. } | Node::Await { .. } => true,
            node => matches!(
                by_node.get(&(i as u32)),
                Some(entry) if entry.stmt == crate::runtime::stmt_hash16(node)
            ),
        };
        if !matched {
            break;
        }
        i += 1;
    }
    i
}

/// Render `body`'s top-level statements one per line, each prefixed with a design Part-2 status
/// marker: `✓` done (index `< done_before`, and not the failed one), `✗` the failed statement
/// (`failed`, if given), `·` not yet run. Reuses flux-lang's per-statement
/// [`crate::render::render_statement`] rather than the recursive tree — patch-and-continue's
/// granularity is top-level statements only, matching `execute_flow_resumable`'s F14 scoping.
fn render_marked_plan(body: &[Node], done_before: usize, failed: Option<NodeId>) -> String {
    body.iter()
        .enumerate()
        .map(|(i, node)| {
            let marker = match failed {
                Some(f) if f.0 as usize == i => "✗",
                _ if i < done_before => "✓",
                _ => "·",
            };
            format!(
                "{marker} {i}: {}",
                crate::render::render_statement(node, &crate::render::Palette::PLAIN)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The wire label for a [`FailureKind`] — derived via the SAME `#[serde(rename_all = "snake_case")]`
/// its `RunEvent`/`PlanHalt` fields serialize with, rather than a second hand-written match that could
/// drift from it.
fn failure_kind_label(kind: FailureKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "runtime".to_string())
}

/// Kind-specific guidance appended to a halt's feedback (design Part 2's feedback contract): the
/// model needs different instructions depending on WHY the statement failed — a transient runtime
/// failure invites a surgical retry, an `assert` demands re-planning the remainder, and a denial must
/// never be silently re-emitted (the guard above already refuses that; this is the same message for
/// when the model asks a HUMAN or changes its approach instead).
fn halt_guidance(kind: FailureKind, node: NodeId) -> String {
    let keep = if node.0 > 0 {
        format!(
            "Keep steps 0–{} byte-identical — the runtime will skip them; it re-runs any step you \
             change. ",
            node.0 - 1
        )
    } else {
        String::new()
    };
    match kind {
        FailureKind::Runtime => format!(
            "{keep}Fix step {} and re-emit the corrected plan. If the fix would otherwise repeat an \
             already-completed EFFECTFUL step you want to keep exactly as it ran, guard it with \
             `once` instead of re-emitting it.",
            node.0
        ),
        FailureKind::AssertFailed => format!(
            "{keep}The plan's own invariant broke at step {} — re-plan the remainder from there with \
             a different approach; re-emitting the same assertion unchanged will fail again.",
            node.0
        ),
        FailureKind::Denied | FailureKind::ConfirmDenied => format!(
            "Step {} was denied by policy or the user. Do not re-emit that exact step unchanged — \
             choose a different approach, ask the user, or answer in prose to end the turn.",
            node.0
        ),
    }
}

/// The ephemeral `[resume context]` message a fresh turn's first `plan()` call injects when the
/// session has an open halt latch (design Part 2's cross-turn/cross-process bullet) — never persisted
/// into the conversation store (see [`EngineLoopHost::plan`]), just prepended to this call's working
/// conversation so the model knows a prior turn's plan didn't finish cleanly.
fn resume_context_message(open: &OpenHalt) -> String {
    format!(
        "[resume context] A previous plan halted at step {} ({}): {}. {} statement(s) before it \
         already completed and are still available in this session. If you re-emit a plan, keep \
         those completed steps byte-identical so the runtime skips them — it re-runs any step you \
         change, starting from the failed one.",
        open.halt.node.0,
        failure_kind_label(open.halt.kind),
        open.halt.message,
        open.ledger.completed.len(),
    )
}

/// The symbol a top-level statement binds, if any — for the `completed[{node, stmt, bind, op}]`
/// array in the machine-readable `failure` object (design Part 2's feedback contract). The ledger
/// itself only carries `node`/`stmt`/`value`; `bind`/`op` are looked up in the executed plan's body,
/// mirroring flux-lang's own (private) `top_level_bind` at the node kinds that matter for a legible
/// transcript.
fn stmt_bind_name(node: &Node) -> Option<String> {
    match node {
        Node::Bind { name, .. } => Some(name.to_string()),
        Node::Memo { name, .. } => Some(name.to_string()),
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
        | Node::Once { bind: Some(b), .. } => Some(b.to_string()),
        Node::Each {
            collect: Some(c), ..
        } => Some(c.to_string()),
        Node::Repeat {
            collect: Some(c), ..
        } => Some(c.to_string()),
        _ => None,
    }
}

/// The op a top-level statement dispatched, when directly derivable — mirrors flux-lang's own
/// (private) `top_level_op`: `Some` for a bare `call` or a `$x = call(...)` bind, `None` for a
/// composite/control-flow statement where no single op identifies it.
fn stmt_op(node: &Node) -> Option<String> {
    match node {
        Node::Call { op, .. } => Some(op.clone()),
        Node::Bind { value, .. } => match value.as_ref() {
            Node::Call { op, .. } => Some(op.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The `completed[{node, stmt, bind, op}]` array in the machine-readable `failure` object (design
/// Part 2's feedback contract) — built from `body` (the plan that actually just ran) and `ledger`
/// (that same run's completed-statement memory, whether freshly dispatched or fast-forwarded past).
fn completed_entries_json(body: &[Node], ledger: &crate::runtime::ResumeLedger) -> Vec<Value> {
    ledger
        .completed
        .iter()
        .map(|e| {
            let node = body.get(e.node.0 as usize);
            serde_json::json!({
                "node": e.node.0,
                "stmt": e.stmt,
                "bind": node.and_then(stmt_bind_name),
                "op": node.and_then(stmt_op),
            })
        })
        .collect()
}

/// Per-turn loop-guard state for the retry-breaker. Reset each turn in [`EngineLoopHost::set_turn`].
#[derive(Default)]
struct LoopGuard {
    /// Hash of the previous raw `run_plan` transcript. An unchanged, non-empty transcript means the
    /// round made no progress; keep only the hash so large transcripts don't live in the guard.
    last_transcript_hash: Option<String>,
    /// Consecutive count of that identical transcript.
    transcript_stall: u32,
    /// Stable key for deterministic failures that can be identical even when surrounding transcript
    /// context changes (for example, the same cargo wrapper flag misuse in a different plan).
    last_failure_key: Option<String>,
    /// Consecutive count for [`last_failure_key`].
    failure_stall: u32,
    /// Armed once the stall reaches [`STALL_STOP`]: the honest message the next `plan` returns (as a
    /// `chat`) to terminate the loop via the flow's existing `case "chat"`.
    force_stop: Option<String>,
    /// Hash of the previous round's plan AST, with whether it ran to success. A byte-identical plan
    /// re-emitted right after a SUCCESSFUL run is the silent-success confusion (A-05): the model
    /// couldn't tell the op ran (e.g. an empty-output command) and tries again. `run_plan` skips the
    /// re-execution and says so instead of re-running a possibly destructive op.
    last_plan_hash: Option<String>,
    last_plan_ok: bool,
}

/// The engine-side reflexive host. Holds the stable machinery the two ops need that a `ToolContext`
/// does not carry — the planner (provider + model), the shared store, a `Weak` back to the executor it
/// re-enters — plus the per-turn [`TurnCtx`] (session + sink), updated each turn.
pub struct EngineLoopHost {
    /// Back-reference to the executor whose context holds THIS host. `run_plan` re-enters it so the inner
    /// run shares the SAME perms + approver + evidence + context — no envelope divergence. `Weak` breaks
    /// the executor ⇄ host cycle.
    executor: Weak<Executor>,
    /// The planner, interior-mutable so the REPL `/model` command can swap it on the shared host.
    provider: Mutex<Arc<dyn Provider>>,
    model: Mutex<String>,
    /// Shared with the outer flow run: inner values/symbols/trace land in the SAME session.
    store: Arc<FlowStore>,
    /// Dynamic composite ops registered by the agent or loaded from project/global/session stores.
    composites: Arc<DynamicComposites>,
    /// Active reentry depth, guarding against runaway `run_plan` recursion.
    depth: AtomicU32,
    opts: CompileOptions,
    /// Per-turn session + sink, set by [`set_turn`](Self::set_turn) before each run.
    turn: Mutex<TurnCtx>,
    /// Retry-breaker state, reset per turn — stops a stalled loop from replaying to the cap.
    guard: Mutex<LoopGuard>,
    /// Token usage accumulated across every planner call this turn (the loop calls `plan` once per
    /// iteration). Reset per turn in [`set_turn`]; read by the engine at turn-end for `sink.turn_end`.
    usage: Mutex<Usage>,
    /// Every planner call this turn, individually — `(model, usage)` per [`plan`](Self::plan)
    /// invocation, in call order. This is the attribution record [`usage`] can't give you: `usage` is
    /// the turn TOTAL (replace-style `accumulate`), which loses which model produced which slice once
    /// a mid-turn `/model` switch changes the active planner. Reset per turn in [`set_turn`]; read by
    /// the engine at turn-end to emit one `EventKind::CallUsage` per call (C-06 attribution).
    calls: Mutex<Vec<(String, Usage)>>,
    /// The complete fast-path (A-06): armed by [`run_plan`](Self::run_plan) when a plan carrying a
    /// `complete` directive ran to success (non-suspended). The NEXT [`plan`](Self::plan) call
    /// renders the final message via [`render_completion`] — a toolless, catalog-less call — instead
    /// of a full planner round, and the turn ends through the flow's existing `case "chat"`. Never
    /// armed on rejection, error, suspension, or the identical-plan skip. Reset per turn.
    pending_completion: Mutex<Option<Completion>>,
    /// Monotonic step counter for this turn's durable plan-attempt records (C-14). Reset per turn.
    attempt_step: AtomicU32,
    /// Per-turn token ceiling (A-10): once the turn's accumulated planner usage crosses it, the
    /// next `plan` ends the turn honestly (the stall-stop pattern) instead of consulting the model
    /// again. `None` (the default) = no ceiling. Set by the host surface (flag > env > config);
    /// NOT reset per turn — it is engine configuration, not turn state.
    token_budget: Mutex<Option<u64>>,
    /// The orient/gather grounding artifact (design Part 1's phased loop, A-14): set the moment an
    /// accepted `gather: true` plan carries a `brief`, and prepended to every subsequent planner
    /// feedback message for the rest of the turn (`plan`'s `feedback` reconstruction below) so a
    /// multi-round gather doesn't lose the thread. Reset per turn in [`set_turn`].
    brief: Mutex<Option<Brief>>,
    /// The phase (A-14) the most recent [`plan`](Self::plan) call was asked for — read by
    /// [`run_plan`](Self::run_plan) so its own `PlanAttempt` records (the `"rejected"` outcome) are
    /// phase-stamped too, not only the ones `plan` itself records. Defaults to [`Phase::Execute`],
    /// matching a phase-less call.
    last_phase: Mutex<Phase>,
    /// A-16 cross-turn resume: whether THIS turn's first [`plan`](Self::plan) call has already
    /// injected the ephemeral `[resume context]` message (design Part 2). Set on that first call and
    /// reset per turn in [`set_turn`](Self::set_turn) — a multi-round turn (gather/execute phases)
    /// must not repeat it every round.
    resume_context_shown: Mutex<bool>,
}

impl EngineLoopHost {
    /// Wire the reflexive capability onto `executor`, returning the shared `Arc<Executor>` to drive flows
    /// with **and** the host handle (to [`set_turn`](Self::set_turn) before each run). `store` is shared
    /// with the runs; the initial `session_id`/`sink`/`base_system` seed the first turn.
    ///
    /// Construction is cyclic: the host re-enters this very executor, so it can only be wired in *after*
    /// the executor exists — [`Arc::new_cyclic`] hands us the `Weak<Executor>` to close the loop. The
    /// host is captured out of the constructor through a slot so the caller gets both halves.
    #[allow(clippy::too_many_arguments)]
    pub fn install(
        mut executor: Executor,
        provider: Arc<dyn Provider>,
        model: String,
        base_system: Option<String>,
        store: Arc<FlowStore>,
        composites: Arc<DynamicComposites>,
        session_id: String,
        sink: Arc<Mutex<dyn AgentSink>>,
        opts: CompileOptions,
    ) -> (Arc<Executor>, Arc<EngineLoopHost>) {
        let slot: Arc<Mutex<Option<Arc<EngineLoopHost>>>> = Arc::new(Mutex::new(None));
        let slot2 = slot.clone();
        let executor = Arc::new_cyclic(move |weak: &Weak<Executor>| {
            let host = Arc::new(EngineLoopHost {
                executor: weak.clone(),
                provider: Mutex::new(provider),
                model: Mutex::new(model),
                store,
                composites,
                depth: AtomicU32::new(0),
                opts,
                turn: Mutex::new(TurnCtx {
                    session_id,
                    base_system,
                    sink,
                    advertised: None,
                    audit: None,
                }),
                guard: Mutex::new(LoopGuard::default()),
                usage: Mutex::new(Usage::default()),
                calls: Mutex::new(Vec::new()),
                pending_completion: Mutex::new(None),
                attempt_step: AtomicU32::new(0),
                token_budget: Mutex::new(None),
                brief: Mutex::new(None),
                last_phase: Mutex::new(Phase::default()),
                resume_context_shown: Mutex::new(false),
            });
            *slot2.lock().unwrap() = Some(host.clone());
            executor.set_loop_host(host.clone());
            executor.set_composite_registrar(host);
            executor
        });
        let host = slot.lock().unwrap().take().expect("host captured");
        (executor, host)
    }

    /// Point the host at a new turn: the session to run in, the planner's base system prompt (project
    /// context + matched skills), and the live sink. Called by the engine bootstrap before each turn.
    pub fn set_turn(
        &self,
        session_id: String,
        base_system: Option<String>,
        sink: Arc<Mutex<dyn AgentSink>>,
        advertised: Option<std::collections::HashSet<String>>,
        audit: Option<(Arc<flux_events::EventStore>, i64)>,
    ) {
        *self.turn.lock().unwrap() = TurnCtx {
            session_id,
            base_system,
            sink,
            advertised,
            audit,
        };
        // Fresh turn → reset the retry-breaker so a prior turn's stall never bleeds in.
        *self.guard.lock().unwrap() = LoopGuard::default();
        // …and the per-turn token tally, so usage reflects only this turn's planner calls.
        *self.usage.lock().unwrap() = Usage::default();
        self.calls.lock().unwrap().clear();
        // …and any un-rendered completion directive — it belonged to the prior turn's plan.
        *self.pending_completion.lock().unwrap() = None;
        // …and the plan-attempt step counter (C-14).
        self.attempt_step.store(0, Ordering::SeqCst);
        // …and the orient/gather brief + last-requested phase — both belonged to the prior turn
        // (A-14).
        *self.brief.lock().unwrap() = None;
        *self.last_phase.lock().unwrap() = Phase::default();
        // …and the cross-turn resume-context one-shot (A-16) — a new turn gets its own chance to
        // see it, whether or not the prior turn's latch is still open.
        *self.resume_context_shown.lock().unwrap() = false;
    }

    /// The token usage accumulated across this turn's planner calls (every `plan` re-entry). The
    /// engine reads it once the agent loop completes and hands it to `sink.turn_end`.
    pub fn turn_usage(&self) -> Usage {
        self.usage.lock().unwrap().clone()
    }

    /// Every planner call this turn, individually: `(resolved model, that call's usage)`, in call
    /// order. The engine reads this at turn-end to append one `EventKind::CallUsage` per call — the
    /// per-model attribution `turn_usage`'s single replace-style total can't express (C-06).
    pub fn turn_calls(&self) -> Vec<(String, Usage)> {
        self.calls.lock().unwrap().clone()
    }

    /// Swap the planner (provider + model) — the REPL `/model` command, applied to the shared host so
    /// subsequent `plan` calls use the new model.
    pub fn set_model(&self, provider: Arc<dyn Provider>, model: String) {
        *self.provider.lock().unwrap() = provider;
        *self.model.lock().unwrap() = model;
    }

    /// Install (or clear) the per-turn token ceiling (A-10). Resolution order is the surface's
    /// concern (`--turn-budget` > `FLUX_TURN_TOKEN_BUDGET` > config `[limits] turn_token_budget`).
    pub fn set_token_budget(&self, budget: Option<u64>) {
        *self.token_budget.lock().unwrap() = budget;
    }

    fn executor(&self) -> Result<Arc<Executor>> {
        self.executor
            .upgrade()
            .ok_or_else(|| Error::Other("loop host: the executor is no longer alive".into()))
    }

    /// Retry-breaker: fold one `run_plan` transcript into the loop-guard. An unchanged (non-empty)
    /// transcript means the round made no progress; on repeats we escalate the fed-back directive,
    /// and once the stall persists we arm a hard stop (the next `plan` ends the turn). Returns the
    /// transcript to feed back — augmented with an explicit directive when the loop is repeating.
    fn guard_transcript(&self, transcript: String) -> String {
        self.guard_transcript_with_key(transcript, None)
    }

    /// [`Self::guard_transcript`], but with an explicit deterministic failure key rather than one
    /// sniffed out of the transcript text — A-16's structured `halt:{op}:{stmt}:{kind}` key (built
    /// from a reified [`flux_lang::runtime::PlanHalt`]) is far more precise than
    /// [`deterministic_failure_key`]'s substring heuristics, so a real halt supplies its own key and
    /// falls into the SAME stall-counting/escalation machinery rather than a parallel one.
    fn guard_transcript_with_key(
        &self,
        transcript: String,
        explicit_key: Option<String>,
    ) -> String {
        let mut g = self.guard.lock().unwrap();
        let hash = transcript_hash(&transcript);
        let stalled = !transcript.trim().is_empty()
            && g.last_transcript_hash.as_deref() == Some(hash.as_str());
        if stalled {
            g.transcript_stall += 1;
        } else {
            // New (or empty) transcript = progress: reset the counter and remember this round.
            g.transcript_stall = 1;
            g.last_transcript_hash = Some(hash);
        }

        let failure_key = explicit_key.or_else(|| deterministic_failure_key(&transcript));
        if let Some(key) = failure_key {
            if g.last_failure_key.as_deref() == Some(key.as_str()) {
                g.failure_stall += 1;
            } else {
                g.failure_stall = 1;
                g.last_failure_key = Some(key);
            }
        } else {
            g.failure_stall = 0;
            g.last_failure_key = None;
        }

        let transcript_stall = g.transcript_stall;
        let failure_stall = g.failure_stall;
        let failure_key = g.last_failure_key.clone();
        if let Some(key) = failure_key.as_deref() {
            if failure_stall >= STALL_STOP {
                g.force_stop = Some(format!(
                    "Stopping: the same deterministic failure ({key}) repeated {failure_stall}×, \
                     so re-running plans is not making progress."
                ));
                return format!(
                    "[loop-guard] STOP — the same deterministic failure ({key}) has repeated \
                     {failure_stall}×; the turn will now end.\n{transcript}"
                );
            }
            if failure_stall >= STALL_ESCALATE {
                return format!(
                    "[loop-guard] The same deterministic failure ({key}) has repeated \
                     {failure_stall}×. Do not rerun the same shape: change the failing inputs or \
                     answer in prose if you cannot make progress.\n{transcript}"
                );
            }
        }

        if transcript_stall >= STALL_STOP {
            g.force_stop = Some(format!(
                "Stopping: the last step made no progress — it ran {transcript_stall}× in a row with an \
                 identical result, so I could not complete this turn."
            ));
            return format!(
                "[loop-guard] STOP — this exact step has repeated {transcript_stall}× with no change; the turn \
                 will now end.\n{transcript}"
            );
        }
        if transcript_stall >= STALL_ESCALATE {
            return format!(
                "[loop-guard] You have produced this EXACT same step and result {transcript_stall}× in a row — \
                 it is NOT making progress. Do not repeat it: change the call (e.g. supply the \
                 missing argument) or take a different approach; if you cannot, answer in prose.\n\
                 {transcript}"
            );
        }
        transcript
    }

    /// Durably record one planning attempt (C-14): `accepted` (with the AST fingerprint + rendered
    /// plan text), `chat`, `compile_error`, or `rejected`. Non-fatal, no-op when the turn has no
    /// audit target (tests, the pre-authored `flow run` path).
    ///
    /// C-22: the rendered `plan_text` (and any error string) is scrubbed through the SAME
    /// [`Redactor`](flux_secret::Redactor) the executor uses on tool results (seeded per C-13)
    /// before it reaches the store — a `bash("curl -H 'Authorization: Bearer …'")` arg in the
    /// accepted plan graph would otherwise persist in the clear on the `PlanAttempted` event.
    fn record_plan_attempt(&self, mut attempt: flux_events::PlanAttempt) {
        let audit = self.turn.lock().unwrap().audit.clone();
        if let Some((events, turn_id)) = audit {
            if let Ok(executor) = self.executor() {
                let redactor = &executor.context().redactor;
                if let Some(text) = attempt.plan_text.take() {
                    attempt.plan_text = Some(redactor.redact(&text));
                }
                if let Some(err) = attempt.error.take() {
                    attempt.error = Some(redactor.redact(&err));
                }
            }
            let session_id = self.turn.lock().unwrap().session_id.clone();
            let _ = events.record_plan_attempt(&session_id, turn_id, attempt);
        }
    }

    /// The next 1-based step number for this turn's plan-attempt records.
    fn next_attempt_step(&self) -> u32 {
        self.attempt_step.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// Cap for the durable rendered-plan text on an accepted attempt — the auditable graph, not a
/// transcript dump (C-14).
const PLAN_TEXT_CAP: usize = 8_000;

fn cap_plan_text(text: String) -> String {
    if text.chars().count() <= PLAN_TEXT_CAP {
        return text;
    }
    let kept: String = text.chars().take(PLAN_TEXT_CAP).collect();
    format!("{kept}\n[plan text truncated]")
}

#[async_trait]
impl LoopHost for EngineLoopHost {
    /// Re-enter the planner over the **working conversation**: the persisted session log (the real
    /// `user → assistant` history, including the user's request) plus the loop-carried `$feedback` —
    /// last iteration's results — appended as an ephemeral turn. The feedback accretes across `repeat`
    /// iterations via last-writer-wins symbol rebinding (no new machinery), so each `plan` sees what the
    /// prior `run_plan` produced without any of it being persisted. Returns a `Plan`:
    /// `{kind: "plan", ast, complete}` for an emitted graph or `{kind: "chat", text}` for a prose answer.
    async fn plan(&self, input: Value) -> Result<Value> {
        // A-14: which pass of the phased turn loop is asking — threaded through the already-opaque
        // JSON input (no `LoopHost` trait change). Absent/unrecognized ⇒ `Execute`, the
        // byte-compatible contract a phase-less call (an ejected/overridden pre-A-14 loop, the
        // one-shot `--plan`/`/plan` surfaces) must keep getting. Recorded for `run_plan`'s own
        // audit trail (the "rejected" outcome) before anything below can return early.
        let phase = Phase::from_wire(input.get("phase").and_then(|v| v.as_str()));
        *self.last_phase.lock().unwrap() = phase;
        // Observed at `plan()` entry (design Part 1) so a surface's spinner can read "orienting…" /
        // "gathering…" / "revising…" — independent of whichever branch below produces the result.
        {
            let obs = flux_evidence::Observation::new(
                "loop.phase",
                flux_evidence::Phase::Turn,
                serde_json::json!({ "phase": phase.as_str() }),
            );
            if let Ok(executor) = self.executor() {
                executor.observe(obs.clone());
            }
            SharedSink(self.turn.lock().unwrap().sink.clone()).observation(&obs);
        }
        // Retry-breaker hard stop: a prior `run_plan` flagged a stalled loop. End the turn honestly
        // as a prose answer (the flow's `case "chat"` terminates the loop) instead of re-planning.
        let force_stop = self.guard.lock().unwrap().force_stop.take();
        if let Some(msg) = force_stop {
            // A chat result always settles the phased loop's gather pass (design Part 1): "" is
            // reserved for an accepted `gather: true` plan.
            return Ok(serde_json::json!({ "kind": "chat", "text": msg, "settled": "true" }));
        }
        // Token-budget ceiling (A-10), the same stall-stop pattern: when the turn's accumulated
        // planner usage has crossed the installed budget, end the turn honestly instead of paying
        // another model call. Checked BEFORE the completion fast-path too — the ceiling is hard.
        let budget = *self.token_budget.lock().unwrap();
        if let Some(budget) = budget {
            let used = self.usage.lock().unwrap().total();
            if used >= budget {
                let obs = flux_evidence::Observation::new(
                    "turn.budget_exceeded",
                    flux_evidence::Phase::Turn,
                    serde_json::json!({ "budget": budget, "used": used }),
                );
                if let Ok(executor) = self.executor() {
                    executor.observe(obs.clone());
                }
                SharedSink(self.turn.lock().unwrap().sink.clone()).observation(&obs);
                return Ok(serde_json::json!({
                    "kind": "chat",
                    "text": format!(
                        "Stopping: this turn's token budget ({budget} tokens) is exhausted \
                         ({used} used). Results so far are in the session — raise --turn-budget \
                         (or FLUX_TURN_TOKEN_BUDGET / [limits] turn_token_budget) or narrow the \
                         request to continue."
                    ),
                    "settled": "true",
                }));
            }
        }
        let feedback = input
            .get("feedback")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Brief carry (design Part 1): once an orient/gather round's plan is accepted with a
        // `brief`, it is host-carried per-turn and prepended to every subsequent planner feedback
        // message — so a multi-round gather (or the execute phase that follows it) doesn't lose the
        // thread. Read here (the state a PRIOR round's acceptance left behind); written below after
        // this round's `compile_turn` call.
        let brief_prefix = self.brief.lock().unwrap().as_ref().map(format_brief);
        let feedback = match brief_prefix {
            Some(prefix) if feedback.trim().is_empty() => prefix,
            Some(prefix) => format!("{prefix}\n\n{feedback}"),
            None => feedback,
        };
        // Snapshot the per-turn session + base system + advertised set (drop the lock before the
        // model call).
        let (session_id, base_system, advertised) = {
            let t = self.turn.lock().unwrap();
            (
                t.session_id.clone(),
                t.base_system.clone(),
                t.advertised.clone(),
            )
        };
        // Working conversation = persisted history + the loop-carried feedback (ephemeral). This is
        // engine.rs's `working` vector, relocated into the op so the loop itself stays in flux-lang.
        let mut conversation = self.store.conversation(&session_id).unwrap_or_default();
        // A-16 cross-turn resume (design Part 2): the FIRST `plan()` call of a turn — not every
        // phase round within it — injects one ephemeral `[resume context]` message when the session
        // still has an open halt latch from a prior turn (or process). Never persisted: it is pushed
        // onto THIS call's working conversation only, exactly like the loop-carried `feedback` below.
        {
            let mut shown = self.resume_context_shown.lock().unwrap();
            if !*shown {
                *shown = true;
                if let Ok(Some(open)) = self.store.open_halted_plan(&session_id) {
                    conversation.push(Message::user_text(resume_context_message(&open)));
                }
            }
        }
        if !feedback.trim().is_empty() {
            conversation.push(Message::user_text(feedback));
        }
        // Degenerate case (no history, no feedback): still hand the planner a turn to act on.
        if conversation.is_empty() {
            conversation.push(Message::user_text(""));
        }

        // The complete fast-path (A-06): the previous `run_plan` ran a completion-carrying plan to
        // success, so the model already said how to close the turn. Render the final message from
        // the working conversation (the fed-back results included) with a toolless, catalog-less
        // call — no second full planner round — and end the turn via the flow's `case "chat"`.
        // A render failure falls through to a normal planner round: a summary hiccup must never
        // abort a turn whose work already succeeded.
        let pending = self.pending_completion.lock().unwrap().take();
        if let Some(directive) = pending {
            let provider = self.provider.lock().unwrap().clone();
            let model = self.model.lock().unwrap().clone();
            match render_completion(
                &*provider,
                &model,
                &conversation,
                &directive,
                self.opts.max_tokens,
            )
            .await
            {
                Ok((text, call_usage)) if !text.trim().is_empty() => {
                    self.usage.lock().unwrap().accumulate(&call_usage);
                    let spec = flux_core::canonical_model_spec(Some(provider.name()), &model);
                    self.calls.lock().unwrap().push((spec, call_usage));
                    return Ok(
                        serde_json::json!({ "kind": "chat", "text": text, "settled": "true" }),
                    );
                }
                Ok((_, call_usage)) => {
                    // An empty render still cost tokens; account for them, then re-plan normally.
                    self.usage.lock().unwrap().accumulate(&call_usage);
                    let spec = flux_core::canonical_model_spec(Some(provider.name()), &model);
                    self.calls.lock().unwrap().push((spec, call_usage));
                }
                Err(e) => {
                    let sink = self.turn.lock().unwrap().sink.clone();
                    SharedSink(sink).observation(&flux_evidence::Observation::new(
                        "completion.render_error",
                        flux_evidence::Phase::Turn,
                        serde_json::json!({ "error": e.to_string() }),
                    ));
                }
            }
        }

        let executor = self.executor()?;
        self.composites
            .ensure_session_loaded(&self.store, &session_id)?;
        // The evidence-gated catalog (A-04): the engine computes the turn's advertised set once
        // (`surfaced_for_turn`) and every loop iteration plans against it — advertisement filters
        // what the model sees, and `compile_turn` rejects a plan naming a hidden op.
        let mut ops = OpRegistry::new(executor.registry())
            .with_owned_composites(self.composites.active_for_session(&session_id));
        if let Some(advertised) = advertised {
            ops = ops.with_advertised(advertised);
        }
        let view = self.store.view(&session_id)?;
        let view_ref = (!view.symbols.is_empty()).then_some(&view);
        let provider = self.provider.lock().unwrap().clone();
        let model = self.model.lock().unwrap().clone();
        // UX (A-12): the planning call is otherwise silent in a normal turn — the REPL `/plan` path
        // (`engine::plan_turn`) already brackets its `compile_turn` call with `sink.planning(true/false)`
        // and forwards live thinking-token deltas; this is the one mode users actually live in, so wire
        // the same plumbing here. `PlanningGuard`'s `Drop` fires `planning(false)` on every exit —
        // success, a compile error, or a future early `?` — so the "composing plan…" indicator can never
        // get stuck on.
        let sink = self.turn.lock().unwrap().sink.clone();
        let _planning = PlanningGuard::start(sink.clone());
        let mut thinking_sink = SharedSink::new(sink);
        // A-14: the phase resolved from `input` at entry threads straight into `compile_turn`,
        // selecting that pass's instruction segment (orient's three-way contract, gather's
        // keep-gathering-or-settle contract, or execute's byte-compatible default).
        let (out, call_usage) = match compile_turn(
            &*provider,
            &model,
            &conversation,
            base_system.as_deref(),
            &ops,
            view_ref,
            None,
            Some(&mut thinking_sink),
            self.opts.clone(),
            phase,
        )
        .await
        {
            Ok(v) => v,
            // Surface a provider failure (credit/auth/rate-limit/transport) as a readable sentence —
            // the error flows out through the op envelope and becomes the turn's answer, never raw
            // JSON. Recorded durably as a failed attempt first (C-14).
            Err(e) => {
                let msg = crate::engine::planner_error(&e);
                self.record_plan_attempt(flux_events::PlanAttempt {
                    step: self.next_attempt_step(),
                    outcome: "compile_error".into(),
                    error: Some(msg.clone()),
                    phase: Some(phase.as_str().to_string()),
                    ..Default::default()
                });
                return Err(Error::Other(msg));
            }
        };
        // Fold this planner call's tokens into the per-turn tally the engine reports at turn-end, AND
        // record it individually (with the model that was ACTIVE for this call) so a mid-turn `/model`
        // switch attributes tokens/cost to the right model (C-06 attribution) rather than being lost in
        // the turn's single replace-style total.
        self.usage.lock().unwrap().accumulate(&call_usage);
        // Stamped with the CANONICAL provider/model spec (C-15) so `cost_summary` never splits one
        // backend's spend across key variants.
        let spec = flux_core::canonical_model_spec(Some(provider.name()), &model);
        self.calls.lock().unwrap().push((spec, call_usage));

        let plan = match out {
            // `complete` carries the model's full directive (object) or null — `run_plan` re-parses
            // it off this value to arm the fast-path after a successful run (A-06).
            TurnOutput::Plan(c) => {
                // Durable audit (C-14): the accepted plan's identity (fingerprint) + its readable
                // graph — "a turn is a readable graph", persisted.
                let fingerprint =
                    transcript_hash(&serde_json::to_string(&c.ast).unwrap_or_default());
                self.record_plan_attempt(flux_events::PlanAttempt {
                    step: self.next_attempt_step(),
                    outcome: "accepted".into(),
                    fingerprint: Some(fingerprint),
                    plan_text: Some(cap_plan_text(crate::render::render_pretty(&c.ast))),
                    phase: Some(phase.as_str().to_string()),
                    ..Default::default()
                });
                // Design Part 1 — settled: "" only for an accepted `gather: true` plan (the phased
                // loop's `until $settled` gate); truthy for the full execution plan, so it flows
                // straight into the standard plan/execute/revise pass.
                let settled = if c.gather { "" } else { "true" };
                // `c.brief` is only ever `Some` alongside `c.gather` (see `compile.rs`'s
                // `parse_brief` call site), so this alone gates both.
                if let Some(brief) = &c.brief {
                    // Host-carried per turn (reset in `set_turn`): the moment a brief is accepted
                    // it is stored for every subsequent feedback message, and observed so a
                    // surface can render it (A-15).
                    *self.brief.lock().unwrap() = Some(brief.clone());
                    let obs = flux_evidence::Observation::new(
                        "flow.brief",
                        flux_evidence::Phase::Turn,
                        serde_json::json!({ "goal": brief.goal, "needs": brief.needs }),
                    );
                    if let Ok(executor) = self.executor() {
                        executor.observe(obs.clone());
                    }
                    SharedSink(self.turn.lock().unwrap().sink.clone()).observation(&obs);
                }
                serde_json::json!({
                    "kind": "plan",
                    "ast": serde_json::to_value(&c.ast).unwrap_or(Value::Null),
                    "complete": c.complete.as_ref().map(|d| serde_json::json!({
                        "primer": d.primer,
                        "instructions": d.instructions,
                    })),
                    "settled": settled,
                })
            }
            TurnOutput::Chat(text) => {
                self.record_plan_attempt(flux_events::PlanAttempt {
                    step: self.next_attempt_step(),
                    outcome: "chat".into(),
                    phase: Some(phase.as_str().to_string()),
                    ..Default::default()
                });
                serde_json::json!({ "kind": "chat", "text": text, "settled": "true" })
            }
        };
        Ok(plan)
    }

    /// Re-enter the interpreter to run the plan's `ast` in the current session, streaming live. A `chat`
    /// plan has nothing to run, so its text is surfaced as the result. Bounded by [`MAX_REENTRY_DEPTH`].
    ///
    /// A-17: a thin normalizing wrapper over [`Self::run_plan_dispatch`] (the actual order of
    /// operations, unchanged). Only the halt/denial-guard return paths there set a `failure` key —
    /// this makes it ALWAYS present (`null` when this round didn't halt) before returning, because
    /// flux-lang's dotted field-access sugar (`$ran.failure` lowers to a `jq` lookup) runtime-errors
    /// on a genuinely MISSING key, and the loop now routes on `$ran.failure` every execute round.
    /// `null` reads falsy via the runtime's `json_truthy`, matching the `$plan.settled` `""`/non-empty
    /// idiom the loop already relies on — see `run_plan_failure_field_is_always_present_never_missing`.
    async fn run_plan(&self, plan: Value) -> Result<Value> {
        let mut out = self.run_plan_dispatch(plan).await?;
        if let Some(obj) = out.as_object_mut() {
            obj.entry("failure").or_insert(Value::Null);
        }
        Ok(out)
    }
}

impl EngineLoopHost {
    /// The full `run_plan` order of operations (design Part 2 / A-16), wrapped by the thin
    /// [`LoopHost::run_plan`] above so every return path is normalized there in one place.
    async fn run_plan_dispatch(&self, plan: Value) -> Result<Value> {
        // Depth guard: increment, ensure we decrement on every exit, then check.
        let prev = self.depth.fetch_add(1, Ordering::SeqCst);
        let _guard = DepthGuard(&self.depth);
        if prev >= MAX_REENTRY_DEPTH {
            return Err(Error::Other(format!(
                "run_plan: reflexive reentry exceeded max depth {MAX_REENTRY_DEPTH}"
            )));
        }

        if plan.get("kind").and_then(|v| v.as_str()) == Some("chat") {
            let text = plan
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Ok(serde_json::json!({ "transcript": "", "result": text, "steps": 0 }));
        }

        let ast_val = plan
            .get("ast")
            .cloned()
            .ok_or_else(|| Error::Other("run_plan: the plan has no `ast` to run".into()))?;
        let ast: DraftAst = serde_json::from_value(ast_val)
            .map_err(|e| Error::Other(format!("run_plan: invalid plan ast: {e}")))?;
        // The plan's completion directive (A-06), if any. Armed only at the success point below —
        // every early return (identical-skip, rejection, error) and a suspension must NOT arm it,
        // or the turn would close on a summary of work that didn't happen.
        let completion = parse_completion(plan.get("complete"));

        // Silent-success guard (A-05): a byte-identical plan re-emitted right after a SUCCESSFUL
        // run means the model couldn't read the success (e.g. an empty-output command) — don't
        // re-run it (it may be non-idempotent, like `rm`); tell the model instead. If the model
        // insists, this same informational transcript repeats and the transcript-stall guard
        // escalates/stops the turn.
        let plan_fingerprint = transcript_hash(&serde_json::to_string(&ast).unwrap_or_default());
        {
            let g = self.guard.lock().unwrap();
            if g.last_plan_ok && g.last_plan_hash.as_deref() == Some(plan_fingerprint.as_str()) {
                return Ok(serde_json::json!({
                    "transcript": "[loop-guard] This EXACT plan already ran SUCCESSFULLY in the \
                                   previous round — it was NOT re-run. An empty result there means \
                                   the operation succeeded with no output. Use those results, or \
                                   answer in prose to finish the turn.",
                    "result": "",
                    "steps": 0,
                }));
            }
        }

        let (session_id, sink) = {
            let t = self.turn.lock().unwrap();
            (t.session_id.clone(), t.sink.clone())
        };
        let executor = self.executor()?;
        self.composites
            .ensure_session_loaded(&self.store, &session_id)?;
        let composites = self.composites.active_for_session(&session_id);
        // A fresh proxy over the shared sink: the inner run streams live, interleaved under the outer op.
        let mut sink = SharedSink(sink);

        // A-16: fold the session's open halt latch (design Part 2's `run_plan` order of
        // operations, step 2). `None` = a fresh start (no prior halt, or it was already consumed by
        // a `PlanResumed`) — every step below degrades to today's strict, non-resuming behavior.
        let open_halt = self.store.open_halted_plan(&session_id)?;

        // Step 3 — denial re-emission guard: policy/the user already said no to this EXACT
        // statement. Never silently patched around: refuse without executing. A DIFFERENT approach
        // (any edit that changes that one statement's content hash) flows through to the resume
        // path below like any other correction.
        if let Some(open) = &open_halt {
            if matches!(
                open.halt.kind,
                FailureKind::Denied | FailureKind::ConfirmDenied
            ) {
                let reemitted = ast
                    .body
                    .iter()
                    .any(|n| crate::runtime::stmt_hash16(n) == open.halt.stmt);
                if reemitted {
                    return Ok(serde_json::json!({
                        "transcript": format!(
                            "[plan halted — {}] The statement previously refused (\"{}\") is \
                             unchanged in this plan. It was NOT re-run. Choose a different \
                             approach, ask the user, or answer in prose to end the turn.",
                            failure_kind_label(open.halt.kind),
                            open.halt.message,
                        ),
                        "result": "",
                        "steps": 0,
                        "failure": {
                            "node": open.halt.node.0,
                            "stmt": open.halt.stmt,
                            "op": open.halt.op,
                            "kind": failure_kind_label(open.halt.kind),
                            "fatal": open.halt.kind.is_fatal(),
                            "message": open.halt.message,
                            "plan": open.halt.plan,
                            "completed": open.ledger.completed.iter().map(|e| serde_json::json!({
                                "node": e.node.0,
                                "stmt": e.stmt,
                                "bind": Value::Null,
                                "op": Value::Null,
                            })).collect::<Vec<_>>(),
                        },
                    }));
                }
            }
        }

        // Step 4 — compute the prospective skip prefix IN-HOST, before executing anything: scopes
        // suffix-only risk/approval (the user is never re-prompted to approve already-completed
        // writes) and marks the surfaced plan tree with ✓-done / ·-to-run.
        let skip_len = open_halt
            .as_ref()
            .map(|open| prospective_skip_len(&ast.body, &open.ledger))
            .unwrap_or(0);

        // Divergence-before-failure warning: the re-emitted plan changed a statement BEFORE the
        // point the previous halt reached, so steps that would have been skipped will now RE-RUN —
        // including their side effects. The model must be told explicitly (design Part 2).
        let rerun_warning = open_halt.as_ref().and_then(|open| {
            let old_index = open.halt.node.0 as usize;
            (skip_len < old_index).then(|| {
                format!(
                    "[warning] steps {skip_len}–{} changed from the halted plan and will RE-RUN \
                     (including their side effects) — only steps 0–{} matched exactly.",
                    old_index.saturating_sub(1),
                    skip_len.saturating_sub(1),
                )
            })
        });

        // Surface the compiled plan BEFORE executing it — auditable, and so the user sees what is about
        // to run before any per-op approval prompt (the `flow.plan` observation the surfaces render as
        // the plan tree). The inner ops then stream + gate live underneath. Risk/approval are scoped to
        // the SUFFIX that will actually run when resuming (suffix-scoped approval, step 4).
        let risk = if skip_len > 0 {
            let mut suffix_ast = ast.clone();
            suffix_ast.body = ast.body[skip_len..].to_vec();
            crate::runtime::plan_risk_with_composites(&suffix_ast, executor.registry(), &composites)
        } else {
            crate::runtime::plan_risk_with_composites(&ast, executor.registry(), &composites)
        };
        let resumed = open_halt.is_some();
        let plan_render = if resumed {
            render_marked_plan(&ast.body, skip_len, None)
        } else {
            crate::render::render_pretty(&ast)
        };
        // A-17 (closes the A-15 residual): the incoming `plan`'s own `settled` field (set by
        // `plan()` from the model's `gather: true` tag, "" while still gathering) tells the surface
        // directly whether THIS run is the bounded read-only gather pass — cheaper and more precise
        // than the CLI/TUI's prior `loop.phase`/`flow.brief`-order inference, which couldn't tell an
        // orient-phase gather plan apart from orient emitting the full plan directly when the model
        // omitted a usable `brief`. `resumed` similarly hands the surface an explicit signal for
        // when `plan` carries per-statement ✓/✗/· status markers instead of a fresh full tree.
        let gather = plan.get("settled").and_then(|v| v.as_str()) == Some("");
        let phase = self.last_phase.lock().unwrap().as_str().to_string();
        sink.observation(&flux_evidence::Observation::new(
            "flow.plan",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "plan": plan_render,
                "plan_ast": serde_json::to_value(&ast).unwrap_or(Value::Null),
                "risk": risk.summary(),
                "ops": risk.ops.len(),
                "gather": gather,
                "phase": phase,
                "resumed": resumed,
            }),
        ));

        // Plan-level approval: the user approves the plan as a whole here — its tree + aggregate risk
        // were just surfaced. A read-only plan needs no approval; a mutating one prompts ONCE, and then
        // every inner op runs without a per-op prompt (the scope guard, held across `execute_flow`,
        // tells `dispatch` to skip the gate). `always` trusts all plans for the session. On rejection we
        // feed a stop-signal back so the model ends the turn in prose rather than re-planning forever.
        let _scope = if risk.mutating {
            // Let the surface drain + render the plan tree (the observation above) BEFORE we print the
            // approval prompt and — on a TTY — switch to raw mode for the keypress. A CLI renders the
            // observation asynchronously (drained off an mpsc as the loop yields); without this yield the
            // prompt can print ahead of the tree, and the tree can render under raw mode (no `\n`→`\r\n`),
            // staircasing it. Yielding here parks us so the drain runs in cooked mode first.
            tokio::task::yield_now().await;
            match executor.approve_plan(&risk.approval_request()).await {
                Some(scope) => Some(scope),
                None => {
                    self.record_plan_attempt(flux_events::PlanAttempt {
                        step: self.next_attempt_step(),
                        outcome: "rejected".into(),
                        fingerprint: Some(plan_fingerprint.clone()),
                        phase: Some(self.last_phase.lock().unwrap().as_str().to_string()),
                        ..Default::default()
                    });
                    return Ok(serde_json::json!({
                        "transcript": "[plan rejected by user] The user declined to run this plan. \
                                       Do not propose another — acknowledge briefly and stop.",
                        "result": "",
                        "steps": 0,
                    }));
                }
            }
        } else {
            None
        };

        // Step 5 — execute RESUMABLE with the ledger: a failing top-level statement is reified onto
        // `outcome.failure` instead of propagating `Err` (composites/nested bodies keep strict `Err`
        // propagation, unchanged — F14). Passing `ledger: Some(..)` here is what consumes the open
        // halt latch (the runtime appends `PlanResumed` even at zero skips) — the authored
        // `flux flow run` path never calls this resumable wrapper (L-25).
        let ledger = open_halt.as_ref().map(|open| &open.ledger);
        let outcome = match execute_flow_resumable_with_composites(
            self.store.as_ref(),
            executor.as_ref(),
            &session_id,
            &ast,
            &composites,
            ledger,
            &mut sink,
        )
        .await
        {
            Ok(o) => o,
            // An INFRASTRUCTURE failure only (store IO, …) — a statement's own failure is reified
            // onto `outcome.failure` by the resumable runtime instead of `Err` (A-16). Feed the
            // error back exactly as before so the loop re-plans.
            Err(e) => {
                {
                    let mut g = self.guard.lock().unwrap();
                    g.last_plan_hash = Some(plan_fingerprint);
                    g.last_plan_ok = false; // a failed plan may legitimately be retried (repaired)
                }
                let transcript = self.guard_transcript(format!(
                    "[plan error] {e}\nAdjust and emit another plan, or answer in prose."
                ));
                let transcript = cap_loop_feedback(transcript);
                return Ok(serde_json::json!({
                    "transcript": transcript,
                    "result": "",
                    "steps": 0,
                }));
            }
        };

        // Step 6 — a reified top-level statement failure (design Part 2): the runtime already
        // ledgered the completed prefix and appended `PlanHalted` — build the structured feedback
        // contract instead of erroring the turn. Never arms the complete fast-path.
        if let Some(halt) = &outcome.failure {
            {
                let mut g = self.guard.lock().unwrap();
                g.last_plan_hash = Some(plan_fingerprint);
                g.last_plan_ok = false;
            }
            // The FULL completed ledger for THIS run (fast-forwarded + freshly dispatched, up to the
            // failure) — re-fold rather than reconstruct: `execute_flow_resumable_with_composites`
            // just wrote exactly this data as `StatementCompleted` events, and folding it back is
            // cheaper and less error-prone than rebuilding it from `outcome` + the prior ledger.
            let completed_ledger = self
                .store
                .open_halted_plan(&session_id)?
                .map(|p| p.ledger)
                .unwrap_or_default();
            let total = ast.body.len();
            let step_n = halt.node.0 as usize + 1;
            // A-17: a discrete, real-time UI cue the moment a plan halts — right where the rest of
            // the feedback contract is built — so the surface can print `✗ step N/M <op> failed —
            // revising…` instead of the halt being legible only inside the fed-back transcript text.
            sink.observation(&flux_evidence::Observation::new(
                "flow.halt",
                flux_evidence::Phase::Turn,
                serde_json::json!({
                    "step": step_n,
                    "of": total,
                    "op": halt.op,
                    "kind": failure_kind_label(halt.kind),
                    "fatal": halt.kind.is_fatal(),
                }),
            ));
            let marked = render_marked_plan(&ast.body, halt.node.0 as usize, Some(halt.node));
            let guidance = halt_guidance(halt.kind, halt.node);
            let mut transcript = format!(
                "{}\n\n[plan halted at step {step_n} of {total}] {}\n{marked}\n{guidance}",
                outcome.transcript, halt.message,
            );
            if let Some(w) = &rerun_warning {
                transcript = format!("{w}\n\n{transcript}");
            }
            let transcript = self.guard_transcript_with_key(
                transcript,
                Some(format!(
                    "halt:{}:{}:{}",
                    halt.op.as_deref().unwrap_or("-"),
                    halt.stmt,
                    failure_kind_label(halt.kind)
                )),
            );
            let transcript = cap_loop_feedback(transcript);
            return Ok(serde_json::json!({
                "transcript": transcript,
                "result": outcome.result,
                "steps": outcome.steps,
                "failure": {
                    "node": halt.node.0,
                    "stmt": halt.stmt,
                    "op": halt.op,
                    "kind": failure_kind_label(halt.kind),
                    "fatal": halt.kind.is_fatal(),
                    "message": halt.message,
                    "plan": halt.plan,
                    "completed": completed_entries_json(&ast.body, &completed_ledger),
                },
            }));
        }

        {
            let mut g = self.guard.lock().unwrap();
            g.last_plan_hash = Some(plan_fingerprint);
            // A suspension is not a completed success — a repeat after resume is legitimate.
            g.last_plan_ok = outcome.suspension.is_none();
        }
        // Success, not suspended: arm the complete fast-path — the NEXT `plan` renders the final
        // message from these results instead of paying a full planner round (A-06).
        if outcome.suspension.is_none() {
            if let Some(directive) = completion {
                *self.pending_completion.lock().unwrap() = Some(directive);
            }
        }

        // A suspension means the turn is awaiting input — real progress, not a stall — so skip the
        // retry-breaker and feed back the raw transcript.
        let transcript = if outcome.suspension.is_some() {
            outcome.transcript.clone()
        } else {
            self.guard_transcript(outcome.transcript.clone())
        };
        let mut transcript = cap_loop_feedback(transcript);
        if let Some(w) = &rerun_warning {
            transcript = format!("{w}\n\n{transcript}");
        }
        let mut out = serde_json::json!({
            "transcript": transcript,
            "result": outcome.result,
            "steps": outcome.steps,
        });
        if let Some(susp) = &outcome.suspension {
            out["suspension"] = serde_json::json!({ "source": susp.source, "node": susp.node.0 });
        }
        Ok(out)
    }
}

#[async_trait]
impl CompositeRegistrar for EngineLoopHost {
    async fn register_composite(&self, request: CompositeRegisterRequest) -> Result<Value> {
        let (scope, decl, source, replace) = prepare_registration(request)?;
        let session_id = {
            let t = self.turn.lock().unwrap();
            t.session_id.clone()
        };
        let executor = self.executor()?;
        self.composites
            .ensure_session_loaded(&self.store, &session_id)?;
        self.composites.validate_registration(
            scope,
            &session_id,
            &decl,
            replace,
            executor.registry(),
        )?;

        let path = match scope {
            CompositeScope::Turn => None,
            CompositeScope::Session => {
                self.store
                    .save_session_composite(&session_id, &decl.name, &source)?;
                None
            }
            CompositeScope::Project => {
                let path = scope.path_for(&decl.name).expect("project has path");
                executor.context().system.write_file(&path, &source).await?;
                Some(path)
            }
            CompositeScope::Global => {
                if !executor
                    .context()
                    .system
                    .workspace()
                    .has_named_root("global_ops")
                {
                    return Err(Error::Other(
                        "op.register: global scope is unavailable because @global_ops is not \
                         configured"
                            .into(),
                    ));
                }
                let path = scope.path_for(&decl.name).expect("global has path");
                executor.context().system.write_file(&path, &source).await?;
                Some(path)
            }
        };

        self.composites
            .install(scope, &session_id, decl.clone(), replace)?;
        let obs = flux_evidence::Observation::new(
            "op.registered",
            flux_evidence::Phase::Turn,
            serde_json::json!({
                "name": decl.name,
                "scope": scope.as_str(),
                "path": path,
                "replace": replace,
            }),
        );
        executor.observe(obs);

        Ok(serde_json::json!({
            "name": decl.name,
            "scope": scope.as_str(),
            "persisted": matches!(scope, CompositeScope::Session | CompositeScope::Project | CompositeScope::Global),
            "path": path,
        }))
    }
}

/// Decrements the active reentry depth when a `run_plan` unwinds (success, error, or early return).
struct DepthGuard<'a>(&'a AtomicU32);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// RAII bracket for `plan`'s otherwise-silent compile wait (A-12): [`Self::start`] fires
/// `sink.planning(true)` immediately, and `Drop` guarantees the matching `planning(false)` on every
/// exit — the happy path, a compile error, or any future early `?` — so a surface's "composing plan…"
/// indicator (or spinner) can never get stuck on. Mirrors [`DepthGuard`]'s drop-based bracketing.
struct PlanningGuard(Arc<Mutex<dyn AgentSink>>);

impl PlanningGuard {
    fn start(sink: Arc<Mutex<dyn AgentSink>>) -> Self {
        SharedSink(sink.clone()).planning(true);
        Self(sink)
    }
}

impl Drop for PlanningGuard {
    fn drop(&mut self) {
        SharedSink(self.0.clone()).planning(false);
    }
}

/// A live [`AgentSink`] proxy over a shared handle. Every method locks the shared sink for the duration
/// of that **one** call only — never across an `.await` — so the outer turn and an inner `run_plan` can
/// both stream into the same surface without buffering and without a second `&mut` borrow. Construct one
/// per run (cheap: an `Arc` clone) and pass `&mut` of it to `execute_flow`.
pub struct SharedSink(Arc<Mutex<dyn AgentSink>>);

impl SharedSink {
    /// Wrap a shared sink handle. The outer flow and every inner run get their own `SharedSink` over the
    /// same `Arc<Mutex<…>>`, so all streamed output lands on one surface.
    pub fn new(sink: Arc<Mutex<dyn AgentSink>>) -> Self {
        Self(sink)
    }
}

impl AgentSink for SharedSink {
    fn text_delta(&mut self, t: &str) {
        self.0.lock().unwrap().text_delta(t);
    }
    fn thinking_delta(&mut self, t: &str) {
        self.0.lock().unwrap().thinking_delta(t);
    }
    fn planning(&mut self, active: bool) {
        self.0.lock().unwrap().planning(active);
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        self.0.lock().unwrap().tool_call(name, input);
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        self.0.lock().unwrap().tool_result(name, result);
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.0.lock().unwrap().observation(o);
    }
    fn turn_end(&mut self, usage: Option<flux_core::Usage>) {
        self.0.lock().unwrap().turn_end(usage);
    }
}

/// One captured [`AgentSink`] call, forwarded over a channel. The agent-loop bootstrap drives the loop
/// flow through an owned, shareable [`ChannelSink`] (so the `'static` loop host can hold it for reentrant
/// `run_plan`), and drains these events onto its **borrowed** `&mut dyn AgentSink` *concurrently* with the
/// run — live streaming, no buffer-then-replay, and `run_turn`'s signature is untouched (the mpsc proxy
/// the design called for).
pub enum SinkEvent {
    Text(String),
    Thinking(String),
    Planning(bool),
    ToolCall(String, Value),
    ToolResult(String, ToolResult),
    Observation(flux_evidence::Observation),
    TurnEnd(Option<flux_core::Usage>),
}

impl SinkEvent {
    /// Replay this captured call onto a real sink (called by the bootstrap's drain loop).
    pub fn apply(self, sink: &mut dyn AgentSink) {
        match self {
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

/// An [`AgentSink`] that forwards every call as a [`SinkEvent`] over an unbounded channel. Owned and
/// cheap to share (just the sender behind the `Arc<Mutex<…>>`); the bootstrap drains the receiver onto
/// the real sink as events arrive.
pub struct ChannelSink(tokio::sync::mpsc::UnboundedSender<SinkEvent>);

impl ChannelSink {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<SinkEvent>) -> Self {
        Self(tx)
    }
}

impl AgentSink for ChannelSink {
    fn text_delta(&mut self, t: &str) {
        let _ = self.0.send(SinkEvent::Text(t.to_string()));
    }
    fn thinking_delta(&mut self, t: &str) {
        let _ = self.0.send(SinkEvent::Thinking(t.to_string()));
    }
    fn planning(&mut self, active: bool) {
        let _ = self.0.send(SinkEvent::Planning(active));
    }
    fn tool_call(&mut self, name: &str, input: &Value) {
        let _ = self
            .0
            .send(SinkEvent::ToolCall(name.to_string(), input.clone()));
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        let _ = self
            .0
            .send(SinkEvent::ToolResult(name.to_string(), result.clone()));
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        let _ = self.0.send(SinkEvent::Observation(o.clone()));
    }
    fn turn_end(&mut self, usage: Option<flux_core::Usage>) {
        let _ = self.0.send(SinkEvent::TurnEnd(usage));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use serde_json::json;

    use crate::runtime::execute_flow;
    use flux_core::{Chunk, ContentBlock, StopReason};
    use flux_provider::{ChunkStream, Request};
    use flux_runtime::{
        AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
    };
    use flux_spec::ToolSpec;
    use flux_system::{System, Workspace};

    /// A provider that replays canned chunk sequences, one per `stream()` call (mirrors the engine tests).
    struct MockProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// Echo the `text` param back as content.
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
        async fn execute(&self, _c: &ToolContext, params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// A sink that records (into shared handles) the op names dispatched, text streamed, planning
    /// brackets, thinking-token deltas, and observations — so the test can inspect what reached the
    /// shared surface after the sink is moved behind the `Arc<Mutex<…>>`.
    #[derive(Clone, Default)]
    struct Recorder {
        tools: Arc<Mutex<Vec<String>>>,
        text: Arc<Mutex<String>>,
        planning: Arc<Mutex<Vec<bool>>>,
        thinking: Arc<Mutex<String>>,
        observations: Arc<Mutex<Vec<flux_evidence::Observation>>>,
    }
    struct RecSink(Recorder);
    impl AgentSink for RecSink {
        fn text_delta(&mut self, t: &str) {
            self.0.text.lock().unwrap().push_str(t);
        }
        fn thinking_delta(&mut self, t: &str) {
            self.0.thinking.lock().unwrap().push_str(t);
        }
        fn planning(&mut self, active: bool) {
            self.0.planning.lock().unwrap().push(active);
        }
        fn tool_call(&mut self, name: &str, _input: &Value) {
            self.0.tools.lock().unwrap().push(name.to_string());
        }
        fn observation(&mut self, o: &flux_evidence::Observation) {
            self.0.observations.lock().unwrap().push(o.clone());
        }
    }

    /// One model turn that emits a plan via the `emit_plan` tool call carrying `ast`.
    fn emit_plan(ast: Value) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "p1".into(),
                name: "emit_plan".into(),
                input: json!({ "ast": ast }),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    /// One model turn that answers in prose (a chat turn).
    fn prose(text: &str) -> Vec<Chunk> {
        vec![
            Chunk::TextDelta(text.to_string()),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ]
    }

    /// One model turn that emits a bounded read-only `gather: true` plan (A-14) carrying `ast` and
    /// a `brief`.
    fn emit_gather_plan(ast: Value, goal: &str, needs: &[&str]) -> Vec<Chunk> {
        vec![
            Chunk::Block(ContentBlock::ToolUse {
                id: "p1".into(),
                name: "emit_plan".into(),
                input: json!({
                    "ast": ast,
                    "gather": true,
                    "brief": { "goal": goal, "needs": needs },
                }),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::ToolUse),
            },
        ]
    }

    /// A one-node echo plan AST, `$name = echo(marker)`.
    fn echo_ast(name: &str, marker: &str) -> Value {
        json!({ "body": [{
            "kind": "bind", "name": name,
            "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": marker }] }
        }]})
    }

    #[test]
    fn loop_feedback_cap_bounds_long_transcripts_and_can_be_disabled() {
        let capped = cap_loop_feedback_with_cap("abcdef".to_string(), 3);
        assert!(capped.starts_with("abc\n[loop-feedback truncated: 3 of 6 chars omitted"));
        assert_eq!(
            cap_loop_feedback_with_cap("abcdef".to_string(), 0),
            "abcdef"
        );
    }

    /// Build a minimal host (no canned plans needed) for exercising the retry-breaker directly.
    fn guard_test_host() -> Arc<EngineLoopHost> {
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(Recorder::default())));
        let dir = std::env::temp_dir().join(format!("flux-loop-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (_executor, host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store,
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared,
            CompileOptions::default(),
        );
        host
    }

    /// A-04: the loop-host planner advertises only the turn's surfaced op set. Before the fix,
    /// `plan` built an UNGATED `OpRegistry` — every registered op (incl. `bash` with the `shell`
    /// group off) was rendered into the planner catalog every iteration.
    #[tokio::test]
    async fn plan_advertises_only_the_turn_surfaced_ops() {
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(Recorder::default())));
        let dir = std::env::temp_dir().join(format!("flux-loop-adv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg); // includes `bash` (group: shell) and `read`
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![prose("nothing to do")])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (_executor, host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "mock".into(),
            None,
            store,
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );
        // The engine hands the turn's surfaced set to the host: everything except `bash`.
        let advertised: std::collections::HashSet<String> = {
            let ex = host.executor().unwrap();
            let names = OpRegistry::new(ex.registry()).op_names();
            names.into_iter().filter(|n| n != "bash").collect()
        };
        host.set_turn("sess".into(), None, shared, Some(advertised), None);

        let out = host.plan(json!({ "feedback": "" })).await.unwrap();
        assert_eq!(out["kind"], "chat");
        let systems = provider.systems.lock().unwrap();
        let sys = systems.first().expect("one planner call");
        assert!(sys.contains("- read("), "surfaced ops stay in the catalog");
        assert!(
            !sys.contains("- bash("),
            "a hidden op must not be advertised to the model"
        );
    }

    /// A-05: a byte-identical plan re-emitted right after a SUCCESSFUL run (the silent-success
    /// confusion) is NOT re-executed — `run_plan` returns an informational transcript instead, so a
    /// non-idempotent op (`rm`) can't run twice because its empty output looked like "nothing
    /// happened".
    #[tokio::test]
    async fn run_plan_skips_an_identical_plan_after_success() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir = std::env::temp_dir().join(format!("flux-loop-a05-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["plan".into(), "run_plan".into(), "echo".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (_executor, host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store,
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared,
            CompileOptions::default(),
        );
        // An echo of "" — a silent success (empty view).
        let plan = json!({
            "kind": "plan",
            "ast": { "body": [
                { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "" }] }
            ]}
        });
        let first = host.run_plan(plan.clone()).await.unwrap();
        assert_eq!(first["steps"], 1);
        assert!(
            first["transcript"]
                .as_str()
                .unwrap()
                .contains("✓ ok (no output)"),
            "the silent success must be legible in the feedback: {}",
            first["transcript"]
        );
        let second = host.run_plan(plan).await.unwrap();
        assert_eq!(second["steps"], 0, "the identical plan must not re-run");
        assert!(
            second["transcript"]
                .as_str()
                .unwrap()
                .contains("already ran SUCCESSFULLY"),
            "{}",
            second["transcript"]
        );
        let echoes = rec
            .tools
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.as_str() == "echo")
            .count();
        assert_eq!(echoes, 1, "echo dispatched exactly once across both rounds");
    }

    /// Fix 2: an identical `run_plan` transcript repeating means the loop is not progressing. The
    /// retry-breaker stays quiet on the first occurrence, escalates the fed-back directive on repeats,
    /// and arms a hard stop once the stall persists — so a stuck loop ends in a few rounds, not 25.
    #[tokio::test]
    async fn retry_breaker_escalates_then_arms_a_hard_stop() {
        let host = guard_test_host();
        let stuck = format!(
            "[plan error] python_run: provide either `script` or `module`\n{}",
            "x".repeat(DEFAULT_LOOP_FEEDBACK_CAP + 1024)
        );
        // Round 1 (stall=1): first sighting — not flagged.
        assert!(!host.guard_transcript(stuck.clone()).contains("loop-guard"));
        // Round 2 (stall=2 = STALL_ESCALATE): escalate, no stop yet.
        let r2 = host.guard_transcript(stuck.clone());
        assert!(r2.contains("[loop-guard]") && r2.contains("NOT making progress"));
        assert!(host.guard.lock().unwrap().force_stop.is_none());
        // Round 3 (stall=3): still escalating.
        assert!(host
            .guard_transcript(stuck.clone())
            .contains("[loop-guard]"));
        assert!(host.guard.lock().unwrap().force_stop.is_none());
        // Round 4 (stall=4 = STALL_STOP): hard stop armed — the next `plan` will end the turn.
        assert!(host.guard_transcript(stuck.clone()).contains("STOP"));
        assert!(host.guard.lock().unwrap().force_stop.is_some());
        // A changed transcript = progress: the counter resets and nothing is flagged.
        assert!(!host
            .guard_transcript("a different result".into())
            .contains("loop-guard"));
    }

    #[tokio::test]
    async fn retry_breaker_escalates_on_repeated_cargo_duplicate_flag_failure() {
        let host = guard_test_host();
        let first = "[plan error] runtime error: step `cargo_build` failed: error: the argument \
                     '--workspace' cannot be used multiple times\nUsage: cargo build [OPTIONS]";
        let second = "[plan error] runtime error: step `cargo_test` failed: error: the argument \
                      '--workspace' cannot be used multiple times\nUsage: cargo test [OPTIONS]";
        let different =
            "[plan error] runtime error: step `cargo_clippy` failed: error: the argument \
                         '--all-targets' cannot be used multiple times";

        assert!(!host.guard_transcript(first.into()).contains("loop-guard"));
        let repeated = host.guard_transcript(second.into());
        assert!(repeated.contains("[loop-guard]"));
        assert!(repeated.contains("cargo.duplicate_arg:--workspace"));
        assert!(repeated.contains("Do not rerun the same shape"));
        assert!(!host
            .guard_transcript(different.into())
            .contains("loop-guard"));
    }

    #[tokio::test]
    async fn retry_breaker_escalates_on_repeated_stale_edit_anchor() {
        let host = guard_test_host();
        let first =
            "[plan error] runtime error: step `edit` failed: edit: `old_string` not found in \
                     crates/flux-cli/src/skill_cmd.rs";
        let second = "[plan error] runtime error: step `edit` failed after reread: edit: \
                      `old_string` not found in crates/flux-cli/src/skill_cmd.rs";

        assert!(!host.guard_transcript(first.into()).contains("loop-guard"));
        let repeated = host.guard_transcript(second.into());
        assert!(repeated.contains("[loop-guard]"));
        assert!(repeated.contains("edit.old_string_not_found:crates/flux-cli/src/skill_cmd.rs"));
    }

    /// The keystone: `plan` re-enters the planner (the mock emits a one-node echo plan), `run_plan`
    /// re-enters the interpreter over the SAME executor + session, the inner `echo` streams live into the
    /// SAME shared sink as the outer turn, and its result flows back out as the flow's return.
    #[tokio::test]
    async fn plan_then_run_plan_reenters_and_streams_live() {
        // A shared live sink, recording into handles the test keeps.
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));

        // Executor over echo + the reflexive ops, all pre-allowed; allow-all approver.
        let dir = std::env::temp_dir().join(format!("flux-loop-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into(), "plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );

        // The planner will emit a one-node plan: $g = echo("hi").
        let echo_ast = json!({
            "body": [{
                "kind": "bind", "name": "g",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "hi" }] }
            }]
        });
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::from(vec![emit_plan(echo_ast)])),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());

        // Wire the reflexive capability onto the executor (shares the same envelope + session + sink).
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        // The outer flow IS the loop, in flux-lang: $p = plan(...); $out = run_plan($p); return $out.
        let outer_ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "p",
                  "value": { "kind": "call", "op": "plan",
                             "args": [{ "kind": "lit", "value": "echo hi for me" }] } },
                { "kind": "bind", "name": "out",
                  "value": { "kind": "call", "op": "run_plan",
                             "args": [{ "kind": "var", "name": "p" }] } },
                { "kind": "return", "value": { "kind": "var", "name": "out" } }
            ]
        }))
        .unwrap();

        let mut outer = SharedSink::new(shared.clone());
        let outcome = execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &outer_ast,
            &mut outer,
        )
        .await
        .unwrap();

        // Every level streamed into the ONE shared sink: the outer ops AND the inner echo.
        let tools = rec.tools.lock().unwrap().clone();
        assert!(
            tools.contains(&"plan".to_string()),
            "plan dispatched: {tools:?}"
        );
        assert!(
            tools.contains(&"run_plan".to_string()),
            "run_plan dispatched: {tools:?}"
        );
        assert!(
            tools.contains(&"echo".to_string()),
            "the INNER echo streamed live into the shared sink: {tools:?}"
        );
        // The interleaving is real: echo (inner) comes after run_plan (outer) opened.
        let run_at = tools.iter().position(|t| t == "run_plan").unwrap();
        let echo_at = tools.iter().position(|t| t == "echo").unwrap();
        assert!(echo_at > run_at, "echo runs inside run_plan: {tools:?}");

        // The inner plan's result flowed back out: the flow returns the run_plan Outcome carrying "hi".
        assert!(
            outcome.result.contains("hi"),
            "outer return carries the inner Outcome: {}",
            outcome.result
        );

        // The plan was actually executed in the shared session: $g bound to the echo output.
        let g = store
            .resolve("sess", &crate::ast::SymbolName("g".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap());
        assert_eq!(g, Some(crate::ast::Value::String("hi".into())));

        // One audit trail across the reentry: the INNER run's `tool_call` markers land in the SAME
        // shared evidence log as the outer ops — because `run_plan` re-enters the same executor/context.
        let calls: Vec<String> = executor
            .evidence()
            .by_kind("tool_call")
            .filter_map(|o| {
                o.data
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .map(String::from)
            })
            .collect();
        assert!(
            calls.contains(&"echo".to_string()) && calls.contains(&"run_plan".to_string()),
            "inner + outer tool calls share one evidence log: {calls:?}"
        );
    }

    /// A-12: in a *normal* turn (not the REPL `/plan` toggle), the planning call was silent —
    /// `sink.planning(true/false)` was only ever invoked from `engine::plan_turn`. This drives a
    /// normal `plan()` reentry (the same op the flux-lang agent loop calls every iteration) over the
    /// shared sink and asserts the "composing plan…" bracket now reaches it.
    #[tokio::test]
    async fn normal_turn_planning_state_reaches_the_sink() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir =
            std::env::temp_dir().join(format!("flux-loop-a12-planning-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::from(vec![prose("nothing to do")])),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        // A normal turn's `plan()` call — no REPL `/plan` toggle involved.
        let outer_ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "p",
                  "value": { "kind": "call", "op": "plan",
                             "args": [{ "kind": "lit", "value": "" }] } },
                { "kind": "return", "value": { "kind": "var", "name": "p" } }
            ]
        }))
        .unwrap();

        let mut outer = SharedSink::new(shared.clone());
        execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &outer_ast,
            &mut outer,
        )
        .await
        .unwrap();

        let planning = rec.planning.lock().unwrap().clone();
        assert_eq!(
            planning,
            vec![true, false],
            "the normal-turn plan() call must bracket the compile wait with planning(true/false): \
             {planning:?}"
        );
    }

    /// A-12: thinking-token deltas the provider streams during a normal turn's planner call must reach
    /// the sink too — before the fix, `EngineLoopHost::plan` passed `thinking_sink: None` into
    /// `compile_turn`, so `Chunk::ThinkingDelta` chunks were silently dropped instead of forwarded.
    #[tokio::test]
    async fn normal_turn_streams_thinking_deltas() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir =
            std::env::temp_dir().join(format!("flux-loop-a12-thinking-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let thinking_turn = vec![
            Chunk::ThinkingDelta("pondering the request…".into()),
            Chunk::TextDelta("nothing to do".into()),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ];
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::from(vec![thinking_turn])),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        let outer_ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "p",
                  "value": { "kind": "call", "op": "plan",
                             "args": [{ "kind": "lit", "value": "" }] } },
                { "kind": "return", "value": { "kind": "var", "name": "p" } }
            ]
        }))
        .unwrap();

        let mut outer = SharedSink::new(shared.clone());
        execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &outer_ast,
            &mut outer,
        )
        .await
        .unwrap();

        let thinking = rec.thinking.lock().unwrap().clone();
        assert!(
            thinking.contains("pondering the request…"),
            "thinking-token deltas from a normal turn's plan() call must reach the sink: {thinking:?}"
        );
    }

    #[tokio::test]
    async fn op_register_session_composite_is_callable_and_reloads_from_store() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        let (executor, host) = setup_host(
            reg,
            PermissionManager::from_rules(
                &["op.register".into(), "run_plan".into(), "echo".into()],
                &[],
            ),
            Arc::new(AllowApprover),
            rec,
            "session-register",
        );

        let source = r#"
op greet(name: String) -> String
  description "greet through echo"
  risk "low"
  idempotency "idempotent"
  effects [read]
  expose true

  $out = echo($name)
  return $out
"#;
        let registered = executor
            .dispatch(
                "op.register",
                json!({ "source": source, "scope": "session" }),
            )
            .await;
        assert!(
            !registered.is_error,
            "registration failed: {}",
            registered.content
        );

        let out = host
            .run_plan(json!({"kind":"plan","ast":{"body":[
                {"kind":"call","op":"greet","args":[{"kind":"lit","value":"Ada"}]}
            ]}}))
            .await
            .unwrap();
        assert!(
            out["result"].as_str().unwrap_or("").contains("Ada"),
            "{out}"
        );

        let saved = host.store.session_composites("sess").unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0, "greet");

        let reloaded = DynamicComposites::default();
        reloaded.ensure_session_loaded(&host.store, "sess").unwrap();
        let names: Vec<_> = reloaded
            .active_for_session("sess")
            .into_iter()
            .map(|op| op.name)
            .collect();
        assert_eq!(names, vec!["greet"]);
    }

    #[tokio::test]
    async fn op_register_project_writes_flux_source_and_global_requires_named_root() {
        let tag = "project-register";
        let dir =
            std::env::temp_dir().join(format!("flux-planapproval-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        let (executor, _host) = setup_host(
            reg,
            PermissionManager::from_rules(&["op.register".into()], &[]),
            Arc::new(AllowApprover),
            rec,
            tag,
        );

        let source = r#"
op project_greet(name: String) -> String
  risk "low"
  idempotency "idempotent"
  effects [read]

  $out = echo($name)
  return $out
"#;
        let registered = executor
            .dispatch(
                "op.register",
                json!({ "source": source, "scope": "project" }),
            )
            .await;
        assert!(
            !registered.is_error,
            "project registration failed: {}",
            registered.content
        );
        let written = std::fs::read_to_string(dir.join(".flux/ops/project_greet.flux")).unwrap();
        assert!(written.starts_with("op project_greet(name: String) -> String"));
        assert!(written.contains("  expose true"));

        let global = executor
            .dispatch(
                "op.register",
                json!({ "source": source, "scope": "global", "replace": true }),
            )
            .await;
        assert!(global.is_error, "global should require @global_ops");
        assert!(
            global.content.contains("global scope is unavailable"),
            "{}",
            global.content
        );
    }

    /// A provider that records the conversation each `stream()` call saw, then replays a canned response.
    struct RecordingProvider {
        responses: Mutex<VecDeque<Vec<Chunk>>>,
        seen: Mutex<Vec<String>>,
        systems: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        fn name(&self) -> &str {
            "rec"
        }
        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            // `system_text()` joins the segmented system (A-03) — the planner carries its catalog
            // in segments, so recording `req.system` alone would capture nothing.
            self.systems
                .lock()
                .unwrap()
                .push(req.system_text().unwrap_or_default());
            let convo = req
                .messages
                .iter()
                .map(|m| m.text().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            self.seen.lock().unwrap().push(convo);
            let chunks = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// P4: the loop-carried working conversation. A 2-round `repeat` rebinds `$feedback` to the last
    /// `run_plan` Outcome each iteration (last-writer-wins symbol rebinding, no new machinery); round 2's
    /// `plan` therefore sees round 1's transcript in the conversation handed to the planner.
    #[tokio::test]
    async fn loop_carried_feedback_lets_round_two_see_round_one() {
        let rec_sink = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec_sink.clone())));

        let dir = std::env::temp_dir().join(format!("flux-loop-feedback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into(), "plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );

        // Round 1 emits a plan echoing a unique marker; round 2 echoes another.
        let r1 = json!({ "body": [{ "kind": "call", "op": "echo",
            "args": [{ "kind": "lit", "value": "ROUND1-MARKER" }] }] });
        let r2 = json!({ "body": [{ "kind": "call", "op": "echo",
            "args": [{ "kind": "lit", "value": "ROUND2-DONE" }] }] });
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![emit_plan(r1), emit_plan(r2)])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });

        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        // $feedback = ""; repeat 2 { $p = plan($feedback); $r = run_plan($p); $feedback = fmt("{r}") }
        // (`bind` only accepts call/expr/fmt/jq/parse/thing, so the loop-carry copies $r via `fmt`.)
        let loop_ast: DraftAst = serde_json::from_value(json!({
            "body": [
                { "kind": "bind", "name": "feedback", "value": { "kind": "fmt", "template": "" } },
                { "kind": "repeat", "max": 2, "body": [
                    { "kind": "bind", "name": "p", "value": { "kind": "call", "op": "plan",
                        "args": [{ "kind": "var", "name": "feedback" }] } },
                    { "kind": "bind", "name": "r", "value": { "kind": "call", "op": "run_plan",
                        "args": [{ "kind": "var", "name": "p" }] } },
                    { "kind": "bind", "name": "feedback", "value": { "kind": "fmt", "template": "{r}" } }
                ]}
            ]
        }))
        .unwrap();

        let mut outer = SharedSink::new(shared.clone());
        execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &loop_ast,
            &mut outer,
        )
        .await
        .unwrap();

        let seen = provider.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "the planner was consulted twice");
        assert!(
            !seen[0].contains("ROUND1-MARKER"),
            "round 1's planner saw an empty working conversation: {:?}",
            seen[0]
        );
        assert!(
            seen[1].contains("ROUND1-MARKER"),
            "round 2's planner saw round 1's transcript via loop-carried $feedback: {:?}",
            seen[1]
        );
    }

    #[tokio::test]
    async fn plan_includes_existing_visible_session_symbols() {
        let rec_sink = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec_sink.clone())));

        let dir = std::env::temp_dir().join(format!("flux-loop-symbols-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["echo".into(), "plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );

        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![emit_plan(json!({ "body": [] }))])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let vid = store
            .put_value(
                "sess",
                &crate::ast::Value::String("important cached context".into()),
            )
            .unwrap();
        store
            .bind(
                "sess",
                &crate::ast::SymbolName("cached".into()),
                &vid,
                Some("text"),
                "read src/lib.rs: important cached context",
                crate::ast::Visibility::Visible,
            )
            .unwrap();

        let (_executor, host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        host.plan(json!({ "feedback": "" })).await.unwrap();

        let systems = provider.systems.lock().unwrap().clone();
        assert_eq!(systems.len(), 1);
        assert!(
            systems[0].contains("Existing session symbols")
                && systems[0].contains("$cached: text = read src/lib.rs: important cached context"),
            "planner prompt should include visible session symbols:\n{}",
            systems[0]
        );
    }

    #[tokio::test]
    async fn builtin_agent_loop_shows_symbols_created_by_prior_round_to_next_plan() {
        let rec_sink = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec_sink.clone())));

        let dir =
            std::env::temp_dir().join(format!("flux-loop-symbol-roundtrip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        flux_tools::register_evidence(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );

        let first_plan = json!({
            "body": [{
                "kind": "bind", "name": "g",
                "value": { "kind": "call", "op": "echo", "args": [{ "kind": "lit", "value": "ROUND-SYMBOL" }] }
            }]
        });
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![emit_plan(first_plan), prose("done")])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
        let mut outer = SharedSink::new(shared.clone());
        execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &loop_ast,
            &mut outer,
        )
        .await
        .unwrap();

        let systems = provider.systems.lock().unwrap().clone();
        assert_eq!(systems.len(), 2, "builtin loop should plan twice");
        assert!(
            !systems[0].contains("$g"),
            "first planner call should not see a symbol before it is created"
        );
        assert!(
            systems[1].contains("Existing session symbols")
                && systems[1].contains("$g = ROUND-SYMBOL"),
            "second planner call should see the symbol bound by round one:\n{}",
            systems[1]
        );
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A mutating echo: declares a `FilesystemWrite` intent so `plan_risk` marks the plan mutating
    /// (and therefore approval-worthy). Not pre-allowed, so without plan-level approval it would prompt.
    struct WriteEchoTool;
    #[async_trait]
    impl Tool for WriteEchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "writeecho",
                "echo (mutating)",
                json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            )
        }
        fn intents(&self, _p: &Value) -> flux_spec::IntentSet {
            use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};
            let mut s = flux_spec::IntentSet::new();
            s.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path {
                    path: "out.txt".into(),
                },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
            s
        }
        async fn execute(&self, _c: &ToolContext, p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                p.get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// Counts plan-level vs per-op approval calls. Per-op `request` returns `Deny`, so any op that
    /// wrongly prompts fails — a clean run proves the per-op gate was skipped.
    struct CountingApprover {
        plan_calls: Arc<AtomicUsize>,
        op_calls: Arc<AtomicUsize>,
        allow: bool,
    }
    #[async_trait]
    impl flux_runtime::Approver for CountingApprover {
        async fn request(
            &self,
            _t: &str,
            _s: &[String],
            _i: &flux_spec::IntentSet,
        ) -> flux_runtime::ApprovalChoice {
            self.op_calls.fetch_add(1, Ordering::SeqCst);
            flux_runtime::ApprovalChoice::Deny
        }
        async fn request_plan(
            &self,
            _plan: &flux_runtime::PlanApprovalRequest,
        ) -> flux_runtime::ApprovalChoice {
            self.plan_calls.fetch_add(1, Ordering::SeqCst);
            if self.allow {
                flux_runtime::ApprovalChoice::Allow
            } else {
                flux_runtime::ApprovalChoice::Deny
            }
        }
    }

    /// Returns the executor `Arc` alongside the host — the host holds only a `Weak<Executor>`, so the
    /// caller must keep this alive for the duration of the run (the real engine does via `FlowEngine`).
    fn setup_host(
        reg: ToolRegistry,
        perms: PermissionManager,
        approver: Arc<dyn flux_runtime::Approver>,
        rec: Recorder,
        tag: &str,
    ) -> (Arc<Executor>, Arc<EngineLoopHost>) {
        let dir =
            std::env::temp_dir().join(format!("flux-planapproval-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let executor = Executor::new(reg, perms, approver, ToolContext::new(system));
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec)));
        EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store,
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared,
            CompileOptions::default(),
        )
    }

    fn mutating_plan() -> serde_json::Value {
        json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"writeecho","args":[{"kind":"lit","value":"hi"}]}
        ]}})
    }

    /// A mutating plan is approved ONCE at the plan level; its ops then run with no per-op prompt.
    #[tokio::test]
    async fn run_plan_approves_a_mutating_plan_once_then_runs_all_ops() {
        let rec = Recorder::default();
        let plan_calls = Arc::new(AtomicUsize::new(0));
        let op_calls = Arc::new(AtomicUsize::new(0));
        let approver = Arc::new(CountingApprover {
            plan_calls: plan_calls.clone(),
            op_calls: op_calls.clone(),
            allow: true,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteEchoTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::new(),
            approver,
            rec.clone(),
            "approve-once",
        );

        host.run_plan(mutating_plan()).await.unwrap();

        assert_eq!(
            plan_calls.load(Ordering::SeqCst),
            1,
            "asked once, at the plan level"
        );
        assert_eq!(
            op_calls.load(Ordering::SeqCst),
            0,
            "no per-op prompt inside the approved plan"
        );
        assert!(
            rec.tools.lock().unwrap().contains(&"writeecho".to_string()),
            "the op ran"
        );
    }

    /// Rejecting the plan runs nothing and feeds a stop-signal back.
    #[tokio::test]
    async fn run_plan_rejection_runs_nothing() {
        let rec = Recorder::default();
        let plan_calls = Arc::new(AtomicUsize::new(0));
        let op_calls = Arc::new(AtomicUsize::new(0));
        let approver = Arc::new(CountingApprover {
            plan_calls: plan_calls.clone(),
            op_calls: op_calls.clone(),
            allow: false,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteEchoTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::new(),
            approver,
            rec.clone(),
            "reject",
        );

        let out = host.run_plan(mutating_plan()).await.unwrap();

        assert_eq!(plan_calls.load(Ordering::SeqCst), 1);
        assert!(
            out["transcript"]
                .as_str()
                .unwrap_or("")
                .contains("rejected"),
            "rejection is fed back: {out}"
        );
        assert!(
            !rec.tools.lock().unwrap().contains(&"writeecho".to_string()),
            "a rejected plan runs no ops"
        );
    }

    /// A read-only plan needs no approval at all — neither a plan prompt nor a per-op prompt.
    #[tokio::test]
    async fn run_plan_skips_approval_for_a_read_only_plan() {
        let rec = Recorder::default();
        let plan_calls = Arc::new(AtomicUsize::new(0));
        let op_calls = Arc::new(AtomicUsize::new(0));
        let approver = Arc::new(CountingApprover {
            plan_calls: plan_calls.clone(),
            op_calls: op_calls.clone(),
            allow: true,
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        // Reads are pre-allowed (as in the real CLI), so a pure-read plan never gates.
        let perms = PermissionManager::from_rules(&["echo".into()], &[]);
        let (_ex, host) = setup_host(reg, perms, approver, rec.clone(), "readonly");

        host.run_plan(json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"echo","args":[{"kind":"lit","value":"hi"}]}
        ]}}))
        .await
        .unwrap();

        assert_eq!(
            plan_calls.load(Ordering::SeqCst),
            0,
            "no plan prompt for a read-only plan"
        );
        assert_eq!(
            op_calls.load(Ordering::SeqCst),
            0,
            "no per-op prompt either"
        );
        assert!(
            rec.tools.lock().unwrap().contains(&"echo".to_string()),
            "the read ran"
        );
    }

    /// A-14 named acceptance: a trivial (chat) turn and a simple/actionable (full-plan) turn make
    /// EXACTLY as many provider calls as the pre-phased loop did — Pass 1's orient call IS the
    /// first (and, for chat, only) planner call, and Pass 2's gather loop body never runs when
    /// orient already settled. Zero added latency for tasks that don't need it.
    #[tokio::test]
    async fn orient_fast_path_adds_no_provider_calls() {
        // Scenario A: trivial request -> orient answers in prose. One provider call, full stop.
        {
            let rec = Recorder::default();
            let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
            let dir = std::env::temp_dir()
                .join(format!("flux-loop-a14-orient-chat-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
            let mut reg = ToolRegistry::new();
            reg.register(Arc::new(EchoTool));
            flux_tools::register_reflect(&mut reg);
            flux_tools::register_evidence(&mut reg);
            let executor = Executor::new(
                reg,
                PermissionManager::from_rules(
                    &[
                        "echo".into(),
                        "plan".into(),
                        "run_plan".into(),
                        "observe".into(),
                    ],
                    &[],
                ),
                Arc::new(AllowApprover),
                ToolContext::new(system),
            );
            let provider = Arc::new(RecordingProvider {
                responses: Mutex::new(VecDeque::from(vec![prose("nothing to do")])),
                seen: Mutex::new(Vec::new()),
                systems: Mutex::new(Vec::new()),
            });
            let store = Arc::new(FlowStore::in_memory().unwrap());
            let (executor, _host) = EngineLoopHost::install(
                executor,
                provider.clone(),
                "rec".into(),
                None,
                store.clone(),
                Arc::new(DynamicComposites::default()),
                "sess".into(),
                shared.clone(),
                CompileOptions::default(),
            );
            let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
            let mut outer = SharedSink::new(shared.clone());
            execute_flow(
                store.as_ref(),
                executor.as_ref(),
                "sess",
                &loop_ast,
                &mut outer,
            )
            .await
            .unwrap();

            assert_eq!(
                provider.seen.lock().unwrap().len(),
                1,
                "trivial turn: exactly one provider call, as today"
            );
            assert_eq!(
                executor.evidence().by_kind("turn.gather").count(),
                0,
                "the gather pass must not run when orient already settled"
            );
        }

        // Scenario B: simple/actionable request -> orient emits the full execution plan
        // (non-gather); the next call (execute phase) answers in prose. Two provider calls total,
        // exactly like the pre-phased loop's plan-then-plan-again shape.
        {
            let rec = Recorder::default();
            let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
            let dir = std::env::temp_dir()
                .join(format!("flux-loop-a14-orient-plan-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
            let mut reg = ToolRegistry::new();
            reg.register(Arc::new(EchoTool));
            flux_tools::register_reflect(&mut reg);
            flux_tools::register_evidence(&mut reg);
            let executor = Executor::new(
                reg,
                PermissionManager::from_rules(
                    &[
                        "echo".into(),
                        "plan".into(),
                        "run_plan".into(),
                        "observe".into(),
                    ],
                    &[],
                ),
                Arc::new(AllowApprover),
                ToolContext::new(system),
            );
            let provider = Arc::new(RecordingProvider {
                responses: Mutex::new(VecDeque::from(vec![
                    emit_plan(echo_ast("g", "ROUND-ECHO")),
                    prose("done"),
                ])),
                seen: Mutex::new(Vec::new()),
                systems: Mutex::new(Vec::new()),
            });
            let store = Arc::new(FlowStore::in_memory().unwrap());
            let (executor, _host) = EngineLoopHost::install(
                executor,
                provider.clone(),
                "rec".into(),
                None,
                store.clone(),
                Arc::new(DynamicComposites::default()),
                "sess".into(),
                shared.clone(),
                CompileOptions::default(),
            );
            let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
            let mut outer = SharedSink::new(shared.clone());
            execute_flow(
                store.as_ref(),
                executor.as_ref(),
                "sess",
                &loop_ast,
                &mut outer,
            )
            .await
            .unwrap();

            assert_eq!(
                provider.seen.lock().unwrap().len(),
                2,
                "simple full-plan turn: exactly two provider calls, as today"
            );
            assert_eq!(
                executor.evidence().by_kind("turn.gather").count(),
                0,
                "the gather pass must not run when orient already emitted the full plan"
            );
            assert_eq!(
                rec.tools
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|t| t.as_str() == "echo")
                    .count(),
                1,
                "the echo op ran exactly once"
            );
        }
    }

    /// A-14 named acceptance: a `gather: true` plan sets `settled: ""`, routes through the bounded
    /// gather pass, its results bind as ordinary FlowStore symbols, and each round emits a
    /// `turn.gather` observation (design Part 1). Also proves phase threading reaches
    /// `compile_turn`: the `loop.phase` observations, emitted at `plan()` entry from the very
    /// value passed into `compile_turn`, land in order orient -> gather -> execute.
    #[tokio::test]
    async fn gather_pass_binds_symbols_and_observes() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir = std::env::temp_dir().join(format!("flux-loop-a14-gather-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        flux_tools::register_evidence(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![
                emit_gather_plan(
                    echo_ast("gathered", "GATHER-ECHO"),
                    "find the bug",
                    &["stack trace"],
                ),
                emit_plan(echo_ast("final", "FINAL-ECHO")),
                prose("done"),
            ])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );
        let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
        let mut outer = SharedSink::new(shared.clone());
        execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &loop_ast,
            &mut outer,
        )
        .await
        .unwrap();

        assert_eq!(
            provider.seen.lock().unwrap().len(),
            3,
            "orient(gather) + gather-phase(settle) + execute-phase(chat)"
        );
        assert_eq!(
            executor.evidence().by_kind("turn.gather").count(),
            1,
            "exactly one gather round ran"
        );
        let evidence = executor.evidence();
        let briefs: Vec<_> = evidence.by_kind("flow.brief").collect();
        assert_eq!(briefs.len(), 1, "the brief is observed once, on acceptance");
        assert_eq!(briefs[0].data["goal"], json!("find the bug"));
        assert_eq!(briefs[0].data["needs"], json!(["stack trace"]));

        // The gather round's result bound as an ordinary FlowStore symbol.
        let gathered = store
            .resolve("sess", &crate::ast::SymbolName("gathered".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap());
        assert_eq!(
            gathered,
            Some(crate::ast::Value::String("GATHER-ECHO".into()))
        );

        let phases: Vec<String> = executor
            .evidence()
            .by_kind("loop.phase")
            .filter_map(|o| {
                o.data
                    .get("phase")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .collect();
        assert_eq!(
            phases,
            vec!["orient", "gather", "execute"],
            "phase threads through compile_turn in order, one loop.phase observation per plan() call"
        );
    }

    /// A-14 named acceptance: once an orient/gather round's plan carries a `brief`, it is
    /// host-carried per turn and prepended to EVERY subsequent planner feedback message — not just
    /// the immediate next round, but the execute phase that follows too.
    #[tokio::test]
    async fn brief_prepended_to_followup_plan_calls() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir = std::env::temp_dir().join(format!("flux-loop-a14-brief-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        flux_tools::register_evidence(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![
                emit_gather_plan(
                    echo_ast("gathered", "GATHER-ECHO"),
                    "find the bug",
                    &["stack trace"],
                ),
                emit_plan(echo_ast("final", "FINAL-ECHO")),
                prose("done"),
            ])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );
        let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
        let mut outer = SharedSink::new(shared.clone());
        execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &loop_ast,
            &mut outer,
        )
        .await
        .unwrap();

        let seen = provider.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3);
        assert!(
            !seen[0].contains("[brief]"),
            "orient's own call carries no brief yet: {:?}",
            seen[0]
        );
        assert!(
            seen[1].contains("[brief] goal: find the bug") && seen[1].contains("stack trace"),
            "the gather-phase call must see the brief prepended to its feedback: {:?}",
            seen[1]
        );
        assert!(
            seen[2].contains("[brief] goal: find the bug"),
            "the execute-phase call must STILL see the brief -- carried for the rest of the turn: {:?}",
            seen[2]
        );
    }

    /// A-14 named acceptance: once the gather budget (`repeat 3`) is spent while the model still
    /// emitted a `gather: true` plan, that read-only plan is not discarded — it simply runs as the
    /// FIRST execute-loop iteration (design Part 1's graceful degradation), and the gather rounds
    /// stay bounded at 3 before execute-phase planning follows.
    #[tokio::test]
    async fn gather_budget_exhaustion_degrades_to_execute() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir = std::env::temp_dir().join(format!("flux-loop-a14-budget-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        flux_tools::register_evidence(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        // Orient + all three gather rounds keep emitting `gather: true` (never settling) — the
        // budget (repeat 3) exhausts with a leftover gather plan still on the table.
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![
                emit_gather_plan(echo_ast("g1", "GATHER-1"), "goal", &["a"]),
                emit_gather_plan(echo_ast("g2", "GATHER-2"), "goal", &["b"]),
                emit_gather_plan(echo_ast("g3", "GATHER-3"), "goal", &["c"]),
                emit_gather_plan(echo_ast("leftover", "LEFTOVER-GATHER"), "goal", &["d"]),
                prose("done"),
            ])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );
        let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
        let mut outer = SharedSink::new(shared.clone());
        let outcome = execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &loop_ast,
            &mut outer,
        )
        .await
        .unwrap();

        assert_eq!(
            provider.seen.lock().unwrap().len(),
            5,
            "orient + 3 gather rounds (the budget) + 1 execute-phase call"
        );
        assert_eq!(
            executor.evidence().by_kind("turn.gather").count(),
            3,
            "the gather budget bounds at 3 rounds"
        );
        assert_eq!(
            executor.evidence().by_kind("turn.iteration").count(),
            1,
            "the leftover gather plan ran as the first EXECUTE iteration, not inside the gather pass"
        );

        for name in ["g1", "g2", "g3", "leftover"] {
            let bound = store
                .resolve("sess", &crate::ast::SymbolName(name.into()))
                .unwrap();
            assert!(bound.is_some(), "${name} should be bound -- its plan ran");
        }
        assert!(outcome.result.contains("done"));
    }

    /// A-14 named acceptance: a phase-less `plan(feedback: ...)` call — the exact shape a pre-A-14
    /// ejected/overridden loop still sends — behaves as the `execute` phase, and its `PlanAttempt`
    /// is stamped `phase: "execute"`.
    #[tokio::test]
    async fn phase_less_plan_call_behaves_as_execute() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir =
            std::env::temp_dir().join(format!("flux-loop-a14-phaseless-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider: Arc<dyn Provider> = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::from(vec![prose("ok")])),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let sid = events.create_session("mock").unwrap();
        let (_executor, host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store,
            Arc::new(DynamicComposites::default()),
            sid.clone(),
            shared.clone(),
            CompileOptions::default(),
        );
        let turn_id = events.begin_turn(&sid, "hi", "mock").unwrap();
        host.set_turn(
            sid.clone(),
            None,
            shared,
            None,
            Some((events.clone(), turn_id)),
        );

        // No "phase" key at all -- the exact shape a phase-less caller sends.
        let out = host.plan(json!({ "feedback": "" })).await.unwrap();
        assert_eq!(out["kind"], "chat");
        assert_eq!(out["settled"], "true");

        let turns = events.turns(&sid).unwrap();
        let attempts = &turns[0].plan_attempts;
        assert_eq!(
            attempts.last().unwrap().phase.as_deref(),
            Some("execute"),
            "a phase-less plan() call must be recorded (and behave) as the execute phase"
        );
    }

    /// A-14 named acceptance: an already-ejected, pre-A-14 `.flux/agent-loop.flux` override — using
    /// the OLD phase-less `plan($feedback)` call, with no phased passes at all — still runs
    /// correctly through the SAME host: no flag, no fallback, just the phase-less contract
    /// behaving as `execute` end-to-end.
    #[tokio::test]
    async fn old_ejected_loop_text_still_runs() {
        const OLD_LOOP: &str = "
flow agent-loop -> string
  $answer = fmt(\"\")
  $feedback = fmt(\"\")
  $done = fmt(\"\")
  repeat 25
    until $done
    $plan = plan($feedback)
    $kind = $plan.kind
    match $kind
      case \"chat\"
        $answer = $plan.text
        $done = fmt(\"true\")
      case \"error\"
        $answer = $plan.text
        $done = fmt(\"true\")
      default
        $ran = run_plan($plan)
        $feedback = $ran.transcript
        do observe \"turn.iteration\", $ran
  return $answer
";
        let old_ast = flux_lang::parse::parse(OLD_LOOP).expect("the old loop text still parses");

        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir =
            std::env::temp_dir().join(format!("flux-loop-a14-old-ejected-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        flux_tools::register_reflect(&mut reg);
        flux_tools::register_evidence(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![
                emit_plan(echo_ast("g", "OLD-LOOP-ECHO")),
                prose("done"),
            ])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );
        let mut outer = SharedSink::new(shared.clone());
        let outcome = execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &old_ast,
            &mut outer,
        )
        .await
        .unwrap();

        assert_eq!(
            provider.seen.lock().unwrap().len(),
            2,
            "old loop: plan then plan again, exactly as before A-14"
        );
        assert!(outcome.result.contains("done"));
        let g = store
            .resolve("sess", &crate::ast::SymbolName("g".into()))
            .unwrap()
            .and_then(|id| store.get_value(&id).unwrap());
        assert_eq!(g, Some(crate::ast::Value::String("OLD-LOOP-ECHO".into())));
    }

    // ---------------------------------------------------------------------------
    // A-17 — revise wiring: the loop routes on `$ran.failure`; end-to-end patch-and-continue
    // ---------------------------------------------------------------------------

    /// A-17 named acceptance (`midplan_failure_revise_and_continue_completes_turn`): a full engine
    /// turn where the orient pass emits a plan that fails mid-way, the loop feeds the structured
    /// halt back (observing `turn.revision`, not `turn.iteration`), the mock provider re-emits a
    /// corrected plan that keeps the completed statement byte-identical, the runtime fast-forwards
    /// that completed prefix (never re-dispatching it — proven by `echo` running exactly twice, not
    /// three times), and the turn completes with the right answer — all within one turn, well under
    /// the loop's caps.
    #[tokio::test]
    async fn midplan_failure_revise_and_continue_completes_turn() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
        let dir = std::env::temp_dir().join(format!("flux-loop-a17-revise-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(BoomTool));
        flux_tools::register_reflect(&mut reg);
        flux_tools::register_evidence(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(
                &[
                    "echo".into(),
                    "boom".into(),
                    "plan".into(),
                    "run_plan".into(),
                    "observe".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        // Round 1 (orient): the full execution plan — statement 0 succeeds, statement 1 (`boom`)
        // fails mid-way. Round 2 (execute, revised): statement 0 kept BYTE-IDENTICAL (fast-forward
        // must skip it, not re-dispatch it) and statement 1 replaced with a working op. Round 3
        // (execute): the model answers in prose, ending the turn.
        let round1 = json!({"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"first"}]}},
            {"kind":"call","op":"boom","args":[]}
        ]});
        let round2 = json!({"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"first"}]}},
            {"kind":"bind","name":"b","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"fixed"}]}}
        ]});
        let provider = Arc::new(MockProvider {
            responses: Mutex::new(VecDeque::from(vec![
                emit_plan(round1),
                emit_plan(round2),
                prose("done"),
            ])),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (executor, _host) = EngineLoopHost::install(
            executor,
            provider,
            "mock".into(),
            None,
            store.clone(),
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );
        let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
        let mut outer = SharedSink::new(shared.clone());
        let outcome = execute_flow(
            store.as_ref(),
            executor.as_ref(),
            "sess",
            &loop_ast,
            &mut outer,
        )
        .await
        .unwrap();

        assert_eq!(
            outcome.result, "done",
            "the turn completes with the model's answer after the revise round"
        );

        // Fast-forward proof: `echo` dispatched exactly twice — once for statement 0 (round 1) and
        // once for the new statement `b` (round 2) — NEVER a third time for statement 0, which
        // would mean the runtime re-ran an already-completed statement instead of skipping it.
        let echoes = rec
            .tools
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.as_str() == "echo")
            .count();
        assert_eq!(
            echoes, 2,
            "statement 0 must be fast-forwarded (skipped), not re-dispatched, on the revise round"
        );
        let booms = rec
            .tools
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.as_str() == "boom")
            .count();
        assert_eq!(
            booms, 1,
            "boom only ever runs the one time it actually failed"
        );

        // The loop's own `do observe` calls (`turn.revision`/`turn.iteration`) go through the
        // ordinary `observe` op, so they land in the executor's shared evidence log — the same
        // place `turn.gather`/`turn.iteration` are already asserted elsewhere in this file (A-14).
        // The observation trail tells the true story: a `turn.revision` round (the halt) and a
        // `turn.iteration` round (the clean revise-and-continue run) — never the other way round.
        let evidence = executor.evidence();
        assert_eq!(
            evidence.by_kind("turn.revision").count(),
            1,
            "exactly one round carried a failure"
        );
        assert_eq!(
            evidence.by_kind("turn.iteration").count(),
            1,
            "exactly one round ran clean"
        );
        // `flow.halt` (host-side rendering plumbing, streamed to the sink like `flow.plan`) is
        // asserted through the test sink's own recorder instead.
        let obs = rec.observations.lock().unwrap();
        assert_eq!(
            obs.iter().filter(|o| o.kind == "flow.halt").count(),
            1,
            "the halt itself is surfaced once, in real time: {obs:?}"
        );
        let halt = obs.iter().find(|o| o.kind == "flow.halt").unwrap();
        assert_eq!(halt.data["step"], 2);
        assert_eq!(halt.data["of"], 2);
        assert_eq!(halt.data["op"], "boom");
    }

    /// A-17 named acceptance (`loop_routes_fatal_halt_distinctly_from_retryable`): the loop's
    /// `$ran.failure` routing is faithful for BOTH failure classes design Part 2 defines — a
    /// retryable `runtime` halt (`boom`) and a fatal one (`assert`, `is_fatal()` per
    /// `FailureKind`) — both still observe `turn.revision` (never silently absorbed as a plain
    /// iteration), and the `fatal`/`kind` distinction the host already computed survives the loop's
    /// pass-through into the observation the surface/audit trail reads.
    #[tokio::test]
    async fn loop_routes_fatal_halt_distinctly_from_retryable() {
        async fn run_one_halt(
            tag: &str,
            failing_stmt: Value,
            expect_kind: &str,
            expect_fatal: bool,
        ) {
            let rec = Recorder::default();
            let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec.clone())));
            let dir = std::env::temp_dir()
                .join(format!("flux-loop-a17-fatal-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
            let mut reg = ToolRegistry::new();
            reg.register(Arc::new(EchoTool));
            reg.register(Arc::new(BoomTool));
            flux_tools::register_reflect(&mut reg);
            flux_tools::register_evidence(&mut reg);
            let executor = Executor::new(
                reg,
                PermissionManager::from_rules(
                    &[
                        "echo".into(),
                        "boom".into(),
                        "plan".into(),
                        "run_plan".into(),
                        "observe".into(),
                    ],
                    &[],
                ),
                Arc::new(AllowApprover),
                ToolContext::new(system),
            );
            let round1 = json!({"body": [failing_stmt]});
            // The corrected round never re-emits the failed statement (the denial-guard/assert
            // guidance both say not to) — it just answers in prose, ending the turn.
            let provider = Arc::new(MockProvider {
                responses: Mutex::new(VecDeque::from(vec![
                    emit_plan(round1),
                    prose("acknowledged"),
                ])),
            });
            let store = Arc::new(FlowStore::in_memory().unwrap());
            let (executor, _host) = EngineLoopHost::install(
                executor,
                provider,
                "mock".into(),
                None,
                store.clone(),
                Arc::new(DynamicComposites::default()),
                "sess".into(),
                shared.clone(),
                CompileOptions::default(),
            );
            let loop_ast = crate::engine::load_agent_loop(&dir).unwrap();
            let mut outer = SharedSink::new(shared.clone());
            execute_flow(
                store.as_ref(),
                executor.as_ref(),
                "sess",
                &loop_ast,
                &mut outer,
            )
            .await
            .unwrap();

            // The loop's `do observe` calls land in the executor's shared evidence log (the
            // ordinary `observe` op), not the sink — see `midplan_failure_revise_and_continue_completes_turn`.
            let evidence = executor.evidence();
            assert_eq!(
                evidence.by_kind("turn.revision").count(),
                1,
                "[{tag}] a halt of either kind is a revision round, not a plain iteration"
            );
            assert_eq!(
                evidence.by_kind("turn.iteration").count(),
                0,
                "[{tag}] the turn ends in prose right after the halt — no clean round in between"
            );
            let revisions: Vec<_> = evidence.by_kind("turn.revision").collect();
            let revision = revisions.first().unwrap();
            // `do observe "turn.revision", $ran` passes `$ran` through as the JSON-as-string an op
            // result is stored as (the observed `data` is that raw string, not a nested object) — the
            // same convention `jq`/`bind` re-parse on the way in; parse it back out the same way here.
            let ran: Value = revision
                .data
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .expect("turn.revision carries $ran as JSON text");
            assert_eq!(
                ran["failure"]["kind"], expect_kind,
                "[{tag}] the loop's pass-through must not lose the failure kind: {ran:?}"
            );
            assert_eq!(
                ran["failure"]["fatal"], expect_fatal,
                "[{tag}] the loop's pass-through must not lose the fatal/retryable distinction: {ran:?}"
            );
        }

        // Retryable: a plain op error (`FailureKind::Runtime`, `is_fatal() == false`).
        run_one_halt(
            "retryable",
            json!({"kind":"call","op":"boom","args":[]}),
            "runtime",
            false,
        )
        .await;

        // Fatal: an `assert` whose condition is false (`FailureKind::AssertFailed`,
        // `is_fatal() == true`).
        run_one_halt(
            "fatal",
            json!({"kind":"assert","cond":{"kind":"lit","value":false},"message":"invariant broke"}),
            "assert_failed",
            true,
        )
        .await;
    }

    // ---------------------------------------------------------------------------
    // A-16 — loop-host resume policy + structured feedback contract
    // ---------------------------------------------------------------------------

    /// Always fails with a plain (non-denied) error — a `FailureKind::Runtime` halt.
    struct BoomTool;
    #[async_trait]
    impl Tool for BoomTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "boom",
                "always fails",
                json!({"type":"object","properties":{}}),
            )
        }
        async fn execute(&self, _c: &ToolContext, _p: Value) -> Result<ToolResult> {
            Ok(ToolResult::error("boom: kaboom"))
        }
    }

    /// A third read-only echo-like op (distinct from `echo`/`writeecho`) — used where a test needs
    /// an op name that is unambiguously NOT the one under a denial guard.
    struct NoteTool;
    #[async_trait]
    impl Tool for NoteTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "note",
                "note text",
                json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            )
        }
        async fn execute(&self, _c: &ToolContext, p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                p.get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// A second MUTATING echo-like tool, distinct op name from `writeecho` — the suffix-scoped
    /// approval test needs its OWN op name so a captured `PlanApprovalRequest.ops` can distinguish
    /// "the newly-added suffix statement" from "the already-completed prefix statement".
    struct WriteEcho2Tool;
    #[async_trait]
    impl Tool for WriteEcho2Tool {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(
                "writeecho2",
                "echo (mutating) #2",
                json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            )
        }
        fn intents(&self, _p: &Value) -> flux_spec::IntentSet {
            use flux_spec::{Intent, IntentBehavior, IntentCertainty, IntentRole, IntentTarget};
            let mut s = flux_spec::IntentSet::new();
            s.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path {
                    path: "out2.txt".into(),
                },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
            s
        }
        async fn execute(&self, _c: &ToolContext, p: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok(
                p.get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    /// Records every plan-level approval request's `ops` list — used to prove suffix-scoped
    /// approval (A-16): a resumed plan's approval preview must carry only the to-run suffix's ops.
    struct OpsCapturingApprover {
        seen: Arc<Mutex<Vec<Vec<String>>>>,
    }
    #[async_trait]
    impl flux_runtime::Approver for OpsCapturingApprover {
        async fn request(
            &self,
            _t: &str,
            _s: &[String],
            _i: &flux_spec::IntentSet,
        ) -> flux_runtime::ApprovalChoice {
            flux_runtime::ApprovalChoice::Allow
        }
        async fn request_plan(
            &self,
            plan: &flux_runtime::PlanApprovalRequest,
        ) -> flux_runtime::ApprovalChoice {
            self.seen.lock().unwrap().push(plan.ops.clone());
            flux_runtime::ApprovalChoice::Allow
        }
    }

    /// `run_plan_feeds_structured_halt_and_prefix_transcript` (A-16 acceptance): a halted `run_plan`
    /// returns `Ok` — never `Err` — with the prefix transcript, the ✓/✗-marked plan, and a
    /// machine-readable `failure` object the next planner round (or a flux-lang consumer) can route
    /// on.
    #[tokio::test]
    async fn run_plan_feeds_structured_halt_and_prefix_transcript() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["echo".into(), "boom".into()], &[]),
            Arc::new(AllowApprover),
            rec,
            "a16-halt",
        );

        let plan = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"first"}]}},
            {"kind":"call","op":"boom","args":[]}
        ]}});
        let out = host.run_plan(plan).await.unwrap();

        assert_eq!(
            out["steps"], 2,
            "both statements were dispatched — the second one failed, it wasn't skipped: {out}"
        );
        let failure = &out["failure"];
        assert!(
            !failure.is_null(),
            "a halt must be reified, not an Err: {out}"
        );
        assert_eq!(failure["node"], 1);
        assert_eq!(failure["kind"], "runtime");
        assert_eq!(failure["fatal"], false);
        assert_eq!(failure["op"], "boom");
        assert!(failure["message"].as_str().unwrap().contains("boom"));
        let completed = failure["completed"].as_array().unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0]["node"], 0);
        assert_eq!(completed[0]["bind"], "a");
        assert_eq!(completed[0]["op"], "echo");

        let transcript = out["transcript"].as_str().unwrap();
        assert!(
            transcript.contains("[plan halted at step 2 of 2]"),
            "{transcript}"
        );
        assert!(transcript.contains("✓ 0:"), "{transcript}");
        assert!(transcript.contains("✗ 1:"), "{transcript}");
    }

    /// A-17: `run_plan`'s thin normalizing wrapper always sets `failure` — `null` when this round
    /// didn't halt, never a missing key. `flux-lang`'s dotted field-access sugar (`$ran.failure`
    /// lowers to a `jq` lookup) runtime-errors on a genuinely MISSING key, and the loop now routes
    /// on `$ran.failure` every execute round — so a bare-absent key would break every clean turn.
    #[tokio::test]
    async fn run_plan_failure_field_is_always_present_never_missing() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["echo".into()], &[]),
            Arc::new(AllowApprover),
            rec,
            "a17-failure-present",
        );
        let plan = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"hi"}]}}
        ]}});
        let out = host.run_plan(plan).await.unwrap();
        assert_eq!(
            out.get("failure"),
            Some(&Value::Null),
            "success must set failure: null, never omit the key: {out}"
        );
    }

    /// A-17 (closes the A-15 residual + wires the ✓-marker rendering fix): `run_plan`'s `flow.plan`
    /// observation now carries `gather`/`phase`/`resumed` directly, computed host-side — the CLI/TUI
    /// no longer need to infer gather-mode from a `loop.phase`/`flow.brief` state machine (which
    /// couldn't tell an orient-phase gather plan apart from orient emitting the full plan directly
    /// when the model's `brief` was unusable — A-15's recorded residual), nor guess whether `plan`'s
    /// text carries ✓/✗/· status markers instead of a fresh full tree.
    #[tokio::test]
    async fn flow_plan_observation_carries_gather_phase_and_resumed_fields() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["echo".into(), "boom".into()], &[]),
            Arc::new(AllowApprover),
            rec.clone(),
            "a17-plan-fields",
        );

        // A gather-phase call (the wire `settled: ""` an accepted `gather: true` plan carries) —
        // `gather` reads directly off the incoming plan, not the surface's own phase tracking.
        let gather_plan = json!({
            "kind": "plan", "settled": "",
            "ast": {"body": [
                {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"g"}]}}
            ]},
        });
        host.run_plan(gather_plan).await.unwrap();
        {
            let obs = rec.observations.lock().unwrap();
            let plan_obs = obs
                .iter()
                .find(|o| o.kind == "flow.plan")
                .expect("flow.plan observed");
            assert_eq!(plan_obs.data["gather"], true, "{plan_obs:?}");
            assert_eq!(plan_obs.data["resumed"], false, "{plan_obs:?}");
        }
        rec.observations.lock().unwrap().clear();

        // A halt, then its resume: `resumed` is false on the fresh attempt and flips true only on
        // the corrected re-emission that actually consumes the open latch.
        let halted = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"keep"}]}},
            {"kind":"call","op":"boom","args":[]}
        ]}});
        host.run_plan(halted).await.unwrap();
        {
            let obs = rec.observations.lock().unwrap();
            let plan_obs = obs
                .iter()
                .find(|o| o.kind == "flow.plan")
                .expect("flow.plan observed");
            assert_eq!(plan_obs.data["gather"], false);
            assert_eq!(
                plan_obs.data["resumed"], false,
                "the FIRST attempt is not itself a resume"
            );
        }
        rec.observations.lock().unwrap().clear();

        let fixed = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"keep"}]}},
            {"kind":"bind","name":"b","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"fixed"}]}}
        ]}});
        host.run_plan(fixed).await.unwrap();
        let obs = rec.observations.lock().unwrap();
        let plan_obs = obs
            .iter()
            .find(|o| o.kind == "flow.plan")
            .expect("flow.plan observed");
        assert_eq!(
            plan_obs.data["resumed"], true,
            "the corrected re-emission IS a resume: {plan_obs:?}"
        );
    }

    /// A-17: a mid-plan halt emits a discrete `flow.halt` observation (`{step, of, op, kind,
    /// fatal}`) right where the rest of the feedback contract is built, so the surface can print a
    /// real-time `✗ step N/M <op> failed — revising…` cue instead of the halt being legible only
    /// inside the fed-back transcript text.
    #[tokio::test]
    async fn flow_halt_observation_fires_with_step_of_op_and_kind() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["echo".into(), "boom".into()], &[]),
            Arc::new(AllowApprover),
            rec.clone(),
            "a17-flow-halt",
        );
        let plan = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"first"}]}},
            {"kind":"call","op":"boom","args":[]}
        ]}});
        host.run_plan(plan).await.unwrap();
        let obs = rec.observations.lock().unwrap();
        let halt_obs = obs
            .iter()
            .find(|o| o.kind == "flow.halt")
            .expect("flow.halt observed");
        assert_eq!(halt_obs.data["step"], 2);
        assert_eq!(halt_obs.data["of"], 2);
        assert_eq!(halt_obs.data["op"], "boom");
        assert_eq!(halt_obs.data["kind"], "runtime");
        assert_eq!(halt_obs.data["fatal"], false);
    }

    /// `second_run_plan_consumes_latch_and_skips_completed_prefix` (A-16 acceptance): a corrected
    /// re-emission that keeps the completed statement byte-identical fast-forwards past it —
    /// the runtime never re-dispatches it — and runs only the fixed suffix.
    #[tokio::test]
    async fn second_run_plan_consumes_latch_and_skips_completed_prefix() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["echo".into(), "boom".into()], &[]),
            Arc::new(AllowApprover),
            rec.clone(),
            "a16-resume",
        );

        let halted = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"keep"}]}},
            {"kind":"call","op":"boom","args":[]}
        ]}});
        let first = host.run_plan(halted).await.unwrap();
        assert!(!first["failure"].is_null());

        // The corrected re-emission: statement 0 byte-identical, statement 1 fixed.
        let fixed = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"keep"}]}},
            {"kind":"bind","name":"b","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"fixed"}]}}
        ]}});
        let second = host.run_plan(fixed).await.unwrap();
        assert!(
            second.get("failure").map(|f| f.is_null()).unwrap_or(true),
            "the corrected plan must succeed: {second}"
        );
        assert_eq!(
            second["steps"], 1,
            "only the corrected (new) statement dispatched — statement 0 was skipped: {second}"
        );

        let echoes = rec
            .tools
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.as_str() == "echo")
            .count();
        assert_eq!(
            echoes, 2,
            "echo dispatched once for statement 0 (round 1) and once for statement 1 (round 2) — \
             never twice for statement 0"
        );
    }

    /// `unrelated_next_plan_consumes_latch_with_zero_skips` (A-16 acceptance): the halt latch is
    /// one-shot — even a completely unrelated next plan (no matching prefix) consumes it.
    #[tokio::test]
    async fn unrelated_next_plan_consumes_latch_with_zero_skips() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["echo".into(), "boom".into()], &[]),
            Arc::new(AllowApprover),
            rec,
            "a16-zero-skip",
        );

        let halted = json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"boom","args":[]}
        ]}});
        let first = host.run_plan(halted).await.unwrap();
        assert!(!first["failure"].is_null());
        assert!(
            host.store.open_halted_plan("sess").unwrap().is_some(),
            "the latch is open after the halt"
        );

        // A totally unrelated plan — no statement in common with the halted one.
        let unrelated = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"c","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"totally different"}]}}
        ]}});
        let second = host.run_plan(unrelated).await.unwrap();
        assert!(second.get("failure").map(|f| f.is_null()).unwrap_or(true));
        assert_eq!(
            second["steps"], 1,
            "the unrelated plan ran in full — nothing matched to skip"
        );

        assert!(
            host.store.open_halted_plan("sess").unwrap().is_none(),
            "the latch must be consumed (PlanResumed appended) even at zero skips"
        );
    }

    /// A divergence BEFORE the previously-failed index must warn that the changed prefix will
    /// RE-RUN (including its side effects) — the model must not assume it stays skipped.
    #[tokio::test]
    async fn divergence_before_failure_produces_rerun_warning() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["echo".into(), "boom".into()], &[]),
            Arc::new(AllowApprover),
            rec,
            "a16-rerun-warn",
        );

        let halted = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"A"}]}},
            {"kind":"bind","name":"b","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"B"}]}},
            {"kind":"call","op":"boom","args":[]}
        ]}});
        let first = host.run_plan(halted).await.unwrap();
        assert!(!first["failure"].is_null());

        // Statement 0 CHANGED (diverges before the failed index 2); statement 1 unchanged;
        // statement 2 fixed.
        let diverged = json!({"kind":"plan","ast":{"body":[
            {"kind":"bind","name":"a","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"A-changed"}]}},
            {"kind":"bind","name":"b","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"B"}]}},
            {"kind":"bind","name":"c","value":{"kind":"call","op":"echo","args":[{"kind":"lit","value":"fixed"}]}}
        ]}});
        let second = host.run_plan(diverged).await.unwrap();
        let transcript = second["transcript"].as_str().unwrap();
        assert!(
            transcript.contains("will RE-RUN"),
            "a plan that diverges before the failed index must warn: {transcript}"
        );
        assert_eq!(
            second["steps"], 3,
            "nothing matched from index 0, so all three statements ran again: {second}"
        );
    }

    /// Suffix-scoped approval (A-16 acceptance): resuming past an already-completed mutating
    /// statement must NOT re-prompt the user for it — the plan-level approval preview covers only
    /// the suffix that will actually run.
    #[tokio::test]
    async fn suffix_scoped_approval_sees_only_the_to_run_suffix() {
        let rec = Recorder::default();
        let seen_ops: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let approver = Arc::new(OpsCapturingApprover {
            seen: seen_ops.clone(),
        });
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteEchoTool));
        reg.register(Arc::new(WriteEcho2Tool));
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::new(),
            approver,
            rec,
            "a16-suffix-approval",
        );

        // Round 1: `writeecho` succeeds, then `boom` fails — halts at index 1.
        let halted = json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"writeecho","args":[{"kind":"lit","value":"hi"}]},
            {"kind":"call","op":"boom","args":[]}
        ]}});
        let first = host.run_plan(halted).await.unwrap();
        assert!(!first["failure"].is_null());
        seen_ops.lock().unwrap().clear(); // only round 2's approval request is under test

        // Round 2: statement 0 byte-identical (will be skipped); statement 1 replaced by a NEW
        // mutating op. The approval preview must see ONLY the suffix that will actually run.
        let fixed = json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"writeecho","args":[{"kind":"lit","value":"hi"}]},
            {"kind":"call","op":"writeecho2","args":[{"kind":"lit","value":"bye"}]}
        ]}});
        let second = host.run_plan(fixed).await.unwrap();
        assert!(
            second.get("failure").map(|f| f.is_null()).unwrap_or(true),
            "{second}"
        );

        let calls = seen_ops.lock().unwrap();
        assert_eq!(calls.len(), 1, "one plan-level approval prompt for round 2");
        assert!(
            calls[0].contains(&"writeecho2".to_string()),
            "the to-run suffix's op must be in the approval preview: {:?}",
            calls[0]
        );
        assert!(
            !calls[0].contains(&"writeecho".to_string()),
            "an already-completed op must NOT be re-surfaced for approval: {:?}",
            calls[0]
        );
    }

    /// `denied_statement_reemission_is_refused_not_redispatched` (story acceptance): a byte-identical
    /// re-emission of a plan whose statement was denied is refused WITHOUT executing — the denied op
    /// is attempted exactly once, ever.
    #[tokio::test]
    async fn denied_statement_reemission_is_refused_not_redispatched() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&[], &["echo".into()]), // echo denied by permission rules
            Arc::new(AllowApprover),
            rec.clone(),
            "a16-denied-guard",
        );

        let plan = json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"echo","args":[{"kind":"lit","value":"hi"}]}
        ]}});

        let first = host.run_plan(plan.clone()).await.unwrap();
        assert_eq!(
            first["steps"], 1,
            "the denied op was ATTEMPTED once: {first}"
        );
        assert_eq!(first["failure"]["kind"], "denied");
        assert_eq!(first["failure"]["fatal"], true);

        let second = host.run_plan(plan).await.unwrap();
        assert_eq!(
            second["steps"], 0,
            "the byte-identical denied statement must NOT be re-dispatched: {second}"
        );
        assert_eq!(second["failure"]["kind"], "denied");
        let transcript = second["transcript"].as_str().unwrap();
        assert!(transcript.contains("NOT re-run"), "{transcript}");

        let echoes = rec
            .tools
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.as_str() == "echo")
            .count();
        assert_eq!(echoes, 1, "echo dispatched exactly once across both rounds");
    }

    /// `policy_denied_statement_is_not_reattempted_via_resume` (design acceptance): the guard matches
    /// by STATEMENT hash, not whole-plan fingerprint — a plan edited elsewhere that still carries the
    /// exact denied statement is refused in full, before anything (including a brand-new, otherwise
    /// harmless leading statement) runs.
    #[tokio::test]
    async fn policy_denied_statement_is_not_reattempted_via_resume() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(NoteTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["note".into()], &["echo".into()]),
            Arc::new(AllowApprover),
            rec.clone(),
            "a16-denied-resume",
        );

        let denied_plan = json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"echo","args":[{"kind":"lit","value":"hi"}]}
        ]}});
        let first = host.run_plan(denied_plan).await.unwrap();
        assert_eq!(first["failure"]["kind"], "denied");

        // A DIFFERENT plan (a new leading statement) that still contains the EXACT SAME denied
        // statement — proves the guard matches by statement hash, not whole-plan fingerprint.
        let edited_plan = json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"note","args":[{"kind":"lit","value":"context"}]},
            {"kind":"call","op":"echo","args":[{"kind":"lit","value":"hi"}]}
        ]}});
        let second = host.run_plan(edited_plan).await.unwrap();
        assert_eq!(
            second["steps"], 0,
            "the whole re-emitted plan is refused — not just the one statement: {second}"
        );
        assert_eq!(second["failure"]["kind"], "denied");

        assert!(
            !rec.tools.lock().unwrap().contains(&"note".to_string()),
            "the guard fires before ANY statement in the re-emitted plan runs"
        );
    }

    /// Repeated identical halts escalate through the SAME retry-breaker thresholds as any other
    /// stall, keyed by the structured `halt:{op}:{stmt}:{kind}` identity rather than transcript text.
    #[tokio::test]
    async fn guard_key_escalation_on_repeated_identical_halts() {
        let rec = Recorder::default();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(BoomTool));
        let (_ex, host) = setup_host(
            reg,
            PermissionManager::from_rules(&["boom".into()], &[]),
            Arc::new(AllowApprover),
            rec,
            "a16-guard-escalate",
        );
        let plan = json!({"kind":"plan","ast":{"body":[
            {"kind":"call","op":"boom","args":[]}
        ]}});

        let r1 = host.run_plan(plan.clone()).await.unwrap();
        assert!(
            !r1["transcript"].as_str().unwrap().contains("[loop-guard]"),
            "first sighting is quiet: {r1}"
        );

        let r2 = host.run_plan(plan.clone()).await.unwrap();
        let t2 = r2["transcript"].as_str().unwrap();
        assert!(t2.contains("[loop-guard]"), "{t2}");
        assert!(
            t2.contains("halt:boom:"),
            "the structured halt key must drive escalation: {t2}"
        );
        assert!(host.guard.lock().unwrap().force_stop.is_none());

        let _r3 = host.run_plan(plan.clone()).await.unwrap();
        assert!(host.guard.lock().unwrap().force_stop.is_none());

        let r4 = host.run_plan(plan).await.unwrap();
        assert!(r4["transcript"].as_str().unwrap().contains("STOP"), "{r4}");
        assert!(host.guard.lock().unwrap().force_stop.is_some());
    }

    /// Cross-turn resume (A-16 acceptance): a fresh turn's FIRST `plan()` call injects one ephemeral
    /// `[resume context]` message when the session has an open halt latch; a later round in the SAME
    /// turn must not repeat it.
    #[tokio::test]
    async fn fresh_turn_resume_context_is_injected_once() {
        let rec = Recorder::default();
        let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec)));
        let dir =
            std::env::temp_dir().join(format!("flux-loop-a16-resumectx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(BoomTool));
        flux_tools::register_reflect(&mut reg);
        let executor = Executor::new(
            reg,
            PermissionManager::from_rules(&["boom".into(), "plan".into(), "run_plan".into()], &[]),
            Arc::new(AllowApprover),
            ToolContext::new(system),
        );
        let provider = Arc::new(RecordingProvider {
            responses: Mutex::new(VecDeque::from(vec![prose("ok1"), prose("ok2")])),
            seen: Mutex::new(Vec::new()),
            systems: Mutex::new(Vec::new()),
        });
        let store = Arc::new(FlowStore::in_memory().unwrap());
        let (_executor, host) = EngineLoopHost::install(
            executor,
            provider.clone(),
            "rec".into(),
            None,
            store,
            Arc::new(DynamicComposites::default()),
            "sess".into(),
            shared.clone(),
            CompileOptions::default(),
        );

        // Halt a plan in "sess" — leaves the latch open.
        let plan = json!({"kind":"plan","ast":{"body":[{"kind":"call","op":"boom","args":[]}]}});
        let halted = host.run_plan(plan).await.unwrap();
        assert!(!halted["failure"].is_null());

        // A fresh turn boundary.
        host.set_turn("sess".into(), None, shared.clone(), None, None);
        let _ = host
            .plan(json!({"feedback": "", "phase": "orient"}))
            .await
            .unwrap();
        {
            let seen = provider.seen.lock().unwrap();
            assert!(
                seen[0].contains("[resume context]"),
                "the first plan() call of a fresh turn must inject the resume context: {}",
                seen[0]
            );
        }

        // The SAME turn's next round must NOT repeat it.
        let _ = host
            .plan(json!({"feedback": "next round", "phase": "execute"}))
            .await
            .unwrap();
        let seen = provider.seen.lock().unwrap();
        assert!(
            !seen[1].contains("[resume context]"),
            "must not repeat within the same turn: {}",
            seen[1]
        );
    }

    /// The halt latch is folded from the run-event log, not a new in-process table — a completely
    /// FRESH `FlowStore` opened over the same `events.db` (a stand-in for a fresh process after a
    /// crash/restart) must see the same open latch.
    #[tokio::test]
    async fn latch_survives_a_fresh_flowstore_over_the_same_events_db() {
        let dir =
            std::env::temp_dir().join(format!("flux-loop-a16-crossproc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let events_path = dir.join("events.db");
        let flow_path = dir.join("flow.db");

        // "Process one": halt a plan, then drop every handle.
        {
            let rec = Recorder::default();
            let shared: Arc<Mutex<dyn AgentSink>> = Arc::new(Mutex::new(RecSink(rec)));
            let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
            let mut reg = ToolRegistry::new();
            reg.register(Arc::new(BoomTool));
            let executor = Executor::new(
                reg,
                PermissionManager::from_rules(&["boom".into()], &[]),
                Arc::new(AllowApprover),
                ToolContext::new(system),
            );
            let provider: Arc<dyn Provider> = Arc::new(MockProvider {
                responses: Mutex::new(VecDeque::new()),
            });
            let events = Arc::new(flux_events::EventStore::open(&events_path).unwrap());
            let store = Arc::new(FlowStore::open(&flow_path, events).unwrap());
            let (_ex, host) = EngineLoopHost::install(
                executor,
                provider,
                "mock".into(),
                None,
                store,
                Arc::new(DynamicComposites::default()),
                "sess".into(),
                shared,
                CompileOptions::default(),
            );
            let plan =
                json!({"kind":"plan","ast":{"body":[{"kind":"call","op":"boom","args":[]}]}});
            let out = host.run_plan(plan).await.unwrap();
            assert!(!out["failure"].is_null());
        }

        // "Process two": a completely fresh `EventStore` + `FlowStore` over the SAME paths.
        let events2 = Arc::new(flux_events::EventStore::open(&events_path).unwrap());
        let store2 = FlowStore::open(&flow_path, events2).unwrap();
        let open = store2
            .open_halted_plan("sess")
            .unwrap()
            .expect("a fresh store over the same events.db must see the open halt latch");
        assert_eq!(open.halt.op.as_deref(), Some("boom"));

        std::fs::remove_dir_all(&dir).ok();
    }
}

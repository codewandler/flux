//! D-178: **Resurrect** — transparently finish a turn killed mid-execution (OOM, redeploy, a plain
//! crash) from the exact crash point. No model re-spend: the accepted plan is durable source
//! (`PlanAttempted.plan_source`, L-38), so resurrecting never calls a provider. No duplicate side
//! effects for any op with a recorded cassette cell — served exactly once from the crash-tail
//! [`CassetteScope::Resume`] scope (D-175). A loud stop, never a silent close, if the world diverged
//! from the recording.
//!
//! Modeled on [`crate::replay`]/[`crate::fork`]: a thin driver over the shipped
//! [`crate::runtime::execute_flow_resumable_with_composites`], run **on the same session** (unlike
//! replay's scratch store or fork's new session) so the finished turn is indistinguishable from one
//! that never crashed.
//!
//! # Honest exactly-once
//!
//! Every op with a recorded [`RunEvent::OpRecorded`] cell is served **exactly once** — the cell is
//! consumed like any [`crate::cassette::ReplayTape`] match, so the live executor is never touched for
//! it. But an op that fired its side effect and then died **before** its cell could be appended has no
//! cell to serve: it re-fires live on resume. That one op is **at-least-once**, not exactly-once —
//! mirroring Temporal's activity semantics exactly. This is a property of *when* the process died, not
//! a bug: there is no way to know an op fired without a durable record that it did.
//!
//! # The crash-tail slice
//!
//! The [`ResumeTape`] must only ever be offered cells recorded strictly AFTER the last completed
//! top-level statement — cells belonging to already-completed statements are stale: their values are
//! rehydrated through the ledger (`ResumeLedger::from_interrupted`), and serving their op cells again
//! to a *different* (fast-forwarded, not re-executed) statement would risk a false match. See
//! [`crash_tail_cells`].

use std::sync::Arc;

use flux_core::Message;
use flux_events::EventStore;
use flux_lang::ast::{DraftAst, RunEvent};
use flux_lang::program::CompositeOpDecl;
use flux_lang::runtime::{flow_key, PlanHalt, ResumeLedger};
use flux_runtime::Executor;

use crate::agent_sink::AgentSink;
use crate::cassette::{CassetteScope, Cell, RecordScope, ResumeTape};
use crate::engine::suspension_prompt;
use crate::state::FlowStore;
use crate::{FlowError, Result};

/// A turn found open (`TurnStarted` with no `TurnEnded`) by [`interrupted`] — everything
/// [`resurrect`] needs to finish it without re-parsing or re-deriving anything twice.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InterruptedTurn {
    pub turn_id: i64,
    pub started_at_ms: i64,
    pub user_input: String,
    /// The turn's last ACCEPTED plan (`plan_source`, parsed) — the durable source `resurrect` re-runs.
    /// A crash during planning (no accepted plan at all) never reaches here — [`interrupted`] returns
    /// a loud `Err` for that instead.
    pub plan: DraftAst,
    /// [`flow_key`] of `plan` — the identity [`ResumeLedger::from_interrupted`] and the crash-tail
    /// slice are both keyed on.
    pub flow_key: String,
    /// How many of `plan`'s top-level statements were already completed before the crash (0 when the
    /// crash happened before even the first statement finished).
    pub completed: usize,
}

/// What [`resurrect`] reports once the open turn is finished (or re-halted, or re-suspended).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResurrectReport {
    pub session: String,
    pub turn_id: i64,
    /// Top-level statements fast-forwarded without re-dispatch (`RunEvent::PlanResumed.skipped`).
    pub statements_fast_forwarded: usize,
    /// Ops served exactly-once from the crash-tail cassette (never touched the live executor).
    pub ops_served_from_cassette: usize,
    /// Ops that fell through and ran live — the honest at-least-once window, plus every op with no
    /// recorded cell at all (a from-scratch resume with an empty tail).
    pub ops_run_live: usize,
    /// `"resurrected"` (finished clean), `"halted"` (a statement failed on resume), or `"suspended"`
    /// (the flow hit another top-level `await`).
    pub outcome: String,
    pub answer: String,
    /// The reified failure, when `outcome == "halted"`.
    pub halt: Option<PlanHalt>,
    /// A latched divergence — a re-derived `input_hash` mismatch on a served cell, or unconsumed
    /// crash-tail cells left over after the run — surfaced loudly, never silently. `None` on a clean
    /// resurrection.
    pub diverged: Option<String>,
}

fn crash_err(session: &str, msg: impl std::fmt::Display) -> FlowError {
    FlowError::Runtime(format!("session {session} cannot be resurrected: {msg}"))
}

/// The crash-tail cell slice: [`RunEvent::OpRecorded`] cells strictly AFTER the last
/// `StatementCompleted`/`PlanHalted` row in the trace. No statement row at all (the crash happened
/// before the first top-level statement completed) yields an EMPTY tail — conservative by design:
/// every op in that window falls into the documented live at-least-once path rather than risking a
/// stale match against a statement that never actually finished.
fn crash_tail_cells(trace: &[RunEvent]) -> Vec<Cell> {
    let boundary = trace.iter().enumerate().rev().find_map(|(i, ev)| {
        matches!(
            ev,
            RunEvent::StatementCompleted { .. } | RunEvent::PlanHalted { .. }
        )
        .then_some(i + 1)
    });
    match boundary {
        Some(b) => Cell::collect(&trace[b..]),
        None => Vec::new(),
    }
}

/// Detect an interrupted turn: the last [`flux_events::TurnSummary`] with `ended_at_ms == None`. `Ok(None)`
/// means the session's last turn closed cleanly — nothing to resurrect. A crash during PLANNING (the
/// open turn has no accepted plan attempt, or its `plan_source` didn't survive) is loudly `Err` rather
/// than silently reported as "nothing to resume" — there is no durable plan to resume from, and the
/// caller must know that rather than assume the turn quietly vanished.
pub fn interrupted(events: &EventStore, session: &str) -> Result<Option<InterruptedTurn>> {
    let turns = events.turns(session)?;
    let Some(turn) = turns.iter().rev().find(|t| t.ended_at_ms.is_none()) else {
        return Ok(None);
    };
    let Some(accepted) = turn
        .plan_attempts
        .iter()
        .rev()
        .find(|a| a.outcome == "accepted")
    else {
        return Err(crash_err(
            session,
            format!(
                "turn {} crashed before any plan was accepted — the crash happened during planning, \
                 before a durable plan existed to resume from",
                turn.turn_id
            ),
        ));
    };
    let Some(src) = accepted.plan_source.as_deref() else {
        return Err(crash_err(
            session,
            format!(
                "turn {} has an accepted plan attempt but no stored plan_source (pre-L-38 log, or an \
                 oversized plan) — cannot resurrect without a durable plan",
                turn.turn_id
            ),
        ));
    };
    let plan = flux_lang::parse::parse(src)
        .map_err(|e| crash_err(session, format!("stored plan_source no longer parses: {e}")))?;
    let key = flow_key(plan.name.as_deref(), &plan.body);
    let trace = events.run_trace(session)?;
    let completed = ResumeLedger::from_interrupted(&trace, &key)
        .map(|l| l.completed.len())
        .unwrap_or(0);
    Ok(Some(InterruptedTurn {
        turn_id: turn.turn_id,
        started_at_ms: turn.started_at_ms,
        user_input: turn.user_input.clone(),
        plan,
        flow_key: key,
        completed,
    }))
}

/// Resets a [`FlowStore`]'s cassette scope back to `None` on drop — guarantees the scope installed by
/// [`resurrect`] never leaks into a later turn, on EVERY return path (including an early `?` on error).
struct ResetCassette<'a>(&'a FlowStore);

impl Drop for ResetCassette<'_> {
    fn drop(&mut self) {
        self.0.set_cassette(None);
    }
}

/// Finish an interrupted turn in place, on the SAME session: install a [`CassetteScope::Resume`] over
/// the crash-tail cell slice, fast-forward the completed prefix via
/// [`ResumeLedger::from_interrupted`], and run the durable plan through
/// [`crate::runtime::execute_flow_resumable_with_composites`] — the exact driver `replay`/`fork` use,
/// so authorization, approval, and the C-43 recording envelope are all unchanged. `Ok(None)` when
/// there is nothing to resurrect (mirrors [`interrupted`]).
pub async fn resurrect(
    events: &EventStore,
    store: &FlowStore,
    executor: &Executor,
    session: &str,
    composites: &[CompositeOpDecl],
    sink: &mut dyn AgentSink,
) -> Result<Option<ResurrectReport>> {
    let Some(it) = interrupted(events, session)? else {
        return Ok(None);
    };

    let trace = events.run_trace(session)?;
    let ledger = ResumeLedger::from_interrupted(&trace, &it.flow_key);
    let tail_cells = crash_tail_cells(&trace);
    let tail = RecordScope::new(store.event_store(), session);
    let scope = Arc::new(CassetteScope::Resume(ResumeTape::new(tail_cells, tail)));
    store.set_cassette(Some(scope.clone()));
    let _guard = ResetCassette(store);

    let outcome = crate::runtime::execute_flow_resumable_with_composites(
        store,
        executor,
        session,
        &it.plan,
        composites,
        ledger.as_ref(),
        sink,
    )
    .await?;

    let CassetteScope::Resume(resume) = scope.as_ref() else {
        unreachable!("resurrect always installs CassetteScope::Resume")
    };
    let ops_served_from_cassette = resume.served();
    let ops_run_live = resume.ran_live();
    let remaining = resume.remaining();
    // A latched matcher divergence (a truncated served cell) wins; otherwise, unconsumed crash-tail
    // cells are themselves a loud divergence signal — every recorded cell in the tail was expected to
    // be re-consumed by the matching dispatch, so leftovers mean the resumed run's dataflow no longer
    // reaches somewhere the crashed run did.
    let diverged = resume.diverged().or_else(|| {
        (remaining > 0).then(|| {
            format!(
                "{remaining} crash-tail cell(s) recorded before the crash were never re-consumed on \
                 resume — the resumed run's dataflow diverged from the crashed one"
            )
        })
    });

    let statements_fast_forwarded = if ledger.is_some() {
        events
            .run_trace(session)?
            .iter()
            .rev()
            .find_map(|ev| match ev {
                RunEvent::PlanResumed {
                    plan,
                    prior,
                    skipped,
                } if plan == &it.flow_key && prior == &it.flow_key => Some(*skipped),
                _ => None,
            })
            .unwrap_or(0)
    } else {
        0
    };

    let (outcome_str, answer, halt) = if let Some(susp) = &outcome.suspension {
        store.save_suspension(
            session,
            it.plan.name.as_deref(),
            &it.plan.body,
            susp.node,
            &susp.source,
        )?;
        ("suspended", suspension_prompt(&outcome), None)
    } else if let Some(h) = &outcome.failure {
        (
            "halted",
            format!("The resumed flow halted — {}", h.message),
            Some(h.clone()),
        )
    } else {
        ("resurrected", outcome.result.trim().to_string(), None)
    };

    // Mirrors `FlowEngine::finish_turn_lifecycle`'s ordering: close the turn on the durable log first
    // (`TurnEnded`), then the single assistant message, then tell the sink. No model call was made, so
    // there is no usage to attribute.
    store.event_store().end_turn(
        session,
        it.turn_id,
        outcome_str,
        outcome.steps as u32,
        &answer,
        None,
    )?;
    store
        .event_store()
        .record_message(session, &Message::assistant_text(answer.clone()))?;
    sink.turn_end(None);

    Ok(Some(ResurrectReport {
        session: session.to_string(),
        turn_id: it.turn_id,
        statements_fast_forwarded,
        ops_served_from_cassette,
        ops_run_live,
        outcome: outcome_str.to_string(),
        answer,
        halt,
        diverged,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    use flux_events::PlanAttempt;
    use flux_lang::ast::{Node, NodeId, StepId, SymbolName, Value as FlowValue};
    use flux_lang::runtime::{sha256_hex, stmt_hash16};
    use flux_runtime::{
        AllowApprover, ApprovalChoice, Approver, PermissionManager, Tool, ToolContext,
        ToolRegistry, ToolResult,
    };
    use flux_spec::{IntentSet, ToolSpec};
    use flux_system::{System, Workspace};

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// A no-op sink — every [`AgentSink`] method defaults, so this only exists to have a concrete
    /// `&mut dyn AgentSink` to pass.
    #[derive(Default)]
    struct NullSink;
    impl AgentSink for NullSink {}

    /// A tool that counts real executions — proves a served (crash-tail) dispatch never touches the
    /// live executor, and a genuinely unrecorded one runs exactly once.
    struct CountingTool(Arc<AtomicUsize>);

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
            let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(ToolResult::ok(format!("live-{n}")))
        }
    }

    /// An approver that counts every consultation and always returns `choice`.
    struct ScriptedApprover {
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
            self.choice.clone()
        }
    }

    fn counting_tool_executor(approver: Arc<dyn Approver>, calls: Arc<AtomicUsize>) -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-resurrect-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
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

    fn call_counted() -> Node {
        Node::Call {
            op: "counted".into(),
            args: vec![],
        }
    }

    fn bind_counted(name: &str) -> Node {
        Node::Bind {
            name: SymbolName(name.into()),
            value: Box::new(call_counted()),
            ty: None,
            effect: None,
        }
    }

    fn ret(name: &str) -> Node {
        Node::Return {
            value: Box::new(Node::Var {
                name: SymbolName(name.into()),
            }),
        }
    }

    /// `$s0 = counted(); $s1 = counted(); $s2 = counted(); return $s2` — a plain 3-statement plan,
    /// one op per statement.
    fn simple_plan() -> DraftAst {
        DraftAst {
            body: vec![
                bind_counted("s0"),
                bind_counted("s1"),
                bind_counted("s2"),
                ret("s2"),
            ],
            ..Default::default()
        }
    }

    /// `$s0 = counted(); $s1 = counted(); $s2 = seq { counted(); counted() }; return $s2` — the
    /// third statement has TWO inner ops, so it can be "partially recorded" (one op's cell landed
    /// before the crash, the other didn't) — the exactly-once headline scenario needs that shape.
    fn seq_plan() -> DraftAst {
        DraftAst {
            body: vec![
                bind_counted("s0"),
                bind_counted("s1"),
                Node::Seq {
                    body: vec![call_counted(), call_counted()],
                    bind: Some(SymbolName("s2".into())),
                },
                ret("s2"),
            ],
            ..Default::default()
        }
    }

    /// A cassette cell for a zero-arg `counted()` call — `map_args_to_input` maps zero args to `{}`
    /// (L0 invariant), so every zero-arg dispatch hashes `sha256_hex("{}")` regardless of statement.
    fn counted_cell(content: &str, truncated: bool) -> RunEvent {
        let input_json = "{}";
        RunEvent::OpRecorded {
            seq: 0,
            step: StepId("step_counted_test".into()),
            op: "counted".into(),
            input_hash: sha256_hex(input_json),
            input_hash_redacted: None,
            input_view: Some(input_json.into()),
            input_view_truncated: false,
            content: content.into(),
            view: None,
            is_error: false,
            denied: false,
            redacted: false,
            truncated,
        }
    }

    /// Hand-write an open turn on `"sess"`: `TurnStarted` + an accepted `plan_source` for `ast`,
    /// then `completed` top-level statements' `StatementCompleted` rows (with REAL bound values, so
    /// the interpreter's own fast-forward rehydration succeeds) — no `TurnEnded`, simulating a crash
    /// right there. Returns `(turn_id, flow_key)`.
    fn seed_interrupted_turn(
        events: &EventStore,
        store: &FlowStore,
        ast: &DraftAst,
        completed: usize,
    ) -> (i64, String) {
        let turn_id = events
            .begin_turn("sess", "do the thing", "test-model")
            .unwrap();
        let source = flux_lang::format::format(ast);
        events
            .record_plan_attempt(
                "sess",
                turn_id,
                PlanAttempt {
                    step: 1,
                    outcome: "accepted".into(),
                    plan_source: Some(source),
                    ..Default::default()
                },
            )
            .unwrap();
        let key = flow_key(ast.name.as_deref(), &ast.body);
        for idx in 0..completed {
            let vid = store
                .put_value("sess", &FlowValue::String(format!("val-{idx}")))
                .unwrap();
            events
                .record_run_event(
                    "sess",
                    &RunEvent::StatementCompleted {
                        plan: key.clone(),
                        node: NodeId(idx as u32),
                        stmt: stmt_hash16(&ast.body[idx]),
                        value: Some(vid),
                        skipped: false,
                    },
                )
                .unwrap();
        }
        (turn_id, key)
    }

    #[tokio::test]
    async fn interrupted_detects_an_open_turn_with_its_accepted_plan() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = simple_plan();
        let (turn_id, key) = seed_interrupted_turn(&events, &store, &ast, 2);

        let it = interrupted(&events, "sess")
            .unwrap()
            .expect("an open turn exists");
        assert_eq!(it.turn_id, turn_id);
        assert_eq!(it.flow_key, key);
        assert_eq!(it.completed, 2);
        assert_eq!(it.user_input, "do the thing");
    }

    #[tokio::test]
    async fn interrupted_is_none_once_the_turn_closed() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = simple_plan();
        let (turn_id, _key) = seed_interrupted_turn(&events, &store, &ast, 3);
        events
            .end_turn("sess", turn_id, "completed", 3, "done", None)
            .unwrap();

        assert!(interrupted(&events, "sess").unwrap().is_none());
    }

    #[tokio::test]
    async fn interrupted_errs_loudly_when_the_crash_happened_during_planning() {
        let events = EventStore::in_memory().unwrap();
        events.begin_turn("sess", "do it", "test-model").unwrap();
        // No accepted plan attempt at all — the crash happened before planning finished.

        let err = interrupted(&events, "sess").unwrap_err();
        assert!(
            err.to_string().contains("planning"),
            "must name the crash-during-planning case, got: {err}"
        );
    }

    #[tokio::test]
    async fn exactly_once_headline_prefix_and_taped_op_never_refire_unrecorded_op_runs_live() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = seq_plan();
        seed_interrupted_turn(&events, &store, &ast, 2);
        // Statement 2 (`seq { counted(); counted() }`) crashed mid-flight: its first inner op's
        // cell landed, its second did not, and no `StatementCompleted` was ever appended for it.
        events
            .record_run_event("sess", &counted_cell("live-taped", false))
            .unwrap();

        let live_calls = Arc::new(AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = NullSink;
        let report = resurrect(&events, &store, &ex, "sess", &[], &mut sink)
            .await
            .unwrap()
            .expect("an interrupted turn exists");

        assert_eq!(
            live_calls.load(Ordering::SeqCst),
            1,
            "the completed prefix and the taped inner op must never re-fire; only the unrecorded \
             inner op runs live"
        );
        assert_eq!(report.statements_fast_forwarded, 2);
        assert_eq!(report.ops_served_from_cassette, 1);
        assert_eq!(report.ops_run_live, 1);
        assert_eq!(report.outcome, "resurrected");
        assert!(report.diverged.is_none());

        // Turn closed durably: `TurnEnded` + one assistant message.
        let turns = events.turns("sess").unwrap();
        assert!(turns[0].ended_at_ms.is_some());
        let convo = events.conversation("sess").unwrap();
        assert!(convo.iter().any(|m| m.role == flux_core::Role::Assistant));
    }

    #[tokio::test]
    async fn stale_prefix_cells_are_never_served_to_the_crash_tail() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = simple_plan();

        // A cell recorded for the (already-completed) prefix, appended BEFORE the boundary — must
        // never be considered part of the crash tail even though it matches the pending statement's
        // op/hash exactly.
        events
            .record_run_event("sess", &counted_cell("stale", false))
            .unwrap();
        seed_interrupted_turn(&events, &store, &ast, 2);

        let live_calls = Arc::new(AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = NullSink;
        let report = resurrect(&events, &store, &ex, "sess", &[], &mut sink)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            report.ops_served_from_cassette, 0,
            "the stale prefix cell must not be served"
        );
        assert_eq!(report.ops_run_live, 1);
        assert_eq!(live_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_deny_approver_gates_the_live_tail_and_halts() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = DraftAst {
            body: vec![
                bind_counted("s0"),
                bind_counted("s1"),
                Node::Confirm {
                    message: "run counted".into(),
                    risk: None,
                    body: vec![bind_counted("s2")],
                },
                ret("s2"),
            ],
            ..Default::default()
        };
        seed_interrupted_turn(&events, &store, &ast, 2);

        let live_calls = Arc::new(AtomicUsize::new(0));
        let ex = counting_tool_executor(
            Arc::new(ScriptedApprover {
                choice: ApprovalChoice::Deny,
            }),
            live_calls.clone(),
        );
        let mut sink = NullSink;
        let report = resurrect(&events, &store, &ex, "sess", &[], &mut sink)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            live_calls.load(Ordering::SeqCst),
            0,
            "a denied confirm must never reach the tool"
        );
        assert_eq!(report.outcome, "halted");
        assert!(report.halt.is_some());
    }

    #[tokio::test]
    async fn leftover_unconsumed_tail_cells_latch_a_loud_divergence() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = simple_plan();
        // Every statement already completed — resuming dispatches nothing at all — yet a crash-tail
        // cell was recorded after the last completion and never gets consumed.
        seed_interrupted_turn(&events, &store, &ast, 3);
        events
            .record_run_event("sess", &counted_cell("orphaned", false))
            .unwrap();

        let live_calls = Arc::new(AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = NullSink;
        let report = resurrect(&events, &store, &ex, "sess", &[], &mut sink)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(live_calls.load(Ordering::SeqCst), 0);
        assert!(
            report.diverged.is_some(),
            "an unconsumed crash-tail cell must latch loudly, not silently"
        );
    }

    #[tokio::test]
    async fn a_truncated_tail_cell_refuses_without_refiring() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = simple_plan();
        seed_interrupted_turn(&events, &store, &ast, 2);
        events
            .record_run_event("sess", &counted_cell("capped", true))
            .unwrap();

        let live_calls = Arc::new(AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = NullSink;
        let report = resurrect(&events, &store, &ex, "sess", &[], &mut sink)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            live_calls.load(Ordering::SeqCst),
            0,
            "a truncated cell must refuse, never re-fire the side effect live"
        );
        assert_eq!(report.outcome, "halted");
        assert!(report.diverged.is_some());
    }

    #[tokio::test]
    async fn a_crash_after_the_last_statement_closes_with_zero_dispatches() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = simple_plan();
        seed_interrupted_turn(&events, &store, &ast, 3);

        let live_calls = Arc::new(AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = NullSink;
        let report = resurrect(&events, &store, &ex, "sess", &[], &mut sink)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(live_calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.ops_served_from_cassette, 0);
        assert_eq!(report.ops_run_live, 0);
        assert_eq!(report.statements_fast_forwarded, 3);
        assert_eq!(report.outcome, "resurrected");
    }

    #[tokio::test]
    async fn a_crash_at_an_await_closes_as_suspended_with_the_suspension_persisted() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let ast = DraftAst {
            body: vec![
                bind_counted("s0"),
                bind_counted("s1"),
                Node::Await {
                    binding: None,
                    source: "user_input".into(),
                    as_type: None,
                    condition: None,
                },
            ],
            ..Default::default()
        };
        // Only statement 0 is ledgered: `Await`/`Checkpoint` are a free pass-through once the
        // matched prefix reaches them (they carry no ledger entry of their own — the interpreter's
        // shipped contract, unrelated to Resurrect), so a ledger that already covered statement 1
        // would walk straight past the await without ever reaching it live. Stopping the ledger at
        // statement 0 forces the normal sequential walk to dispatch statement 1 live and then hit
        // the await for real — the honest shape of "the crash happened right as the flow was about
        // to suspend".
        seed_interrupted_turn(&events, &store, &ast, 1);

        let live_calls = Arc::new(AtomicUsize::new(0));
        let ex = counting_tool_executor(Arc::new(AllowApprover), live_calls.clone());
        let mut sink = NullSink;
        let report = resurrect(&events, &store, &ex, "sess", &[], &mut sink)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            live_calls.load(Ordering::SeqCst),
            1,
            "statement 1 runs live"
        );
        assert_eq!(report.outcome, "suspended");
        assert!(store.has_suspension("sess").unwrap());
    }
}

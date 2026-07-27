//! D-176: the **rerun** drivers for Tune's world-pinned re-plan — re-execute a recorded session's
//! plans into a correlated destination session under a caller-supplied [`CassetteScope`], with no
//! model call, so the destination's own run trace stays self-contained (replayable, and
//! positionally diffable against the source via [`flux_events::run_diff`]).
//!
//! Neither existing driver fits Tune directly: [`crate::replay::replay_session`] writes into a
//! throwaway SCRATCH store, so nothing is diffable afterward; [`crate::fork::replay_prefix`] stops
//! at a chosen statement and then goes LIVE. Tune needs the full (or one-turn) recorded plan
//! sequence re-executed into a real, correlated session, entirely under the caller's pinned scope:
//! [`rerun_pinned`] is the pure-substitution driver (no model call, ever — the caller pins a
//! [`crate::cassette::FrozenTape`]), and [`replay_turns_prefix`] hermetically rebuilds the turns
//! BEFORE a re-plan target turn, so a model/prompt variant can then drive exactly one live turn with
//! [`crate::engine::FlowEngine::run_turn_pinned`].

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use flux_events::EventStore;
use flux_lang::ast::DraftAst;
use flux_lang::host::OpOutcome;
use flux_lang::runtime::flow_key;
use flux_runtime::{Executor, ToolResult};
use flux_secret::Redactor;

use crate::agent_sink::AgentSink;
use crate::cassette::{CassetteScope, Cell, RecordScope, ReplayTape};
use crate::state::FlowStore;
use crate::{FlowError, Result};

fn whatif_err(msg: impl Into<String>) -> FlowError {
    FlowError::Runtime(msg.into())
}

/// Wraps a caller's sink, forwarding every event through unchanged, while independently recording
/// each dispatch's `(op, input, outcome)` onto `dst` as an ordinary cassette cell.
///
/// Neither [`CassetteScope::Frozen`] nor [`CassetteScope::Replay`] ever calls
/// [`RecordScope::record`] for a SERVED (non-live) dispatch — by design, nothing ran live, so there
/// is no live tail to record — but a Tune rerun's correlated session must still be self-contained
/// (itself replayable, and diffable node-for-node against the source via
/// [`flux_events::run_diff`]'s cell comparison), so this driver builds that trail itself from the
/// SAME `AgentSink::tool_call`/`tool_result` pair the interpreter already fires for every dispatch,
/// served or live (`flux_lang::runtime::run_call`) — no new dispatch chokepoint, no `cassette.rs`
/// change. `tool_call`'s input is serialized identically to how `ExecutorHost::dispatch` hashes it
/// (`serde_json::to_string` of the same `serde_json::Value`), so a served cell's `input_hash`
/// matches on a later replay of `dst` itself.
///
/// `pending` is keyed by op name (a `VecDeque`, not a single slot) so same-named concurrent
/// `parallel` dispatches pair FIFO instead of a later `tool_call` silently clobbering an earlier
/// one's still-unmatched input.
///
/// `enabled` is `false` for a [`crate::cassette::OffTape::Live`] scope: a live-bridge miss is
/// ALREADY recorded onto `dst` by [`crate::cassette::FrozenTape::record_tail`] (the bridge this
/// sink's `record` would otherwise duplicate) — so this sink only self-records under `OffTape::Halt`,
/// the one case with no other recorder at all.
///
/// Public because every world-pinned driver needs it, not just this module's: the SDK's
/// `Scenario::check` re-drives a golden through
/// [`crate::engine::FlowEngine::run_turn_pinned`] (not through [`rerun_pinned`]) and faces the exact
/// same gap — a fully-served re-drive would otherwise record no cells at all and read as "every
/// statement vanished".
pub struct RerunRecordingSink<'a> {
    inner: &'a mut dyn AgentSink,
    record: RecordScope,
    redactor: Redactor,
    enabled: bool,
    pending: std::collections::HashMap<String, VecDeque<String>>,
}

impl<'a> RerunRecordingSink<'a> {
    /// Wrap `inner`, self-recording every dispatch onto `record`'s session when `enabled` — pass
    /// `enabled = false` whenever another recorder (a [`crate::cassette::OffTape::Live`] bridge, or
    /// a plain [`CassetteScope::Record`]) already writes that same tail.
    pub fn new(
        inner: &'a mut dyn AgentSink,
        record: RecordScope,
        redactor: Redactor,
        enabled: bool,
    ) -> Self {
        Self {
            inner,
            record,
            redactor,
            enabled,
            pending: std::collections::HashMap::new(),
        }
    }
}

impl AgentSink for RerunRecordingSink<'_> {
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
        if self.enabled {
            let input_json = serde_json::to_string(input).unwrap_or_default();
            self.pending
                .entry(name.to_string())
                .or_default()
                .push_back(input_json);
        }
        self.inner.tool_call(name, input);
    }
    fn tool_timing(&mut self, name: &str, timing: &flux_core::OperationTiming) {
        self.inner.tool_timing(name, timing);
    }
    fn tool_result(&mut self, name: &str, result: &ToolResult) {
        if self.enabled {
            if let Some(queue) = self.pending.get_mut(name) {
                if let Some(input_json) = queue.pop_front() {
                    let outcome = OpOutcome {
                        denied: false,
                        timing: None,
                        content: result.content.clone(),
                        view: result.view.clone(),
                        is_error: result.is_error,
                    };
                    self.record
                        .record(&self.redactor, name, &input_json, &outcome);
                }
            }
        }
        self.inner.tool_result(name, result);
    }
    fn observation(&mut self, o: &flux_evidence::Observation) {
        self.inner.observation(o);
    }
    fn turn_end(&mut self, usage: Option<flux_core::Usage>) {
        self.inner.turn_end(usage);
    }
}

/// Record a plan the rerun driver is about to execute as an accepted attempt on `dst` — the same
/// `plan_source` contract [`crate::fork::replay_prefix`] writes (L-38), so the correlated session
/// is itself replayable and its plan text resolves through [`flux_events::stmt_texts`] for a
/// rendered diff. Mirrors `fork.rs`'s private `record_fork_plan` (kept local here rather than
/// widened visibility across modules — same ~15-line shape, one caller each).
fn record_rerun_plan(
    events: &EventStore,
    executor: &Executor,
    dst: &str,
    turn_id: i64,
    ast: &DraftAst,
) {
    let source = flux_lang::format::format(ast);
    let redactor = &executor.context().redactor;
    let _ = events.record_plan_attempt(
        dst,
        turn_id,
        flux_events::PlanAttempt {
            step: 1,
            outcome: "accepted".into(),
            error: None,
            fingerprint: Some(flux_lang::runtime::sha256_hex(
                &serde_json::to_string(ast).unwrap_or_default(),
            )),
            plan_text: None,
            phase: None,
            plan_source: Some(redactor.redact(&source)),
            delta_source: None,
        },
    );
}

/// The ordered list of recorded plan executions to rerun: the trace-derived
/// [`crate::replay::execution_keys`], falling back to acceptance order for a non-resumable
/// recording (mirrors `replay_session`/`replay_prefix`), optionally narrowed to one 1-based turn's
/// accepted plans (mirrors `replay_session`'s `turn` filter).
fn selected_execution_keys(
    events: &EventStore,
    session: &str,
    trace: &[flux_lang::ast::RunEvent],
    accepted_order: Vec<String>,
    turn: Option<usize>,
) -> Result<Vec<String>> {
    let mut exec_keys = crate::replay::execution_keys(trace);
    if exec_keys.is_empty() {
        exec_keys = accepted_order;
    }
    if let Some(t) = turn {
        let turns = events.turns(session)?;
        let wanted = turns
            .get(t.saturating_sub(1))
            .ok_or_else(|| whatif_err(format!("turn {t} does not exist on session {session}")))?;
        let keys: HashSet<String> = wanted
            .plan_attempts
            .iter()
            .filter(|a| a.outcome == "accepted")
            .filter_map(|a| a.plan_source.as_deref())
            .filter_map(|src| flux_lang::parse::parse(src).ok())
            .map(|ast| flow_key(ast.name.as_deref(), &ast.body))
            .collect();
        exec_keys.retain(|k| keys.contains(k));
    }
    Ok(exec_keys)
}

/// The outcome of [`rerun_pinned`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct PinnedRerun {
    /// The correlated destination session's id (the same one the caller passed in).
    pub dst_session: String,
    /// The pinned scope's first latched divergence, if the recorded world was left (a
    /// [`crate::cassette::FrozenTape`] miss under [`crate::cassette::OffTape::Halt`], or a truncated
    /// cell) — `None` on a complete, hermetic rerun.
    pub left_world: Option<String>,
    /// The pinned scope's total recorded cassette cells (from the source session's own trace).
    pub cells_total: usize,
    /// How many of those cells were consumed serving this rerun.
    pub cells_consumed: usize,
}

/// Re-execute `src`'s recorded plan executions (optionally narrowed to one 1-based `turn`) into the
/// correlated session `dst`, entirely under the caller-supplied `scope` — **no model call**: this
/// driver only replays already-accepted `plan_source`s, so a pure [`crate::cassette::FrozenTape`]
/// substitution is fully offline by construction. Every executed plan is re-recorded on `dst` (the
/// same `plan_source` contract [`crate::fork::replay_prefix`] uses) inside one begin/end-turn
/// bracket, so [`flux_events::EventStore::run_trace`]`(dst)` is self-contained and diffs
/// positionally against `src`'s own trace via [`flux_events::run_diff`].
///
/// `store` is `dst`'s [`FlowStore`] (a fresh one for a throwaway variant engine, or the live
/// client's own, per the caller's isolation needs) — this function installs `scope` on it and
/// leaves it installed on return (the caller's next action, if any, reuses or replaces it).
#[allow(clippy::too_many_arguments)]
pub async fn rerun_pinned(
    events: &EventStore,
    store: &FlowStore,
    executor: &Executor,
    src: &str,
    dst: &str,
    turn: Option<usize>,
    scope: Arc<CassetteScope>,
    sink: &mut dyn AgentSink,
) -> Result<PinnedRerun> {
    let trace = events.run_trace(src)?;
    if trace.is_empty() {
        return Err(whatif_err(format!(
            "session {src} has no run trace recorded — nothing to rerun"
        )));
    }
    let cells_total = Cell::collect(&trace).len();
    let (plans, accepted_order) = crate::replay::plans_by_key(events, src)?;
    let exec_keys = selected_execution_keys(events, src, &trace, accepted_order, turn)?;
    if exec_keys.is_empty() {
        return Err(whatif_err(format!(
            "session {src} has no executed plan to rerun{}",
            turn.map(|t| format!(" in turn {t}")).unwrap_or_default()
        )));
    }

    store.set_cassette(Some(scope.clone()));

    let model = events.info(src).map(|i| i.model).unwrap_or_default();
    let turn_id = events.begin_turn(dst, &format!("<what-if {src}>"), &model)?;

    let self_record = matches!(
        scope.as_ref(),
        CassetteScope::Frozen(f) if f.off_tape() == crate::cassette::OffTape::Halt
    );
    let redactor = executor.context().redactor.clone();
    let record = RecordScope::new(store.event_store(), dst);
    let mut rec_sink = RerunRecordingSink::new(sink, record, redactor, self_record);
    for key in &exec_keys {
        let Some(ast) = plans.get(key) else {
            continue;
        };
        record_rerun_plan(events, executor, dst, turn_id, ast);
        let halt = store.open_halted_plan(dst)?;
        let ledger = halt.as_ref().map(|h| &h.ledger);
        crate::runtime::execute_flow_resumable_with_composites(
            store,
            executor,
            dst,
            ast,
            &[],
            ledger,
            &mut rec_sink,
        )
        .await?;
    }
    let _ = events.end_turn(dst, turn_id, "ok", exec_keys.len() as u32, "", None);

    let (left_world, cells_consumed) = match scope.as_ref() {
        CassetteScope::Frozen(frozen) => (
            frozen.diverged(),
            cells_total.saturating_sub(frozen.remaining()),
        ),
        _ => (None, 0),
    };

    Ok(PinnedRerun {
        dst_session: dst.to_string(),
        left_world,
        cells_total,
        cells_consumed,
    })
}

/// Hermetically rebuild `dst`'s state for every execution belonging to `src`'s turns strictly
/// before `upto_turn` (1-based) — Tune's re-plan path's prefix step: earlier turns replay under a
/// plain [`ReplayTape`] (nothing executes live, mirroring [`crate::fork::replay_prefix`]'s full-
/// execution loop), so the caller can then drive turn `upto_turn` itself with
/// [`crate::engine::FlowEngine::run_turn_pinned`] under a different model/prompt. `upto_turn <= 1`
/// (nothing precedes the first turn) and an empty trace are both no-ops.
///
/// Errors loudly if the prefix itself fails to replay faithfully — a re-plan whose OWN prefix can't
/// be trusted must not silently proceed.
pub async fn replay_turns_prefix(
    events: &EventStore,
    store: &FlowStore,
    executor: &Executor,
    src: &str,
    dst: &str,
    upto_turn: usize,
    sink: &mut dyn AgentSink,
) -> Result<()> {
    if upto_turn <= 1 {
        return Ok(());
    }
    let trace = events.run_trace(src)?;
    if trace.is_empty() {
        return Ok(());
    }
    let (plans, accepted_order) = crate::replay::plans_by_key(events, src)?;
    let mut exec_keys = crate::replay::execution_keys(&trace);
    if exec_keys.is_empty() {
        exec_keys = accepted_order;
    }

    let turns = events.turns(src)?;
    let wanted: HashSet<String> = turns
        .iter()
        .take(upto_turn.saturating_sub(1))
        .flat_map(|t| t.plan_attempts.iter())
        .filter(|a| a.outcome == "accepted")
        .filter_map(|a| a.plan_source.as_deref())
        .filter_map(|src| flux_lang::parse::parse(src).ok())
        .map(|ast| flow_key(ast.name.as_deref(), &ast.body))
        .collect();
    exec_keys.retain(|k| wanted.contains(k));
    if exec_keys.is_empty() {
        return Ok(());
    }

    let tape = ReplayTape::from_trace(&trace);
    let scope = Arc::new(CassetteScope::Replay(tape));
    store.set_cassette(Some(scope.clone()));

    let model = events.info(src).map(|i| i.model).unwrap_or_default();
    let turn_id = events.begin_turn(dst, &format!("<what-if prefix {src}>"), &model)?;
    let redactor = executor.context().redactor.clone();
    let record = RecordScope::new(store.event_store(), dst);
    // `Replay` never records anything itself (no bridge to duplicate against) — always self-record.
    let mut rec_sink = RerunRecordingSink::new(sink, record, redactor, true);
    for key in &exec_keys {
        let Some(ast) = plans.get(key) else {
            continue;
        };
        record_rerun_plan(events, executor, dst, turn_id, ast);
        let halt = store.open_halted_plan(dst)?;
        let ledger = halt.as_ref().map(|h| &h.ledger);
        crate::runtime::execute_flow_resumable_with_composites(
            store,
            executor,
            dst,
            ast,
            &[],
            ledger,
            &mut rec_sink,
        )
        .await?;
        if let CassetteScope::Replay(t) = scope.as_ref() {
            if let Some(d) = t.diverged() {
                return Err(whatif_err(format!(
                    "what-if prefix replay diverged before turn {upto_turn}: {d}"
                )));
            }
        }
    }
    let _ = events.end_turn(dst, turn_id, "ok", exec_keys.len() as u32, "", None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Node, SymbolName};
    use crate::cassette::{FrozenTape, OffTape, RecordScope};
    use async_trait::async_trait;
    use flux_lang::host::OpOutcome;
    use flux_runtime::{AllowApprover, PermissionManager, Tool, ToolContext, ToolRegistry};
    use flux_system::{System, Workspace};
    use serde_json::json;

    /// A read-only op that echoes its single positional arg back — enough to build a real
    /// `Executor` for these driver-only tests (no model, no adaptive loop involved).
    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> flux_spec::ToolSpec {
            flux_spec::ToolSpec::read_only(
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
        ) -> flux_core::Result<flux_runtime::ToolResult> {
            Ok(flux_runtime::ToolResult::ok(
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    fn test_executor() -> Executor {
        let dir = std::env::temp_dir().join(format!(
            "flux-flow-whatif-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        let perms = PermissionManager::from_rules(&["echo".into()], &[]);
        Executor::new(
            reg,
            perms,
            Arc::new(AllowApprover),
            ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap()))),
        )
    }

    fn echo_ast() -> DraftAst {
        DraftAst {
            body: vec![Node::Bind {
                name: SymbolName("x".into()),
                value: Box::new(Node::Call {
                    op: "echo".into(),
                    args: vec![Node::Lit { value: json!("hi") }],
                }),
                ty: None,
                effect: None,
            }],
            ..Default::default()
        }
    }

    struct TestSink;
    impl AgentSink for TestSink {}

    async fn record_one_op_session(events: &Arc<EventStore>, executor: &Executor) -> String {
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let session = events.create_session("mock").unwrap();
        store.set_cassette(Some(Arc::new(CassetteScope::Record(RecordScope::new(
            events.clone(),
            session.clone(),
        )))));
        let turn_id = events.begin_turn(&session, "hi", "mock").unwrap();
        let ast = echo_ast();
        record_rerun_plan(events, executor, &session, turn_id, &ast);
        let mut sink = TestSink;
        crate::runtime::execute_flow_resumable_with_composites(
            &store,
            executor,
            &session,
            &ast,
            &[],
            None,
            &mut sink,
        )
        .await
        .unwrap();
        let _ = events.end_turn(&session, turn_id, "ok", 1, "done", None);
        session
    }

    #[tokio::test]
    async fn rerun_pinned_replays_a_pure_substitution_with_no_dispatch() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let executor = test_executor();
        let src = record_one_op_session(&events, &executor).await;

        let trace = events.run_trace(&src).unwrap();
        let tape = ReplayTape::from_trace(&trace);
        let frozen = FrozenTape::hermetic(tape).substitute_op("echo", OpOutcome::ok("substituted"));
        let scope = Arc::new(CassetteScope::Frozen(frozen));

        let dst_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let dst = events
            .create_session_with_context(
                "mock",
                &flux_events::EventContext {
                    correlation_id: Some(src.clone()),
                    agent_id: Some(format!("what_if:{src}@1")),
                    ..Default::default()
                },
            )
            .unwrap();

        let mut sink = TestSink;
        let report = rerun_pinned(
            &events, &dst_store, &executor, &src, &dst, None, scope, &mut sink,
        )
        .await
        .unwrap();

        assert_eq!(report.dst_session, dst);
        assert!(report.left_world.is_none(), "no divergence expected");
        assert_eq!(report.cells_total, 1);
        assert_eq!(report.cells_consumed, 1);

        let dst_trace = events.run_trace(&dst).unwrap();
        let cells = Cell::collect(&dst_trace);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].content, "substituted");
    }

    #[tokio::test]
    async fn rerun_pinned_off_tape_halt_latches_loudly_on_a_diverged_substitution() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let executor = test_executor();
        let src = record_one_op_session(&events, &executor).await;

        // An empty tape under `OffTape::Halt` can never serve the recorded `echo` dispatch.
        let frozen = FrozenTape::hermetic(ReplayTape::from_cells(vec![]));
        assert_eq!(frozen.off_tape(), OffTape::Halt);
        let scope = Arc::new(CassetteScope::Frozen(frozen));

        let dst_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let dst = events.create_session("mock").unwrap();
        let mut sink = TestSink;

        let report = rerun_pinned(
            &events,
            &dst_store,
            &executor,
            &src,
            &dst,
            None,
            scope.clone(),
            &mut sink,
        )
        .await
        .unwrap();

        assert!(
            report.left_world.is_some(),
            "OffTape::Halt must latch a divergence on a miss and report it, not silently pass"
        );
        if let CassetteScope::Frozen(frozen) = scope.as_ref() {
            assert!(frozen.diverged().is_some());
        }
    }
}

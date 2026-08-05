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

use std::sync::Arc;

use flux_core::DispatchId;
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
/// C-531: `pending` is keyed by the call's `DispatchId`, so a result is matched to the exact call
/// that produced it. It used to be a per-op-name `VecDeque` paired FIFO — sound only while
/// same-name dispatches also COMPLETED in issue order, which C-528's parallel gather batches broke:
/// two concurrent `read`s released out of order recorded each other's input against the wrong
/// outcome. The identity match has no such hazard and needs no ordering assumption.
///
/// D-182: `enabled` alone used to be `false` for a [`crate::cassette::OffTape::Live`] scope on the
/// theory that a live-bridge miss is ALREADY recorded onto `dst` by
/// [`crate::cassette::FrozenTape::record_tail`], so self-recording would only ever duplicate it —
/// but that reasoning silently swept up every SERVED hit too, so a `Live`-scoped rerun that served
/// most of its dispatches from tape and only fell through to live on a few recorded NONE of the
/// served ones. [`Self::defer_for_live_bridge`] + [`Self::finish`] fix this precisely: `enabled` now
/// means "self-record every dispatch this sink sees", and deferred reconciliation (rather than a
/// real-time check) distinguishes served from live without racing the dispatch itself — see
/// [`Self::defer_for_live_bridge`]'s doc for why a real-time check does not work here.
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
    /// D-182: set via [`Self::defer_for_live_bridge`] whenever the pinned scope can possibly fall
    /// through to a live dispatch — `false` when it cannot (`OffTape::Halt`, or a
    /// [`crate::cassette::CassetteScope::Replay`] scope), in which case every dispatch this sink sees
    /// is, by construction, served, and is recorded eagerly, exactly as it always has been.
    defer: bool,
    /// Buffered `(op, input_json, outcome)` triples awaiting [`Self::finish`]'s reconciliation —
    /// populated only while `defer` is `true`.
    captured: Vec<(String, String, OpOutcome)>,
    /// The serialized input of every call still awaiting its result, keyed by dispatch id.
    pending: std::collections::HashMap<DispatchId, String>,
}

impl<'a> RerunRecordingSink<'a> {
    /// Wrap `inner`, self-recording every dispatch onto `record`'s session when `enabled` — pass
    /// `enabled = false` whenever another recorder (a plain [`CassetteScope::Record`]) already writes
    /// that same tail wholesale. Chain [`Self::defer_for_live_bridge`] when the pinned scope can go
    /// live, and call [`Self::finish`] once the driven turn/plan has fully completed.
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
            defer: false,
            captured: Vec::new(),
            pending: std::collections::HashMap::new(),
        }
    }

    /// D-182: opt into deferred, reconciled recording — required whenever the pinned scope can fall
    /// through to a live dispatch (a [`crate::cassette::FrozenTape`] under `OffTape::Live`), so a
    /// live fall-through's cell (already written by
    /// [`crate::cassette::FrozenTape::record_tail`]) is never ALSO self-recorded here.
    ///
    /// A real-time check (comparing a live counter's value at `tool_call` against its value at
    /// `tool_result`, for the SAME dispatch) looks obvious but is unsound in at least one real
    /// caller: the SDK's `Session::what_if()` re-plan drives the FULL adaptive agent loop, not a raw
    /// flow, and its native tool dispatches reach this sink through a composite op's own internal
    /// channel relay (`crate::loop_host::ChannelSink`) — delivery preserves the original dispatch
    /// ORDER but not its WALL-CLOCK timing relative to the live dispatch it describes, so a counter
    /// snapshotted at `tool_call` time can no longer reflect "before this dispatch" by the time
    /// `tool_result` fires. A live re-drive under this exact shape (one served + one live dispatch
    /// behind the agent loop) was observed to silently double-record the live one under a real-time
    /// delta check. [`Self::finish`] avoids the race by never comparing counters in real time at
    /// all: it waits until the whole turn/plan has settled, then reconciles once.
    ///
    /// Trade-off, honestly kept rather than hidden: deferring means every self-recorded (served)
    /// cell is appended to the underlying append-only stream AFTER whatever live cells
    /// `record_tail` already wrote synchronously during the run — even a served dispatch that
    /// happened FIRST in true execution order lands LAST in the persisted trace. This driver only
    /// promises completeness (every dispatch recorded, exactly once) under `Live`, not position; see
    /// [`Self::finish`] for the exact mechanism.
    pub fn defer_for_live_bridge(mut self) -> Self {
        self.defer = true;
        self
    }

    /// Flush every buffered dispatch from [`Self::defer_for_live_bridge`] mode, skipping any that
    /// [`crate::cassette::FrozenTape::record_tail`] already recorded onto the same `record` target —
    /// a no-op if `defer_for_live_bridge` was never called (the eager path already wrote everything
    /// per-dispatch, exactly as before D-182, in true position too — the ordering trade-off below is
    /// specific to the deferred path).
    ///
    /// MUST be called only once the driven turn/plan has fully settled (after the
    /// `run_turn_pinned`/`execute_flow_resumable_with_composites` future this sink was passed into
    /// resolves) — calling it earlier could read `dst`'s trace before an in-flight async sink relay
    /// has delivered every dispatch, under-counting the already-recorded cells and misclassifying a
    /// live one as served.
    ///
    /// The merge walks `captured` (this sink's own dispatches, in real order) against `already`
    /// (cells `record_tail` wrote, also in real order — a strict sub-sequence of `captured`, since
    /// every live dispatch appears in both and nothing else ever writes to `record`'s target under a
    /// deferred scope): whenever the next captured dispatch matches the next unconsumed `already`
    /// cell (by op + input hash), it was the live one — skip it and advance; otherwise it was served —
    /// record it now, which necessarily appends it AFTER every `already` cell present at this point
    /// (an append-only log offers no other option) — the positional trade-off documented on
    /// [`Self::defer_for_live_bridge`]. Misattribution is possible only if the identical `(op,
    /// input)` pair is dispatched more than once with a MIX of served and live outcomes in one run —
    /// narrow enough that no known caller hits it, and cheaper to document than to engineer around
    /// with the AgentSink surface available here.
    pub fn finish(self) {
        if !self.defer || self.captured.is_empty() {
            return;
        }
        let already = self.record.recorded_cells();
        let mut idx = 0usize;
        for (op, input_json, outcome) in self.captured {
            let input_hash = flux_lang::runtime::sha256_hex(&input_json);
            let matches_next_bridge_cell = already
                .get(idx)
                .is_some_and(|c| c.op == op && c.input_hash == input_hash);
            if matches_next_bridge_cell {
                idx += 1;
                continue;
            }
            self.record
                .record(&self.redactor, &op, &input_json, &outcome);
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
    fn tool_call(&mut self, dispatch: DispatchId, name: &str, input: &serde_json::Value) {
        if self.enabled {
            let input_json = serde_json::to_string(input).unwrap_or_default();
            self.pending.insert(dispatch, input_json);
        }
        self.inner.tool_call(dispatch, name, input);
    }
    fn tool_timing(
        &mut self,
        dispatch: DispatchId,
        name: &str,
        timing: &flux_core::OperationTiming,
    ) {
        self.inner.tool_timing(dispatch, name, timing);
    }
    fn tool_result(&mut self, dispatch: DispatchId, name: &str, result: &ToolResult) {
        if self.enabled {
            {
                if let Some(input_json) = self.pending.remove(&dispatch) {
                    // D-182: `denied` is hardcoded `false` here, not carried through from the real
                    // dispatch — `AgentSink::tool_result`'s `ToolResult` (the bridge every sink sees,
                    // `flux_runtime::ToolResult`) has no `denied` field at all; it is dropped one
                    // layer up, in `SinkBridge::tool_result` (`crates/flux-flow/src/runtime.rs`),
                    // which rebuilds a `ToolResult` from the interpreter's own `OpOutcome` without
                    // carrying `OpOutcome::denied` across. Widening `ToolResult` (a `flux_runtime`
                    // type used far beyond this sink) or `SinkBridge` to carry it is out of this
                    // pass's file-ownership scope — so a D-177 reauthorize denial served through this
                    // recording sink is re-recorded as a plain `is_error` outcome, indistinguishable
                    // here from an ordinary op failure. This is a real, narrow classification loss
                    // (not a correctness bug in `hermetic()`/`diff()`, which both key off the LIVE
                    // `FrozenTape` denial path, not off this recorded cell) — flagged in the D-182
                    // story rather than silently accepted.
                    let outcome = OpOutcome {
                        denied: false,
                        timing: None,
                        content: result.content.clone(),
                        view: result.view.clone(),
                        is_error: result.is_error,
                    };
                    if self.defer {
                        self.captured.push((name.to_string(), input_json, outcome));
                    } else {
                        self.record
                            .record(&self.redactor, name, &input_json, &outcome);
                    }
                }
            }
        }
        self.inner.tool_result(dispatch, name, result);
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
///
/// D-181: the `turn` narrowing used to filter the WHOLE-SESSION `exec_keys` list by content-
/// addressed `flow_key` **set membership** (`keys.contains(k)`) — sound only when a `flow_key`
/// identifies at most one execution session-wide. A plan re-accepted byte-identical in a later (or
/// earlier) turn shares its `flow_key` with the wanted turn's own execution, so set membership lets
/// that OTHER turn's execution leak into the "one turn" selection too. Deriving `exec_keys` directly
/// from the wanted turn's OWN turn-scoped trace ([`crate::cassette::turn_run_trace`]) instead avoids
/// the ambiguity entirely — it never looks at another turn's events, so there is nothing to leak.
fn selected_execution_keys(
    events: &EventStore,
    session: &str,
    trace: &[flux_lang::ast::RunEvent],
    accepted_order: Vec<String>,
    turn: Option<usize>,
) -> Result<Vec<String>> {
    let Some(t) = turn else {
        let mut exec_keys = crate::replay::execution_keys(trace);
        if exec_keys.is_empty() {
            exec_keys = accepted_order;
        }
        return Ok(exec_keys);
    };
    let turns = events.turns(session)?;
    let wanted = turns
        .get(t.saturating_sub(1))
        .ok_or_else(|| whatif_err(format!("turn {t} does not exist on session {session}")))?;
    let turn_trace = crate::cassette::turn_run_trace(events, session, wanted.turn_id)?;
    let mut exec_keys = crate::replay::execution_keys(&turn_trace);
    if exec_keys.is_empty() {
        // No resumable ledger markers within this turn's own window (a non-resumable recording) —
        // fall back to THIS turn's own accepted order, not the whole session's.
        exec_keys = accepted_plan_keys(std::iter::once(wanted));
    }
    Ok(exec_keys)
}

/// The `flow_key`s of every accepted plan attempt across the given turns, in order — the acceptance-
/// order fallback used when a turn's own trace has no resumable ledger markers to derive
/// `execution_keys` from.
fn accepted_plan_keys<'a>(
    turns: impl Iterator<Item = &'a flux_events::TurnSummary>,
) -> Vec<String> {
    turns
        .flat_map(|t| t.plan_attempts.iter())
        .filter(|a| a.outcome == "accepted")
        .filter_map(|a| a.plan_source.as_deref())
        .filter_map(|src| flux_lang::parse::parse(src).ok())
        .map(|ast| flow_key(ast.name.as_deref(), &ast.body))
        .collect()
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

/// The source-session executions a [`rerun_pinned`] will replay, resolved up front — and the whole
/// of that driver's **refusal** surface: an unrecorded source, a `turn` that does not exist, a turn
/// that executed no plan.
///
/// Resolving reads only the SOURCE session (its run trace, its accepted plans, its turn windows) and
/// never touches a destination, so a caller can decide "can this rerun run at all?" *before* it mints
/// the session the rerun would write into. That ordering is the point (C-254): a what-if answers
/// "what would happen" **without** it happening, so a driver that refuses once its destination
/// already exists has left behind exactly the artifact the simulation promised not to leave — and
/// silently, since nothing downstream tells a minted-then-refused rerun apart from one that never
/// minted. Passing a `RerunSelection` rather than a `(src, turn)` pair makes that structural instead
/// of incidental: there is no way to reach the driver, or the `dst` writes it opens with, while a
/// refusal is still outstanding, and no later reordering of the driver's body can reintroduce one.
///
/// What remains fallible inside `rerun_pinned` is execution, not refusal — a store write that fails,
/// or a replayed plan that errors. Those cannot be decided in advance, and a session recording the
/// attempt is the correct trace for them.
#[derive(Debug, Clone)]
pub struct RerunSelection {
    src: String,
    trace: Vec<flux_lang::ast::RunEvent>,
    plans: std::collections::HashMap<String, DraftAst>,
    exec_keys: Vec<String>,
}

// No `is_empty`: a resolved selection holds at least one execution by construction, because
// `resolve` refuses an empty one outright rather than returning it. A published `is_empty` that
// ignored its receiver and always answered `false` would read as a test an external caller could
// rely on, so the invariant is stated here instead of hidden inside a method that pretends to check
// it.
#[allow(clippy::len_without_is_empty)]
impl RerunSelection {
    /// Resolve `src`'s recorded plan executions, optionally narrowed to one 1-based `turn`, refusing
    /// loudly if there is nothing to rerun. Reads `src` only — never `dst`, which need not exist yet.
    pub fn resolve(events: &EventStore, src: &str, turn: Option<usize>) -> Result<Self> {
        let trace = events.run_trace(src)?;
        if trace.is_empty() {
            return Err(whatif_err(format!(
                "session {src} has no run trace recorded — nothing to rerun"
            )));
        }
        let (plans, accepted_order) = crate::replay::plans_by_key(events, src)?;
        let exec_keys = selected_execution_keys(events, src, &trace, accepted_order, turn)?;
        if exec_keys.is_empty() {
            return Err(whatif_err(format!(
                "session {src} has no executed plan to rerun{}",
                turn.map(|t| format!(" in turn {t}")).unwrap_or_default()
            )));
        }
        Ok(Self {
            src: src.to_string(),
            trace,
            plans,
            exec_keys,
        })
    }

    /// The source session this selection was resolved from.
    pub fn source(&self) -> &str {
        &self.src
    }

    /// The source session's full recorded run trace — what a caller pins its
    /// [`crate::cassette::FrozenTape`] to, so it need not read it a second time.
    pub fn trace(&self) -> &[flux_lang::ast::RunEvent] {
        &self.trace
    }

    /// How many recorded plan executions this selection will replay (never zero — an empty selection
    /// is a refusal, not a value).
    pub fn len(&self) -> usize {
        self.exec_keys.len()
    }
}

/// Re-execute a resolved [`RerunSelection`]'s recorded plan executions into the correlated session
/// `dst`, entirely under the caller-supplied `scope` — **no model call**: this driver only replays
/// already-accepted `plan_source`s, so a pure [`crate::cassette::FrozenTape`] substitution is fully
/// offline by construction. Every executed plan is re-recorded on `dst` (the same `plan_source`
/// contract [`crate::fork::replay_prefix`] uses) inside one begin/end-turn bracket, so
/// [`flux_events::EventStore::run_trace`]`(dst)` is self-contained and diffs positionally against the
/// source's own trace via [`flux_events::run_diff`].
///
/// Taking a `RerunSelection` rather than a `(src, turn)` pair is deliberate — see that type for why
/// the refusals must be discharged before `dst` exists at all.
///
/// `store` is `dst`'s [`FlowStore`] (a fresh one for a throwaway variant engine, or the live
/// client's own, per the caller's isolation needs) — this function installs `scope` on it and
/// leaves it installed on return (the caller's next action, if any, reuses or replaces it).
pub async fn rerun_pinned(
    events: &EventStore,
    store: &FlowStore,
    executor: &Executor,
    selection: &RerunSelection,
    dst: &str,
    scope: Arc<CassetteScope>,
    sink: &mut dyn AgentSink,
) -> Result<PinnedRerun> {
    let src = selection.source();
    let trace = selection.trace();
    let plans = &selection.plans;
    let exec_keys = &selection.exec_keys;
    let cells_total = Cell::collect(trace).len();

    store.set_cassette(Some(scope.clone()));

    let model = events.info(src).map(|i| i.model).unwrap_or_default();
    let turn_id = events.begin_turn(dst, &format!("<what-if {src}>"), &model)?;

    // D-182: self-record every SERVED dispatch regardless of `off_tape` mode — under `Halt` nothing
    // else ever records a served hit; under `Live` a served hit ALSO goes unrecorded by anything else
    // (only a live fall-through's tail is caught by `FrozenTape::record_tail`), so `enabled` must be
    // `true` for `Frozen` either way. `defer_for_live_bridge` is what keeps a `Live` fall-through from
    // being double-recorded (once by `record_tail`'s bridge, once here) — only needed under `Live`
    // (`Halt` never goes live, so the eager path is both correct and cheaper there).
    let self_record = matches!(scope.as_ref(), CassetteScope::Frozen(_));
    let live = matches!(
        scope.as_ref(),
        CassetteScope::Frozen(f) if f.off_tape() == crate::cassette::OffTape::Live
    );
    let redactor = executor.context().redactor.clone();
    let record = RecordScope::new(store.event_store(), dst);
    let mut rec_sink = RerunRecordingSink::new(sink, record, redactor, self_record);
    if live {
        rec_sink = rec_sink.defer_for_live_bridge();
    }
    for key in exec_keys {
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
    rec_sink.finish();
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
    // D-181: scoped to exactly the PREFIX turns' own window — the previous implementation built
    // `exec_keys` from the WHOLE-SESSION trace and then filtered by `flow_key` set membership
    // against the prefix turns' accepted plans. Set membership can't distinguish "this execution
    // belongs to a prefix turn" from "some execution somewhere shares this turn's plan's content
    // hash" — a plan re-accepted byte-identical in the TARGET turn (`upto_turn` itself, or a later
    // one) leaks that target turn's own execution (and its cells, via `ReplayTape::from_trace`)
    // into what is supposed to be a hermetic REPLAY of only what came before it. Deriving both
    // `exec_keys` and the replay tape from `crate::cassette::turns_run_trace` over just the prefix
    // turn ids avoids the ambiguity the same way `selected_execution_keys` now does.
    let turns = events.turns(src)?;
    let prefix_turn_ids: Vec<i64> = turns
        .iter()
        .take(upto_turn.saturating_sub(1))
        .map(|t| t.turn_id)
        .collect();
    if prefix_turn_ids.is_empty() {
        return Ok(());
    }
    let trace = crate::cassette::turns_run_trace(events, src, &prefix_turn_ids)?;
    if trace.is_empty() {
        return Ok(());
    }
    let (plans, _accepted_order) = crate::replay::plans_by_key(events, src)?;
    let mut exec_keys = crate::replay::execution_keys(&trace);
    if exec_keys.is_empty() {
        // No resumable ledger markers within the prefix's own window — fall back to the prefix
        // turns' own accepted order, not the whole session's.
        exec_keys = accepted_plan_keys(turns.iter().take(upto_turn.saturating_sub(1)));
    }
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

        let selection = RerunSelection::resolve(&events, &src, None).unwrap();
        let mut sink = TestSink;
        let report = rerun_pinned(
            &events, &dst_store, &executor, &selection, &dst, scope, &mut sink,
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
        let selection = RerunSelection::resolve(&events, &src, None).unwrap();
        let mut sink = TestSink;

        let report = rerun_pinned(
            &events,
            &dst_store,
            &executor,
            &selection,
            &dst,
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

    /// C-254: `rerun_pinned`'s refusals are discharged by [`RerunSelection::resolve`], which takes no
    /// destination at all — so they are decidable before a caller has minted anything, and cannot be
    /// reordered back below the driver's own `dst` writes. Both refusals are exercised here against a
    /// store holding exactly one session; the assertion that no second session appears is what proves
    /// the refusal is genuinely free of a destination rather than merely early in one.
    #[tokio::test]
    async fn rerun_selection_refuses_without_a_destination_session() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let executor = test_executor();
        let src = record_one_op_session(&events, &executor).await;
        let before = events.list(1_000).unwrap().len();

        // A turn that exists but executed no plan — nothing to rerun.
        let turn_id = events.begin_turn(&src, "just answer", "mock").unwrap();
        events
            .end_turn(&src, turn_id, "ok", 0, "no plan", None)
            .unwrap();
        let err = RerunSelection::resolve(&events, &src, Some(2))
            .expect_err("a turn with no executed plan has nothing to rerun");
        assert!(
            err.to_string().contains("no executed plan to rerun"),
            "the refusal must name the reason: {err}"
        );

        // A turn that does not exist at all.
        let err = RerunSelection::resolve(&events, &src, Some(99))
            .expect_err("turn 99 is not there at all");
        assert!(
            err.to_string().contains("does not exist"),
            "the refusal must name the reason: {err}"
        );

        assert_eq!(
            events.list(1_000).unwrap().len(),
            before,
            "resolving a rerun reads the source session only — a refusal cannot have created anything"
        );
    }

    /// Two turns on the same session run the BYTE-IDENTICAL plan (same `flow_key`) — records it
    /// twice, back to back.
    async fn record_two_identical_turns(events: &Arc<EventStore>, executor: &Executor) -> String {
        let store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let session = events.create_session("mock").unwrap();
        store.set_cassette(Some(Arc::new(CassetteScope::Record(RecordScope::new(
            events.clone(),
            session.clone(),
        )))));
        let ast = echo_ast();
        for _ in 0..2 {
            let turn_id = events.begin_turn(&session, "hi", "mock").unwrap();
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
        }
        session
    }

    /// D-181 (failing-first): `rerun_pinned`'s `turn` filter used to select executions by
    /// content-addressed `flow_key` SET MEMBERSHIP against the wanted turn's accepted plans — sound
    /// only when a `flow_key` identifies at most one execution session-wide. Here turn 2 re-accepts
    /// and re-executes the BYTE-IDENTICAL plan turn 1 ran, so the old filter's `keys.contains(k)`
    /// matches turn 2's execution too and replays it into `dst` even though only turn 1 was asked
    /// for. Turn-scoping the execution-key derivation itself (not just the plan-key filter) must
    /// replay ONLY turn 1's own execution.
    #[tokio::test]
    async fn rerun_pinned_turn_filter_does_not_leak_a_repeated_identical_plans_other_turn() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let executor = test_executor();
        let src = record_two_identical_turns(&events, &executor).await;

        let trace = events.run_trace(&src).unwrap();
        let tape = ReplayTape::from_trace(&trace);
        let frozen = FrozenTape::hermetic(tape);
        let scope = Arc::new(CassetteScope::Frozen(frozen));

        let dst_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let dst = events.create_session("mock").unwrap();
        let selection = RerunSelection::resolve(&events, &src, Some(1)).unwrap();
        let mut sink = TestSink;

        let report = rerun_pinned(
            &events, &dst_store, &executor, &selection, &dst, scope, &mut sink,
        )
        .await
        .unwrap();

        assert!(
            report.left_world.is_none(),
            "both turns' cells are on tape, hermetic replay of just one must never miss"
        );
        let dst_trace = events.run_trace(&dst).unwrap();
        let cells = Cell::collect(&dst_trace);
        assert_eq!(
            cells.len(),
            1,
            "turn 1 selection must replay ONLY turn 1's own execution, not turn 2's identical-plan \
             execution leaking in too"
        );
    }

    /// D-181 (failing-first): the same content-vs-turn leak in `replay_turns_prefix` — turn 2 (the
    /// prefix, `upto_turn = 3` wants turns 1..2 rebuilt) and turn 3 (the re-plan TARGET, must stay
    /// untouched by the prefix step) both accept the byte-identical plan. The old whole-session
    /// `exec_keys` filtered by `flow_key` set membership can't tell turn 3's execution apart from
    /// turn 2's identical one, so it leaks turn 3's own execution into what should be a hermetic
    /// rebuild of ONLY what came before it.
    #[tokio::test]
    async fn replay_turns_prefix_does_not_leak_the_target_turns_identical_plan() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let executor = test_executor();

        // Turn 1: some other plan, distinguishable from the repeated one.
        let src = record_one_op_session(&events, &executor).await;
        let dst_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        dst_store.set_cassette(Some(Arc::new(CassetteScope::Record(RecordScope::new(
            events.clone(),
            src.clone(),
        )))));

        // Turn 2 and turn 3: the identical plan, executed twice more (turn 2 = the prefix's last
        // turn, turn 3 = the re-plan target that must NOT be replayed by the prefix step).
        let ast = echo_ast();
        for _ in 0..2 {
            let turn_id = events.begin_turn(&src, "hi", "mock").unwrap();
            record_rerun_plan(&events, &executor, &src, turn_id, &ast);
            let mut sink = TestSink;
            crate::runtime::execute_flow_resumable_with_composites(
                &dst_store,
                &executor,
                &src,
                &ast,
                &[],
                None,
                &mut sink,
            )
            .await
            .unwrap();
            let _ = events.end_turn(&src, turn_id, "ok", 1, "done", None);
        }

        let dst = events.create_session("mock").unwrap();
        let dst_store2 = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let mut sink = TestSink;
        replay_turns_prefix(&events, &dst_store2, &executor, &src, &dst, 3, &mut sink)
            .await
            .unwrap();

        let dst_trace = events.run_trace(&dst).unwrap();
        let cells = Cell::collect(&dst_trace);
        assert_eq!(
            cells.len(),
            2,
            "the prefix (turns 1 and 2) must replay exactly twice — turn 3's identical-plan \
             execution must never leak into the prefix rebuild"
        );
    }

    /// D-182: `rerun_pinned` under `OffTape::Live`, driving a plan with TWO statements — the first
    /// matches a recorded cell (served from tape), the second does not (falls through live) — must
    /// self-record the SERVED cell too, not just the live one. Completeness (both recorded, exactly
    /// once), not position: `RerunRecordingSink::finish` deliberately defers every self-recorded
    /// (served) cell until the whole plan has run, so it can reconcile against whatever
    /// `FrozenTape::record_tail` already wrote for a live fall-through WITHOUT risking a real-time
    /// mis-classification (see `finish`'s doc for the concrete case that ruled out a real-time
    /// check) — the deferred cell therefore always lands AFTER any live cell in the underlying
    /// append-only stream, even though it happened FIRST in true execution order. Documented, not
    /// silently accepted: a follow-on story could recover exact position by teaching this
    /// reconciliation to consult the plan's own step trace, out of this pass's scope.
    #[tokio::test]
    async fn rerun_pinned_off_tape_live_records_both_the_served_and_the_live_cell_completely() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let executor = test_executor();
        let src = record_one_op_session(&events, &executor).await; // one recorded cell: echo("hi")

        let trace = events.run_trace(&src).unwrap();
        let tape = ReplayTape::from_trace(&trace);

        let dst_store = FlowStore::in_memory_with_events(events.clone()).unwrap();
        let dst = events.create_session("mock").unwrap();
        let bridge = RecordScope::new(events.clone(), dst.clone());
        let frozen = FrozenTape::live_bridge(tape, bridge);
        let scope = Arc::new(CassetteScope::Frozen(frozen));

        // Two statements: the first calls `echo("hi")` — matches the recorded cell, served from
        // tape; the second calls `echo("bye")` — no matching cell, falls through live.
        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: SymbolName("x".into()),
                    value: Box::new(Node::Call {
                        op: "echo".into(),
                        args: vec![Node::Lit { value: json!("hi") }],
                    }),
                    ty: None,
                    effect: None,
                },
                Node::Bind {
                    name: SymbolName("y".into()),
                    value: Box::new(Node::Call {
                        op: "echo".into(),
                        args: vec![Node::Lit {
                            value: json!("bye"),
                        }],
                    }),
                    ty: None,
                    effect: None,
                },
            ],
            ..Default::default()
        };
        let mut sink = TestSink;
        // Record the plan directly (bypassing the "plans_by_key" lookup rerun_pinned normally does
        // from a source session's OWN accepted plans) so this test can drive a plan with a NEW
        // statement shape the source session never actually executed.
        store_and_run_pinned_plan(
            &events,
            &dst_store,
            &executor,
            &dst,
            &ast,
            scope.clone(),
            &mut sink,
        )
        .await;

        let dst_trace = events.run_trace(&dst).unwrap();
        let cells = Cell::collect(&dst_trace);
        assert_eq!(
            cells.len(),
            2,
            "both the served cell (echo hi) and the live cell (echo bye) must be recorded, exactly \
             once each: {dst_trace:?}"
        );
        let mut contents: Vec<_> = cells.iter().map(|c| c.content.as_str()).collect();
        contents.sort();
        assert_eq!(
            contents,
            vec!["bye", "hi"],
            "no drop, no duplicate — position is a separate, documented limitation"
        );
        if let CassetteScope::Frozen(f) = scope.as_ref() {
            assert_eq!(
                f.went_live(),
                1,
                "exactly one dispatch fell through to live"
            );
        }
    }

    /// Drive one plan directly under a pinned `scope`, self-recording via `RerunRecordingSink`
    /// exactly as `rerun_pinned`'s own inner loop does — a minimal stand-in so the ordering test
    /// above can exercise an AST that the source session never actually executed (`rerun_pinned`
    /// itself only ever replays a source session's OWN previously-accepted plans).
    async fn store_and_run_pinned_plan(
        events: &Arc<EventStore>,
        store: &FlowStore,
        executor: &Executor,
        dst: &str,
        ast: &DraftAst,
        scope: Arc<CassetteScope>,
        sink: &mut dyn AgentSink,
    ) {
        store.set_cassette(Some(scope.clone()));
        let turn_id = events.begin_turn(dst, "hi", "mock").unwrap();
        let redactor = executor.context().redactor.clone();
        let record = RecordScope::new(store.event_store(), dst);
        let mut rec_sink = RerunRecordingSink::new(sink, record, redactor, true);
        if matches!(scope.as_ref(), CassetteScope::Frozen(f) if f.off_tape() == OffTape::Live) {
            rec_sink = rec_sink.defer_for_live_bridge();
        }
        record_rerun_plan(events, executor, dst, turn_id, ast);
        crate::runtime::execute_flow_resumable_with_composites(
            store,
            executor,
            dst,
            ast,
            &[],
            None,
            &mut rec_sink,
        )
        .await
        .unwrap();
        rec_sink.finish();
        let _ = events.end_turn(dst, turn_id, "ok", 1, "", None);
    }
}

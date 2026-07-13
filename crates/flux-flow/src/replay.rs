//! A-45: the hermetic **replay** driver — re-execute a recorded session exactly, offline: plans
//! come from the durable `PlanAttempted.plan_source` (parsed deterministically, no model call),
//! leaf-op outputs are served from the C-43 cassette (no live IO, side effects never re-fire).
//!
//! The driver does NOT re-drive the agent loop (no model stages, no approval machinery): it re-executes
//! the recorded *executions* in recorded order. The execution list is derived from the trace
//! itself — `StatementCompleted`/`PlanHalted` rows carry the plan's `flow_key` — which reproduces
//! the loop host's dispositions by construction: a plan that was accepted but never executed (the
//! A-05 byte-identical skip, or a chat round) left no execution rows and is not re-executed; a
//! halted plan replays exactly its completed prefix and halts on the same statement (the recorded
//! failing cell serves `is_error`/`denied` back); a revision's fast-forward re-derives from the
//! replay session's own halt latch, exactly as the live run did.

use std::collections::HashMap;
use std::sync::Arc;

use flux_events::EventStore;
use flux_lang::ast::{DraftAst, RunEvent};
use flux_lang::runtime::flow_key;
use flux_runtime::Executor;

use crate::agent_sink::AgentSink;
use crate::cassette::{CassetteScope, ReplayTape};
use crate::state::FlowStore;
use crate::{FlowError, Result};

/// One re-executed plan in the replay report.
#[derive(Debug)]
pub struct ReplayedPlan {
    pub flow_key: String,
    /// The reproduced halt message, if the recorded execution halted here too.
    pub halted: Option<String>,
}

/// The outcome of a hermetic session replay.
#[derive(Debug)]
pub struct ReplayReport {
    pub session: String,
    pub plans: Vec<ReplayedPlan>,
    /// Divergence diagnostic — `None` on a faithful replay.
    pub diverged: Option<String>,
    pub cells_total: usize,
    pub cells_consumed: usize,
    /// Executions whose plan source is missing from the log (pre-L-38 turns, or a >32K
    /// `plan_source` drop) — reported honestly instead of silently skipped.
    pub missing_sources: usize,
}

fn not_replayable(session: &str, why: &str) -> FlowError {
    FlowError::Runtime(format!("session {session} is not replayable: {why}"))
}

/// The ordered list of recorded plan *executions*: walk the trace's statement rows and start a new
/// execution whenever the `flow_key` changes or the same key restarts at a non-increasing node
/// index (a revision re-running the same plan).
pub(crate) fn execution_keys(trace: &[RunEvent]) -> Vec<String> {
    let mut keys = Vec::new();
    let mut cur: Option<(&str, u32)> = None;
    for ev in trace {
        let (plan, node) = match ev {
            RunEvent::StatementCompleted { plan, node, .. } => (plan.as_str(), node.0),
            RunEvent::PlanHalted { plan, node, .. } => (plan.as_str(), node.0),
            _ => continue,
        };
        let new_exec = match cur {
            Some((k, last)) => k != plan || node <= last,
            None => true,
        };
        if new_exec {
            keys.push(plan.to_string());
        }
        cur = Some((plan, node));
    }
    keys
}

/// Map every accepted `plan_source` in the session to its parsed plan, keyed by the same
/// `flow_key` derivation the interpreter stamps on `StatementCompleted.plan`. The second return
/// is the acceptance ORDER of those keys — the fallback execution list for recordings whose
/// executions left no statement rows (the non-resumable `flow run` path ledgers nothing).
#[allow(clippy::type_complexity)]
pub(crate) fn plans_by_key(
    events: &EventStore,
    session: &str,
) -> Result<(HashMap<String, DraftAst>, Vec<String>)> {
    let mut map = HashMap::new();
    let mut order = Vec::new();
    for turn in events.turns(session)? {
        for attempt in turn.plan_attempts {
            if attempt.outcome != "accepted" {
                continue;
            }
            let Some(src) = attempt.plan_source else {
                continue;
            };
            // Redaction keeps `plan_source` parseable by construction (L-38); an unparseable
            // source is surfaced, not skipped — a silently missing plan would read as divergence.
            let ast = flux_lang::parse::parse(&src).map_err(|e| {
                not_replayable(
                    session,
                    &format!("stored plan_source no longer parses: {e}"),
                )
            })?;
            let key = flow_key(ast.name.as_deref(), &ast.body);
            order.push(key.clone());
            map.insert(key, ast);
        }
    }
    Ok((map, order))
}

/// Hermetically replay a recorded session: every recorded execution re-runs over a fresh
/// in-memory store + scratch event log, leaf ops served from the session's cassette. `executor`
/// supplies only the op *catalog* (positional-arg mapping) — its dispatch is never reached for a
/// served op, and `confirm` gates auto-allow under a `Replay` scope (nothing can execute).
/// `turn` (1-based) restricts replay to that turn's plans — cross-turn symbol references then
/// fail honestly (their bindings were made by earlier, unreplayed turns).
pub async fn replay_session(
    events: &EventStore,
    executor: &Executor,
    session: &str,
    turn: Option<usize>,
    sink: &mut dyn AgentSink,
) -> Result<ReplayReport> {
    let trace = events.run_trace(session)?;
    if trace.is_empty() {
        return Err(not_replayable(session, "no run trace recorded"));
    }
    let tape = ReplayTape::from_trace(&trace);
    let cells_total = tape.len();
    if cells_total == 0 {
        return Err(not_replayable(
            session,
            "no cassette cells recorded (pre-C-43 run, or FLUX_CASSETTE=0)",
        ));
    }

    let (plans, accepted_order) = plans_by_key(events, session)?;
    let mut exec_keys = execution_keys(&trace);
    if exec_keys.is_empty() {
        // No statement rows (the non-resumable `flow run` path ledgers nothing) — fall back to
        // the accepted plans in acceptance order. An empty fallback too is not replayable.
        exec_keys = accepted_order;
        if exec_keys.is_empty() {
            return Err(not_replayable(
                session,
                "no executed statements and no accepted plan_source recorded",
            ));
        }
    }
    if let Some(t) = turn {
        // Keys accepted by the requested turn only (1-based, matching `flux usage`'s turn order).
        let turns = events.turns(session)?;
        let wanted = turns
            .get(t.saturating_sub(1))
            .ok_or_else(|| not_replayable(session, &format!("turn {t} does not exist")))?;
        let keys: std::collections::HashSet<String> = wanted
            .plan_attempts
            .iter()
            .filter(|a| a.outcome == "accepted")
            .filter_map(|a| a.plan_source.as_deref())
            .filter_map(|src| flux_lang::parse::parse(src).ok())
            .map(|ast| flow_key(ast.name.as_deref(), &ast.body))
            .collect();
        exec_keys.retain(|k| keys.contains(k));
    }

    // Fresh, isolated replay substrate: nothing here touches the recorded session or the live
    // events.db — replay is a pure read of the recording.
    let scratch = Arc::new(EventStore::in_memory()?);
    let rsid = scratch.create_session("replay")?;
    let rstore = FlowStore::in_memory_with_events(scratch)?;
    let scope = Arc::new(CassetteScope::Replay(tape));
    rstore.set_cassette(Some(scope.clone()));

    let mut report = ReplayReport {
        session: session.to_string(),
        plans: Vec::new(),
        diverged: None,
        cells_total,
        cells_consumed: 0,
        missing_sources: 0,
    };

    for key in exec_keys {
        let Some(ast) = plans.get(&key) else {
            report.missing_sources += 1;
            continue;
        };
        // Mirror the live loop host: fold the replay session's OWN halt latch so a revision
        // fast-forwards its completed prefix exactly as the recorded run did.
        let halt = rstore.open_halted_plan(&rsid)?;
        let ledger = halt.as_ref().map(|h| &h.ledger);
        let out = crate::runtime::execute_flow_resumable_with_composites(
            &rstore,
            executor,
            &rsid,
            ast,
            &[],
            ledger,
            sink,
        )
        .await?;
        report.plans.push(ReplayedPlan {
            flow_key: key,
            halted: out.failure.as_ref().map(|f| f.message.clone()),
        });
        if let CassetteScope::Replay(t) = scope.as_ref() {
            if let Some(d) = t.diverged() {
                report.diverged = Some(d);
                break;
            }
        }
    }

    if let CassetteScope::Replay(t) = scope.as_ref() {
        report.cells_consumed = cells_total - t.remaining();
        if report.diverged.is_none() {
            report.diverged = t.diverged();
        }
    }
    Ok(report)
}

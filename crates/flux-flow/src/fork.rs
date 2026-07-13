//! A-46: the **fork** engine — branch a recorded run at a chosen top-level statement of its final
//! executed plan, into a NEW session: the prefix replays hermetically from the C-43 cassette
//! (no side effects), then the tail DIVERGES live through the real envelope.
//!
//! The cassette-vs-live boundary is the scope swap: prefix statements run under
//! `CassetteScope::Replay` (dispatches served from tape, nothing executes), and the moment the
//! prefix lands, [`replay_prefix`] swaps the store's scope to `CassetteScope::Record` for the fork
//! session — so the live tail goes through the real `Executor::dispatch`
//! (authorization/approval unchanged) AND the forked run is itself replayable.

use flux_events::EventStore;
use flux_lang::ast::{DraftAst, Node, RunEvent};
use flux_lang::runtime::{flow_key, LedgerEntry, ResumeLedger};
use flux_runtime::Executor;

use std::sync::Arc;

use crate::agent_sink::AgentSink;
use crate::cassette::{CassetteScope, RecordScope, ReplayTape};
use crate::replay::{execution_keys, plans_by_key};
use crate::state::FlowStore;
use crate::{FlowError, Result};

/// What [`replay_prefix`] hands the divergence modes.
pub struct ForkPrefix {
    /// The full target plan (the recorded run's FINAL executed plan) the fork diverges inside.
    pub plan: DraftAst,
    /// The fork statement index into `plan.body` (everything before it was replayed).
    pub at: usize,
    /// The replayed prefix's completed-statement ledger (fresh value ids in the fork session's
    /// store) — mode C's fast-forward memory.
    pub ledger: ResumeLedger,
    /// The fork session's turn id — every plan the fork executes records an accepted
    /// `PlanAttempted` under it, so the FORK is itself replayable (`flux replay <fork>` needs a
    /// `plan_source` per executed plan, and modes A/C bypass the loop host's recorder).
    pub turn_id: i64,
}

/// Record a plan the fork is about to execute as an accepted attempt on the fork session —
/// the same `plan_source` contract the loop host writes (L-38), so `flux replay <fork>` and
/// `flux diff` treat a forked run exactly like a recorded live run. Redaction is unnecessary
/// here by construction: prefix/tail bodies come from an already-redacted `plan_source`, the
/// synthetic inject plan from operator input — but scrub anyway via the executor's redactor for
/// parity with the loop host (C-22).
fn record_fork_plan(
    events: &EventStore,
    executor: &Executor,
    fork_session: &str,
    turn_id: i64,
    ast: &DraftAst,
) {
    let source = flux_lang::format::format(ast);
    let redactor = &executor.context().redactor;
    let _ = events.record_plan_attempt(
        fork_session,
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

fn fork_err(msg: impl Into<String>) -> FlowError {
    FlowError::Runtime(msg.into())
}

/// Hermetically reconstruct the recorded run's state INTO the fork session, up to (excluding)
/// statement `at` of the final executed plan: every earlier execution replays in full, then the
/// target plan's first `at` statements replay as a truncated body. On success the store's scope is
/// swapped to `Record` for the fork session — the caller's live tail is gated by the real
/// envelope and recorded like any run.
pub async fn replay_prefix(
    events: &EventStore,
    store: &FlowStore,
    executor: &Executor,
    src: &str,
    fork_session: &str,
    at: usize,
    sink: &mut dyn AgentSink,
) -> Result<ForkPrefix> {
    let trace = events.run_trace(src)?;
    let tape = ReplayTape::from_trace(&trace);
    if tape.is_empty() {
        return Err(fork_err(format!(
            "session {src} is not forkable: no cassette cells recorded (pre-C-43 run, or FLUX_CASSETTE=0)"
        )));
    }
    let (plans, accepted_order) = plans_by_key(events, src)?;
    let mut exec_keys = execution_keys(&trace);
    // Host-derived native-call graphs use the non-resumable executor: their cassette cells and
    // accepted source are durable, but they do not emit statement-ledger rows. Preserve fork for
    // those adaptive turns by using accepted execution order, exactly as replay_session does.
    if exec_keys.is_empty() {
        exec_keys = accepted_order;
    }
    let Some(target_key) = exec_keys.last().cloned() else {
        return Err(fork_err(format!(
            "session {src} has no executed plan to fork"
        )));
    };
    let target = plans
        .get(&target_key)
        .ok_or_else(|| {
            fork_err(format!(
                "session {src}'s final plan has no stored plan_source — not forkable"
            ))
        })?
        .clone();
    if at >= target.body.len() {
        return Err(fork_err(format!(
            "--at {at} is out of range: the final plan has {} top-level statement(s)",
            target.body.len()
        )));
    }

    let scope = Arc::new(CassetteScope::Replay(tape));
    store.set_cassette(Some(scope.clone()));

    // Every execution before the final one replays in full; the final one replays truncated.
    let full = &exec_keys[..exec_keys.len() - 1];
    for key in full {
        let Some(ast) = plans.get(key) else {
            return Err(fork_err(format!(
                "a recorded execution ({key}) has no stored plan_source — not forkable"
            )));
        };
        let halt = store.open_halted_plan(fork_session)?;
        let ledger = halt.as_ref().map(|h| &h.ledger);
        crate::runtime::execute_flow_resumable_with_composites(
            store,
            executor,
            fork_session,
            ast,
            &[],
            ledger,
            sink,
        )
        .await?;
        if let CassetteScope::Replay(t) = scope.as_ref() {
            if let Some(d) = t.diverged() {
                return Err(fork_err(format!("prefix replay diverged: {d}")));
            }
        }
    }
    // One fork turn scopes every plan-attempt record below (the fork's replayability contract).
    let model = events
        .info(fork_session)
        .map(|i| i.model)
        .unwrap_or_default();
    let turn_id = events.begin_turn(fork_session, &format!("<fork {src} @ {at}>"), &model)?;

    // The full earlier executions also re-record their plans on the fork session, so the fork's
    // own trace is self-contained for replay.
    for key in full {
        if let Some(ast) = plans.get(key) {
            record_fork_plan(events, executor, fork_session, turn_id, ast);
        }
    }
    if at > 0 {
        let truncated = DraftAst {
            name: None,
            params: target.params.clone(),
            returns: None,
            body: target.body[..at].to_vec(),
        };
        record_fork_plan(events, executor, fork_session, turn_id, &truncated);
        crate::runtime::execute_flow_resumable_with_composites(
            store,
            executor,
            fork_session,
            &truncated,
            &[],
            None,
            sink,
        )
        .await?;
        if let CassetteScope::Replay(t) = scope.as_ref() {
            if let Some(d) = t.diverged() {
                return Err(fork_err(format!("prefix replay diverged: {d}")));
            }
        }
    }

    // The ledger of everything the fork session just completed for the TARGET plan's prefix —
    // mode C's fast-forward memory (content-hash driven, so it matches the edited plan's
    // unchanged statements regardless of the truncated body's own flow_key).
    let completed: Vec<LedgerEntry> = store
        .events(fork_session)?
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::StatementCompleted {
                node, stmt, value, ..
            } => Some(LedgerEntry {
                node: *node,
                stmt: stmt.clone(),
                value: value.clone(),
            }),
            _ => None,
        })
        .collect();
    let ledger = ResumeLedger {
        completed,
        prior_plan: flow_key(None, &target.body[..at]),
    };

    // THE cassette-vs-live boundary: from here on, dispatches are real (envelope intact) and the
    // forked tail records its own cells — the fork is itself replayable.
    store.set_cassette(Some(Arc::new(CassetteScope::Record(RecordScope::new(
        store.event_store(),
        fork_session,
    )))));

    Ok(ForkPrefix {
        plan: target,
        at,
        ledger,
        turn_id,
    })
}

/// Divergence mode A — inject: bind `value` as the result of the fork statement (which must be a
/// top-level `bind`), then run the remainder of the plan LIVE. Injection canonicalizes through
/// [`flux_lang::runtime::lit_value`] (D-67) so the injected symbol is indistinguishable from a
/// literal-bound one downstream.
pub async fn diverge_inject(
    store: &FlowStore,
    executor: &Executor,
    fork_session: &str,
    prefix: &ForkPrefix,
    value: &serde_json::Value,
    sink: &mut dyn AgentSink,
) -> Result<flux_lang::runtime::FlowOutcome> {
    let stmt = &prefix.plan.body[prefix.at];
    let Node::Bind { name, ty, .. } = stmt else {
        return Err(fork_err(format!(
            "--inject needs the fork statement to be a top-level bind; statement {} is not",
            prefix.at
        )));
    };
    // The injection is itself a PLAN — a synthetic `bind name = lit(value)` prepended to the tail
    // — not a store-level poke: the interpreter binds it (lit canonicalization, D-67 parity), the
    // ledger records it, and the recorded plan_source makes the fork fully replayable.
    let mut body = vec![Node::Bind {
        name: name.clone(),
        value: Box::new(Node::Lit {
            value: value.clone(),
        }),
        ty: ty.clone(),
        effect: None,
    }];
    body.extend_from_slice(&prefix.plan.body[prefix.at + 1..]);
    let tail = DraftAst {
        name: None,
        params: prefix.plan.params.clone(),
        returns: prefix.plan.returns.clone(),
        body,
    };
    record_fork_plan(
        &store.event_store(),
        executor,
        fork_session,
        prefix.turn_id,
        &tail,
    );
    crate::runtime::execute_flow_resumable_with_composites(
        store,
        executor,
        fork_session,
        &tail,
        &[],
        None,
        sink,
    )
    .await
}

/// Divergence mode C — edit: run an edited plan against the replayed prefix's ledger. Unchanged
/// leading statements fast-forward (rehydrating the prefix's fresh values); the first edited
/// statement and everything after run LIVE.
pub async fn diverge_edit(
    store: &FlowStore,
    executor: &Executor,
    fork_session: &str,
    prefix: &ForkPrefix,
    edited: &DraftAst,
    sink: &mut dyn AgentSink,
) -> Result<flux_lang::runtime::FlowOutcome> {
    record_fork_plan(
        &store.event_store(),
        executor,
        fork_session,
        prefix.turn_id,
        edited,
    );
    crate::runtime::execute_flow_resumable_with_composites(
        store,
        executor,
        fork_session,
        edited,
        &[],
        Some(&prefix.ledger),
        sink,
    )
    .await
}

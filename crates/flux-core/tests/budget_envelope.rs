//! C-542 — the shared time/token budget vocabulary.
//!
//! These assertions are the contract every surface (flow enforcement, TUI projection, CLI JSON)
//! reads. They pin the two things that make the vocabulary usable across scopes: a soft target is
//! not a hard limit, and a ledger charges each measured effect exactly once even when a caller
//! re-reports an already-charged child total.

use flux_core::{
    BudgetAttribution, BudgetCharge, BudgetDimension, BudgetEnvelope, BudgetLedger, BudgetLimits,
    BudgetScope, BudgetSpend, BudgetUsageEvent,
};

fn attribution() -> BudgetAttribution {
    BudgetAttribution {
        run_id: "run-1".into(),
        session_id: Some("s-1".into()),
        turn_id: Some(7),
        segment: Some("explore".into()),
    }
}

fn tokens(event_id: &str, scope: BudgetScope, total: u64) -> BudgetUsageEvent {
    BudgetUsageEvent {
        event_id: event_id.into(),
        scope,
        attribution: attribution(),
        spend: BudgetSpend {
            model_calls: 1,
            input_tokens: total / 2,
            output_tokens: total - total / 2,
            total_tokens: total,
            ..BudgetSpend::default()
        },
        rollup: false,
    }
}

/// A target is a warning line, a limit is a stop line. Crossing the target must leave the ledger
/// runnable; crossing the hard limit must name scope, dimension, spent and limit.
#[test]
fn target_and_hard_limit_are_distinct_stop_lines() {
    let envelope = BudgetEnvelope {
        scope: BudgetScope::Run,
        target: BudgetLimits {
            total_tokens: Some(100),
            ..BudgetLimits::default()
        },
        limit: BudgetLimits {
            total_tokens: Some(300),
            ..BudgetLimits::default()
        },
    };
    let mut ledger = BudgetLedger::new(envelope);

    let over_target = ledger.record(&tokens("e-1", BudgetScope::Turn, 120));
    assert_eq!(over_target.charge, BudgetCharge::Charged);
    let warning = over_target.warning.expect("target crossing warns");
    assert_eq!(warning.dimension, BudgetDimension::TotalTokens);
    assert_eq!(warning.limit, 100);
    assert_eq!(warning.spent, 120);
    assert!(
        over_target.exhausted.is_none() && ledger.exhausted().is_none(),
        "a target is not a stop line: {over_target:?}"
    );

    let over_limit = ledger.record(&tokens("e-2", BudgetScope::Turn, 200));
    let breach = over_limit.exhausted.expect("hard limit stops the run");
    assert_eq!(breach.scope, BudgetScope::Run);
    assert_eq!(breach.dimension, BudgetDimension::TotalTokens);
    assert_eq!(breach.spent, 320);
    assert_eq!(breach.limit, 300);
    assert_eq!(ledger.exhausted(), Some(breach));
}

/// One warning per dimension, and never a stop when only a target is declared.
#[test]
fn target_without_hard_limit_warns_once_and_never_stops() {
    let envelope = BudgetEnvelope {
        scope: BudgetScope::Run,
        target: BudgetLimits {
            total_tokens: Some(50),
            ..BudgetLimits::default()
        },
        limit: BudgetLimits::default(),
    };
    let mut ledger = BudgetLedger::new(envelope);

    assert!(ledger
        .record(&tokens("e-1", BudgetScope::Turn, 40))
        .warning
        .is_none());
    assert!(
        ledger
            .record(&tokens("e-2", BudgetScope::Turn, 40))
            .warning
            .is_some(),
        "crossing the target must be visible"
    );
    for round in 0..3 {
        let outcome = ledger.record(&tokens(&format!("e-{}", 10 + round), BudgetScope::Turn, 40));
        assert!(
            outcome.warning.is_none(),
            "the target warns once, not once per call: round {round}"
        );
        assert!(
            outcome.exhausted.is_none(),
            "a target never stops execution"
        );
    }
    assert!(ledger.exhausted().is_none());
    assert_eq!(ledger.projection().warnings.len(), 1);
}

/// Attribution must survive into the ledger, and a re-reported child total must not be charged a
/// second time. The three ignore reasons are distinct so a caller can tell a duplicate from a
/// rollup from an out-of-scope event.
#[test]
fn usage_events_charge_once_without_double_counting_rollups() {
    let mut ledger = BudgetLedger::new(BudgetEnvelope {
        scope: BudgetScope::Run,
        target: BudgetLimits::default(),
        limit: BudgetLimits {
            total_tokens: Some(1_000),
            ..BudgetLimits::default()
        },
    });

    assert_eq!(
        ledger
            .record(&tokens("call-1", BudgetScope::Segment, 100))
            .charge,
        BudgetCharge::Charged
    );
    assert_eq!(
        ledger
            .record(&tokens("call-2", BudgetScope::Turn, 100))
            .charge,
        BudgetCharge::Charged
    );
    // The identical event id arriving twice (a retry of the reporting path, not of the model call).
    assert_eq!(
        ledger
            .record(&tokens("call-2", BudgetScope::Turn, 100))
            .charge,
        BudgetCharge::DuplicateIgnored
    );
    // A summary of already-charged children — `TurnEnded.usage` beside per-call events.
    let mut summary = tokens("turn-total", BudgetScope::Turn, 200);
    summary.rollup = true;
    assert_eq!(ledger.record(&summary).charge, BudgetCharge::RollupIgnored);
    // An ancestor's spend is not this scope's to charge.
    let mut turn_ledger = BudgetLedger::new(BudgetEnvelope {
        scope: BudgetScope::Turn,
        target: BudgetLimits::default(),
        limit: BudgetLimits::default(),
    });
    assert_eq!(
        turn_ledger
            .record(&tokens("run-wide", BudgetScope::Run, 100))
            .charge,
        BudgetCharge::OutOfScopeIgnored
    );
    assert_eq!(turn_ledger.spent().total_tokens, 0);

    assert_eq!(ledger.spent().total_tokens, 200);
    assert_eq!(ledger.spent().model_calls, 2);
    let projection = ledger.projection();
    assert_eq!(projection.scope, BudgetScope::Run);
    assert_eq!(projection.spent.total_tokens, 200);
    assert_eq!(
        projection.declared(BudgetDimension::TotalTokens),
        Some(1_000)
    );
}

/// Wall time is measured elapsed time, not an additive counter: folding two overlapping scopes'
/// elapsed windows takes the longer one instead of pretending the run lasted their sum.
#[test]
fn wall_time_folds_as_elapsed_not_as_a_sum() {
    let mut ledger = BudgetLedger::new(BudgetEnvelope {
        scope: BudgetScope::Run,
        target: BudgetLimits::default(),
        limit: BudgetLimits {
            wall_time_ms: Some(5_000),
            ..BudgetLimits::default()
        },
    });
    let elapsed = |id: &str, ms: u64| BudgetUsageEvent {
        event_id: id.into(),
        scope: BudgetScope::Run,
        attribution: attribution(),
        spend: BudgetSpend {
            wall_time_ms: ms,
            ..BudgetSpend::default()
        },
        rollup: false,
    };
    ledger.record(&elapsed("t-1", 3_000));
    ledger.record(&elapsed("t-2", 4_000));
    assert_eq!(ledger.spent().wall_time_ms, 4_000);
    assert!(ledger.exhausted().is_none());

    let breach = ledger
        .record(&elapsed("t-3", 6_000))
        .exhausted
        .expect("the deadline is a hard limit");
    assert_eq!(breach.dimension, BudgetDimension::WallTime);
    assert_eq!(breach.spent, 6_000);
    assert_eq!(breach.limit, 5_000);
}

/// One JSON contract feeds CLI and TUI projections; no surface recalculates totals.
#[test]
fn projection_round_trips_as_the_single_json_contract() {
    let mut ledger = BudgetLedger::new(BudgetEnvelope {
        scope: BudgetScope::Run,
        target: BudgetLimits {
            total_tokens: Some(100),
            ..BudgetLimits::default()
        },
        limit: BudgetLimits {
            total_tokens: Some(400),
            wall_time_ms: Some(60_000),
            ..BudgetLimits::default()
        },
    });
    ledger.record(&tokens("e-1", BudgetScope::Turn, 150));
    let projection = ledger.projection();
    let json = serde_json::to_value(&projection).expect("projection serializes");
    assert_eq!(json["scope"], "run");
    assert_eq!(json["spent"]["total_tokens"], 150);
    assert_eq!(json["limit"]["total_tokens"], 400);
    assert_eq!(json["target"]["total_tokens"], 100);
    assert_eq!(json["warnings"][0]["dimension"], "total_tokens");
    let decoded: flux_core::BudgetProjection =
        serde_json::from_value(json).expect("projection decodes");
    assert_eq!(decoded, projection);
    assert_eq!(decoded.declared(BudgetDimension::WallTime), Some(60_000));
    assert!(decoded.exhausted.is_none());
}

//! C-570 — a worker's upstream progress record is durable, acknowledged and authority-free.
//!
//! These tests are the contract for the report protocol itself: a long-running agent must be able
//! to say "the red test is established", ask for attention and yield at a declared safe checkpoint
//! without any of that becoming a status mutation.

use flux_core::{
    AgentAttentionRequest, AgentLoopBindingMetadata, AgentLoopRunnerKind, AgentReport,
    AgentReportIdentity, AgentReportLedger, AgentReportRejection, AgentReportState,
    AgentReportUnits, BudgetProjection, BudgetScope, BudgetSpend,
};

fn binding() -> AgentLoopBindingMetadata {
    AgentLoopBindingMetadata {
        schema: AgentLoopBindingMetadata::SCHEMA.into(),
        profile: "implementation".into(),
        revision: "1".into(),
        runner: AgentLoopRunnerKind::NativeFlux,
        source_ref: ".flux/loops/team-implementation.flux".into(),
        source_sha256: "a".repeat(64),
        entry_point: "work".into(),
        required_operations: vec!["edit".into(), "read".into()],
        required_runtime_features: vec!["native".into()],
    }
}

fn identity() -> AgentReportIdentity {
    AgentReportIdentity {
        agent_id: "writer-1".into(),
        session_id: "s_7".into(),
        parent_session: Some("s_main".into()),
        assignment: "flux/C-570".into(),
        loop_binding: binding(),
    }
}

fn report(sequence: u64, state: AgentReportState, summary: &str) -> AgentReport {
    AgentReport::new(
        format!("r_{sequence}"),
        sequence,
        identity(),
        "establish-evidence",
        state,
        summary,
    )
}

#[test]
fn a_blocking_child_records_red_test_established_and_is_acknowledged_idempotently() {
    let mut ledger = AgentReportLedger::new(identity());

    let first = ledger
        .admit(
            report(1, AgentReportState::Active, "red test established")
                .with_units(AgentReportUnits {
                    completed: 1,
                    total: Some(4),
                })
                .with_evidence(["test:flux-core::agent_report"]),
        )
        .expect("a bounded active report is admitted");
    assert!(!first.duplicate);
    assert_eq!(first.sequence, 1);
    assert!(!first.event_id.is_empty());

    // Retry of the exact same record after a transport failure must not create a second event.
    let retry = ledger
        .admit(report(1, AgentReportState::Active, "red test established"))
        .expect("a retried report is acknowledged, not rejected");
    assert!(retry.duplicate);
    assert_eq!(retry.event_id, first.event_id);
    assert_eq!(ledger.records().len(), 1, "{:?}", ledger.records());

    let record = &ledger.records()[0];
    assert_eq!(record.summary, "red test established");
    assert_eq!(record.identity.assignment, "flux/C-570");
    assert_eq!(record.identity.loop_binding.entry_point, "work");
    assert_eq!(
        record.units,
        Some(AgentReportUnits {
            completed: 1,
            total: Some(4)
        })
    );
}

#[test]
fn a_worker_cannot_report_itself_past_the_handoff_barrier() {
    let mut ledger = AgentReportLedger::new(identity());

    ledger
        .admit(report(
            1,
            AgentReportState::CandidateReady,
            "implementation frozen for review",
        ))
        .expect("candidate_ready is the worker's own freeze signal");

    let refused = ledger
        .admit(report(2, AgentReportState::HandoffReady, "I am done"))
        .expect_err("handoff_ready is host-derived only");
    assert!(
        matches!(refused, AgentReportRejection::HostDerivedState { .. }),
        "{refused:?}"
    );
    assert_eq!(ledger.records().len(), 1);
    assert_eq!(ledger.highest_sequence(), 1);
}

#[test]
fn a_report_cannot_widen_its_own_budget_projection() {
    let mut ledger = AgentReportLedger::new(identity());
    let host = BudgetProjection {
        scope: BudgetScope::Session,
        spent: BudgetSpend {
            total_tokens: 900,
            ..BudgetSpend::default()
        },
        ..BudgetProjection::default()
    };
    ledger.set_budget_projection(Some(host.clone()));

    let claimed = BudgetProjection {
        spent: BudgetSpend {
            total_tokens: 1,
            ..BudgetSpend::default()
        },
        ..BudgetProjection::default()
    };
    let admitted =
        report(1, AgentReportState::BudgetWarning, "approaching target").with_budget(Some(claimed));
    ledger.admit(admitted).expect("admitted");

    assert_eq!(
        ledger.records()[0].budget.as_ref(),
        Some(&host),
        "the host projection replaces whatever the worker claimed"
    );
}

#[test]
fn wrong_assignment_reused_and_out_of_order_reports_fail_closed() {
    let mut ledger = AgentReportLedger::new(identity());
    ledger
        .admit(report(2, AgentReportState::Active, "phase two"))
        .expect("admitted");

    let mut foreign = report(3, AgentReportState::Active, "other story");
    foreign.identity.assignment = "flux/C-571".into();
    assert!(matches!(
        ledger.admit(foreign).expect_err("wrong assignment"),
        AgentReportRejection::WrongAssignment { .. }
    ));

    let mut other_session = report(3, AgentReportState::Active, "other session");
    other_session.identity.session_id = "s_9".into();
    assert!(matches!(
        ledger.admit(other_session).expect_err("wrong session"),
        AgentReportRejection::WrongSession { .. }
    ));

    let mut switched = report(3, AgentReportState::Active, "another loop");
    switched.identity.loop_binding.source_sha256 = "b".repeat(64);
    assert!(matches!(
        ledger.admit(switched).expect_err("wrong loop binding"),
        AgentReportRejection::WrongLoopBinding { .. }
    ));

    let mut reused = report(2, AgentReportState::Active, "different body, used sequence");
    reused.report_id = "r_other".into();
    assert!(matches!(
        ledger.admit(reused).expect_err("sequence reuse"),
        AgentReportRejection::SequenceReused { .. }
    ));

    let mut stale = report(1, AgentReportState::Active, "stale");
    stale.report_id = "r_stale".into();
    assert!(matches!(
        ledger.admit(stale).expect_err("out of order"),
        AgentReportRejection::OutOfOrder { .. }
    ));

    assert_eq!(ledger.records().len(), 1);
    assert_eq!(ledger.highest_sequence(), 2);
}

#[test]
fn oversized_and_secret_bearing_payloads_never_reach_the_durable_record() {
    let mut ledger = AgentReportLedger::new(identity());

    let huge = report(
        1,
        AgentReportState::Active,
        &"x".repeat(AgentReport::MAX_SUMMARY_BYTES + 1),
    );
    let refused = ledger.admit(huge).expect_err("oversized summary");
    assert!(
        matches!(refused, AgentReportRejection::Oversized { .. }),
        "{refused:?}"
    );
    // The diagnostic names the field and the caps, never the payload.
    let rendered = refused.to_string();
    assert!(rendered.contains("summary"), "{rendered}");
    assert!(!rendered.contains("xxxx"), "{rendered}");

    let refused = ledger
        .admit(report(1, AgentReportState::Active, "ok").with_evidence(
            (0..AgentReport::MAX_EVIDENCE_REFS + 1).map(|i| format!("evidence:{i}")),
        ))
        .expect_err("too many evidence references");
    assert!(
        matches!(refused, AgentReportRejection::TooManyEvidenceRefs { .. }),
        "{refused:?}"
    );

    let mut bearing = report(1, AgentReportState::Waiting, "token sk-live-XYZ failed")
        .with_attention(AgentAttentionRequest {
            question: "rotate sk-live-XYZ?".into(),
            blocking: true,
        });
    bearing.redact_with(&|text| text.replace("sk-live-XYZ", "[redacted]"));
    ledger.admit(bearing).expect("redacted report is admitted");

    let record = &ledger.records()[0];
    assert!(!record.summary.contains("sk-live-XYZ"), "{record:?}");
    assert_eq!(
        record.attention.as_ref().map(|a| a.question.as_str()),
        Some("rotate [redacted]?")
    );
    assert!(record.attention.as_ref().is_some_and(|a| a.blocking));
}

#[test]
fn a_loop_yields_only_at_a_declared_checkpoint_and_resumes_the_exact_binding() {
    let mut ledger = AgentReportLedger::new(identity());
    ledger.declare_checkpoints(["after-red-test", "after-implementation"]);
    ledger
        .admit(report(1, AgentReportState::Active, "red test established"))
        .expect("admitted");

    let refused = ledger
        .yield_at("mid-edit", "half an edit applied")
        .expect_err("an undeclared checkpoint is not a safe boundary");
    assert!(
        matches!(refused, AgentReportRejection::UndeclaredCheckpoint { .. }),
        "{refused:?}"
    );

    let outcome = ledger
        .yield_at("after-red-test", "red test established, awaiting steering")
        .expect("a declared checkpoint yields");
    assert_eq!(outcome.cursor.checkpoint, "after-red-test");
    assert_eq!(outcome.cursor.sequence, 2, "the yield's own final report");
    assert_eq!(outcome.summary, "red test established, awaiting steering");

    // Yield is a partial terminal, not cancellation: the record survives and resume is exact.
    outcome
        .cursor
        .resume(&identity())
        .expect("the same assignment, session and loop binding resume");

    let mut widened = identity();
    widened.loop_binding.required_operations.push("bash".into());
    assert!(matches!(
        outcome
            .cursor
            .resume(&widened)
            .expect_err("a changed loop binding cannot resume the cursor"),
        AgentReportRejection::WrongLoopBinding { .. }
    ));

    let mut reassigned = identity();
    reassigned.assignment = "flux/C-571".into();
    assert!(matches!(
        outcome
            .cursor
            .resume(&reassigned)
            .expect_err("a different assignment cannot resume the cursor"),
        AgentReportRejection::WrongAssignment { .. }
    ));

    // The yield records its own final report, so a resumed loop continues above that sequence.
    assert_eq!(ledger.highest_sequence(), 2);
    assert!(matches!(
        ledger.records().last().map(|r| r.state),
        Some(AgentReportState::Waiting)
    ));
}

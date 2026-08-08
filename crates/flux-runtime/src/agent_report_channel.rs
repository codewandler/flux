//! C-570 — the host-owned channel a running agent authors progress reports through.
//!
//! [`flux_core::AgentReport`] is the record; this is the seam that produces one. A loop never
//! constructs its own identity or sequence and never reaches a sink directly: an
//! [`AgentReportReporter`] stamps the admitted identity, mints the next monotonic sequence, and
//! runs every model-authored string through the same [`Redactor`] the rest of the runtime uses.
//! That keeps a report a *statement*, not an authority — it cannot name another session, replay a
//! sequence, or carry an unredacted secret to a parent surface.
//!
//! The observation bridge mirrors [`crate::SpawnActivity`]'s: a report crosses the existing
//! correlated child boundary as an ordinary `AgentSink` observation, so no surface has to grow a
//! new callback and child thinking, prose and tool-result content stay exactly as private as they
//! already were.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;

use flux_core::{
    AgentAttentionRequest, AgentReport, AgentReportAck, AgentReportIdentity, AgentReportRejection,
    AgentReportState, AgentReportUnits,
};
use flux_evidence::{Observation, Phase};
use flux_secret::Redactor;

/// Observation kind carrying an [`AgentReport`] through the existing `AgentSink` extension point.
pub const KIND_AGENT_REPORT: &str = "agent.report";

/// Encode a report as the observation shape the correlated child boundary already relays.
pub fn agent_report_observation(report: &AgentReport) -> Observation {
    Observation::new(
        KIND_AGENT_REPORT,
        Phase::ToolFollowup,
        serde_json::to_value(report).unwrap_or(Value::Null),
    )
}

/// Observation kind carrying the bounded diagnostic left behind when a relay elides a report.
pub const KIND_AGENT_REPORT_REFUSED: &str = "agent.report.refused";

/// Encode why a report was elided. The diagnostic names the refusal, never the refused payload,
/// so an inadmissible record can be accounted for upstream without relaying it.
pub fn agent_report_refusal_observation(rejection: &AgentReportRejection) -> Observation {
    Observation::new(
        KIND_AGENT_REPORT_REFUSED,
        Phase::ToolFollowup,
        serde_json::json!({ "reason": rejection.to_string() }),
    )
}

/// Decode an agent-report observation; unrelated or malformed observations return `None`.
pub fn agent_report_from_observation(observation: &Observation) -> Option<AgentReport> {
    (observation.kind == KIND_AGENT_REPORT)
        .then(|| serde_json::from_value(observation.data.clone()).ok())
        .flatten()
}

/// Where admitted reports are persisted and acknowledged.
///
/// Implementations own durability and idempotency (see [`flux_core::AgentReportLedger`]) and must
/// not block: a report channel that stalls would backpressure the agent it is reporting on.
pub trait AgentReportSink: Send + Sync {
    fn report(&self, report: AgentReport) -> Result<AgentReportAck, AgentReportRejection>;
}

/// What a loop supplies for one report: the parts a worker legitimately knows.
///
/// Identity, sequence and report id are deliberately absent — the reporter stamps those.
#[derive(Debug, Clone)]
pub struct AgentReportDraft {
    pub phase: String,
    pub state: AgentReportState,
    pub summary: String,
    pub units: Option<AgentReportUnits>,
    pub evidence: Vec<String>,
    pub attention: Option<AgentAttentionRequest>,
}

impl AgentReportDraft {
    pub fn new(
        phase: impl Into<String>,
        state: AgentReportState,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            phase: phase.into(),
            state,
            summary: summary.into(),
            units: None,
            evidence: Vec::new(),
            attention: None,
        }
    }

    pub fn with_units(mut self, units: AgentReportUnits) -> Self {
        self.units = Some(units);
        self
    }

    pub fn with_evidence(mut self, refs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.evidence = refs.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_attention(mut self, request: AgentAttentionRequest) -> Self {
        self.attention = Some(request);
        self
    }
}

/// An owned, `'static` handle a loop uses to report its own progress upstream.
#[derive(Clone)]
pub struct AgentReportReporter {
    identity: AgentReportIdentity,
    redactor: Redactor,
    sink: Arc<dyn AgentReportSink>,
    next_sequence: Arc<AtomicU64>,
}

impl AgentReportReporter {
    /// Bind a reporter to the identity admitted at agent start.
    pub fn new(
        identity: AgentReportIdentity,
        redactor: Redactor,
        sink: Arc<dyn AgentReportSink>,
    ) -> Self {
        Self {
            identity,
            redactor,
            sink,
            next_sequence: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Continue an already-sequenced channel, for example after a cooperative yield and resume.
    pub fn resumed_from(mut self, highest_sequence: u64) -> Self {
        self.next_sequence = Arc::new(AtomicU64::new(highest_sequence.saturating_add(1)));
        self
    }

    pub fn identity(&self) -> &AgentReportIdentity {
        &self.identity
    }

    /// Stamp, redact and submit one draft, returning the sink's durable acknowledgement.
    pub fn report(&self, draft: AgentReportDraft) -> Result<AgentReportAck, AgentReportRejection> {
        let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
        let mut report = AgentReport::new(
            format!("{}:{sequence}", self.identity.session_id),
            sequence,
            self.identity.clone(),
            draft.phase,
            draft.state,
            draft.summary,
        )
        .with_evidence(draft.evidence);
        report.units = draft.units;
        report.attention = draft.attention;
        report.redact_with(&|text| self.redactor.redact(text));
        self.sink.report(report)
    }
}

impl std::fmt::Debug for AgentReportReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentReportReporter")
            .field("identity", &self.identity)
            .field("next_sequence", &self.next_sequence)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::{AgentLoopBindingMetadata, AgentLoopRunnerKind, AgentReportLedger};
    use std::sync::Mutex;

    fn identity() -> AgentReportIdentity {
        AgentReportIdentity {
            agent_id: "writer-1".into(),
            session_id: "s_child".into(),
            parent_session: Some("s_parent".into()),
            assignment: "flux/C-570".into(),
            loop_binding: AgentLoopBindingMetadata {
                schema: AgentLoopBindingMetadata::SCHEMA.into(),
                profile: "implementation".into(),
                revision: "1".into(),
                runner: AgentLoopRunnerKind::NativeFlux,
                source_ref: "profile:implementation@1".into(),
                source_sha256: "a".repeat(64),
                entry_point: "work".into(),
                required_operations: vec!["read".into()],
                required_runtime_features: vec![],
            },
        }
    }

    struct LedgerSink(Mutex<AgentReportLedger>);

    impl AgentReportSink for LedgerSink {
        fn report(&self, report: AgentReport) -> Result<AgentReportAck, AgentReportRejection> {
            self.0.lock().unwrap().admit(report)
        }
    }

    #[test]
    fn the_reporter_owns_identity_and_sequence_and_redacts_before_the_sink() {
        let sink = Arc::new(LedgerSink(Mutex::new(AgentReportLedger::new(identity()))));
        let redactor = Redactor::new();
        redactor.add_secret("canary boundary value");
        let reporter = AgentReportReporter::new(identity(), redactor, sink.clone());

        let first = reporter
            .report(AgentReportDraft::new(
                "establish-evidence",
                AgentReportState::Active,
                "red test established using canary boundary value",
            ))
            .expect("admitted");
        let second = reporter
            .report(AgentReportDraft::new(
                "implement",
                AgentReportState::Active,
                "contract implemented",
            ))
            .expect("admitted");

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert!(!first.duplicate && !second.duplicate);

        let ledger = sink.0.lock().unwrap();
        assert_eq!(ledger.records().len(), 2);
        assert!(
            !ledger.records()[0]
                .summary
                .contains("canary boundary value"),
            "{:?}",
            ledger.records()[0].summary
        );
        assert_eq!(ledger.records()[0].identity.session_id, "s_child");
    }

    #[test]
    fn a_report_round_trips_through_the_correlated_observation_bridge() {
        let report = AgentReport::new(
            "r_1",
            1,
            identity(),
            "validate",
            AgentReportState::CandidateReady,
            "candidate frozen",
        );
        let observation = agent_report_observation(&report);
        assert_eq!(observation.kind, KIND_AGENT_REPORT);
        assert_eq!(
            agent_report_from_observation(&observation).expect("decodes"),
            report
        );
        assert!(
            agent_report_from_observation(&Observation::new(
                "tool.progress",
                Phase::ToolFollowup,
                Value::Null
            ))
            .is_none(),
            "an unrelated observation is not a report"
        );
    }
}

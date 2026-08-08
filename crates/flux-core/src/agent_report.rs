//! C-570 — the durable, acknowledged progress record a running agent may author, and the
//! cooperative yield cursor it may leave behind at a declared safe checkpoint.
//!
//! `SpawnActivity` already tells a host that a child is planning, calling a tool or finished. It is
//! synchronous, live-only and host-derived: a worker cannot use it to say "the red test is
//! established", ask for a decision, or checkpoint a long assignment. [`AgentReport`] is the
//! child-authored counterpart, and it is deliberately powerless. It carries no Board transition, no
//! success bit, no Fleet membership and no capability or budget grant — the fields simply do not
//! exist, and the one state a host derives (`handoff_ready`) is refused when a worker claims it.
//!
//! Everything here is pure. Persistence, redaction and transport belong to the owning host; this
//! module owns the shape, the bounds and the admission rules so every surface enforces the same
//! ones.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{AgentLoopBindingMetadata, BudgetProjection};

/// Lifecycle state a report may carry.
///
/// `candidate_ready` means the worker froze its implementation for review and reflection.
/// `handoff_ready` is the barrier past it: a host derives that state only after both mandatory
/// receipts settle successfully, so [`AgentReport::validate`] refuses a worker that claims it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentReportState {
    /// Working; the phase says where in its loop.
    Active,
    /// Blocked on an answer, a decision or an external settlement.
    Waiting,
    /// Implementation frozen for review and reflection (C-572/C-587).
    CandidateReady,
    /// Host-derived only: review passed and the reflection receipt is stored.
    HandoffReady,
    /// Approaching a declared target, not yet over a hard limit.
    BudgetWarning,
}

impl AgentReportState {
    /// The stable wire/UI name of this state.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentReportState::Active => "active",
            AgentReportState::Waiting => "waiting",
            AgentReportState::CandidateReady => "candidate_ready",
            AgentReportState::HandoffReady => "handoff_ready",
            AgentReportState::BudgetWarning => "budget_warning",
        }
    }

    /// Whether only a host may put an agent into this state.
    pub fn is_host_derived(self) -> bool {
        matches!(self, AgentReportState::HandoffReady)
    }
}

impl fmt::Display for AgentReportState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who is reporting, under which assignment and behavior harness.
///
/// The loop binding is C-569's source-free receipt identity, so a report — and a yield cursor
/// resumed from one — names the exact harness it ran under without copying loop source anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReportIdentity {
    pub agent_id: String,
    pub session_id: String,
    /// The correlated parent session, when this agent is a child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    /// The assignment this agent was admitted for, for example a namespaced Board ref.
    pub assignment: String,
    pub loop_binding: AgentLoopBindingMetadata,
}

impl AgentReportIdentity {
    /// Refuse anything but the exact assignment, session and loop binding already admitted.
    ///
    /// `self` is the admitted identity; `other` is what arrived. Loop-binding comparison uses
    /// [`AgentLoopBindingMetadata::equivalent_to`], so legacy set order is equivalent while a
    /// widened operation set, a changed digest or another entry point is not.
    pub fn ensure_matches(&self, other: &Self) -> Result<(), AgentReportRejection> {
        if self.assignment != other.assignment {
            return Err(AgentReportRejection::WrongAssignment {
                expected: bounded(&self.assignment),
                found: bounded(&other.assignment),
            });
        }
        if self.session_id != other.session_id {
            return Err(AgentReportRejection::WrongSession {
                expected: bounded(&self.session_id),
                found: bounded(&other.session_id),
            });
        }
        if !self.loop_binding.equivalent_to(&other.loop_binding) {
            return Err(AgentReportRejection::WrongLoopBinding {
                expected: binding_fingerprint(&self.loop_binding),
                found: binding_fingerprint(&other.loop_binding),
            });
        }
        Ok(())
    }
}

/// Optional progress counter. `total` stays `None` when the loop honestly cannot know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReportUnits {
    pub completed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// A bounded request for a decision or an answer. It asks; it never authorizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAttentionRequest {
    pub question: String,
    /// Whether the loop is parked on the answer rather than continuing.
    #[serde(default)]
    pub blocking: bool,
}

/// One child-authored progress record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    pub schema: String,
    /// Stable id of this record; a retry of the same record repeats it and is acknowledged once.
    pub report_id: String,
    /// Monotonic per-session sequence, starting at 1.
    pub sequence: u64,
    pub identity: AgentReportIdentity,
    /// Loop-defined phase name, for example `establish-evidence`.
    pub phase: String,
    pub state: AgentReportState,
    /// Bounded, already-redacted prose. Never command output, a diff or a transcript.
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<AgentReportUnits>,
    /// References to evidence held elsewhere — never the evidence itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<AgentAttentionRequest>,
    /// The budget projection at the moment of the report. A host stamps its own on admission; a
    /// worker's copy is a display value, never an authority to spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetProjection>,
}

impl AgentReport {
    pub const SCHEMA: &'static str = "flux.agent-report/v1";
    pub const MAX_REPORT_ID_BYTES: usize = 128;
    pub const MAX_PHASE_BYTES: usize = 64;
    pub const MAX_SUMMARY_BYTES: usize = 2048;
    pub const MAX_EVIDENCE_REFS: usize = 16;
    pub const MAX_EVIDENCE_REF_BYTES: usize = 256;
    pub const MAX_ATTENTION_BYTES: usize = 512;

    pub fn new(
        report_id: impl Into<String>,
        sequence: u64,
        identity: AgentReportIdentity,
        phase: impl Into<String>,
        state: AgentReportState,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            schema: Self::SCHEMA.to_string(),
            report_id: report_id.into(),
            sequence,
            identity,
            phase: phase.into(),
            state,
            summary: summary.into(),
            units: None,
            evidence: Vec::new(),
            attention: None,
            budget: None,
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

    pub fn with_budget(mut self, projection: Option<BudgetProjection>) -> Self {
        self.budget = projection;
        self
    }

    /// Apply the host's redactor to every model-authored string before the record is persisted or
    /// relayed. Structured identities are host-assigned and deliberately left alone.
    pub fn redact_with(&mut self, redact: &dyn Fn(&str) -> String) {
        self.phase = redact(&self.phase);
        self.summary = redact(&self.summary);
        for reference in &mut self.evidence {
            *reference = redact(reference);
        }
        if let Some(attention) = &mut self.attention {
            attention.question = redact(&attention.question);
        }
    }

    /// Admit this record for relay across the correlated child boundary owned by `session_id`.
    ///
    /// A relay knows one thing the record cannot assert for itself: which child session actually
    /// authored the observation it arrived on. A report naming another session is not this child's
    /// to speak for, so it is refused here before it can reach a parent surface.
    pub fn admit_from_session(&self, session_id: &str) -> Result<(), AgentReportRejection> {
        if self.identity.session_id != session_id {
            return Err(AgentReportRejection::WrongSession {
                expected: bounded(session_id),
                found: bounded(&self.identity.session_id),
            });
        }
        self.validate()
    }

    /// Check schema, bounds and authority. Diagnostics name the field and the cap, never the
    /// payload, so a refusal can be logged without leaking what was refused.
    pub fn validate(&self) -> Result<(), AgentReportRejection> {
        if self.schema != Self::SCHEMA {
            return Err(AgentReportRejection::UnknownSchema {
                schema: bounded(&self.schema),
                expected: Self::SCHEMA,
            });
        }
        if self.state.is_host_derived() {
            return Err(AgentReportRejection::HostDerivedState { state: self.state });
        }
        check_text("report_id", &self.report_id, Self::MAX_REPORT_ID_BYTES)?;
        check_text("phase", &self.phase, Self::MAX_PHASE_BYTES)?;
        check_text("summary", &self.summary, Self::MAX_SUMMARY_BYTES)?;
        if self.sequence == 0 {
            return Err(AgentReportRejection::OutOfOrder {
                expected_above: 0,
                found: 0,
            });
        }
        if self.evidence.len() > Self::MAX_EVIDENCE_REFS {
            return Err(AgentReportRejection::TooManyEvidenceRefs {
                count: self.evidence.len(),
                limit: Self::MAX_EVIDENCE_REFS,
            });
        }
        for reference in &self.evidence {
            check_text("evidence", reference, Self::MAX_EVIDENCE_REF_BYTES)?;
            if reference.contains('\n') {
                return Err(AgentReportRejection::EmbeddedContent { field: "evidence" });
            }
        }
        if let Some(attention) = &self.attention {
            check_text("attention", &attention.question, Self::MAX_ATTENTION_BYTES)?;
        }
        Ok(())
    }
}

/// What persistence returns: a durable event id plus whether this delivery was already recorded.
///
/// The id is derived from session, sequence and report id, so a retry — in this process or after a
/// restart — recomputes the same acknowledgement instead of creating a second durable event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReportAck {
    pub event_id: String,
    pub sequence: u64,
    /// True when this exact record was already admitted.
    pub duplicate: bool,
}

/// The durable cursor a cooperative yield leaves behind.
///
/// It is not cancellation and not an operator pause (A-140/A-141): the agent keeps its assignment,
/// session, loop binding and settled usage, and a resume must present the same identity. The cursor
/// carries no capability set and no limits, so resuming through it can never widen an admitted
/// envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentYieldCursor {
    pub schema: String,
    /// The declared safe checkpoint the loop stopped at.
    pub checkpoint: String,
    /// The sequence of the yield's own final report; a resumed turn continues above it.
    pub sequence: u64,
    pub identity: AgentReportIdentity,
    /// Usage settled at the checkpoint, so a resume starts from this projection, not from zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetProjection>,
}

impl AgentYieldCursor {
    pub const SCHEMA: &'static str = "flux.agent-yield-cursor/v1";

    /// Admit a resume only for the exact assignment, session and loop binding that yielded.
    pub fn resume(&self, identity: &AgentReportIdentity) -> Result<(), AgentReportRejection> {
        self.identity.ensure_matches(identity)
    }
}

/// The typed partial terminal of a cooperatively yielded turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentYieldOutcome {
    pub cursor: AgentYieldCursor,
    /// Bounded, already-redacted summary of what the turn completed before yielding.
    pub summary: String,
    /// Acknowledgement of the yield's own final report.
    pub ack: AgentReportAck,
    /// Usage settled at the checkpoint.
    pub settled: Option<BudgetProjection>,
}

/// Why a report was not admitted. Every variant is a bounded diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentReportRejection {
    #[error("unknown agent-report schema `{schema}`, expected `{expected}`")]
    UnknownSchema {
        schema: String,
        expected: &'static str,
    },
    #[error("agent report field `{field}` is empty")]
    EmptyField { field: &'static str },
    #[error("agent report field `{field}` is {bytes} bytes, over the {limit}-byte cap")]
    Oversized {
        field: &'static str,
        bytes: usize,
        limit: usize,
    },
    #[error("agent report carries {count} evidence references, over the cap of {limit}")]
    TooManyEvidenceRefs { count: usize, limit: usize },
    #[error("agent report field `{field}` carries embedded content; it must be a reference")]
    EmbeddedContent { field: &'static str },
    #[error("agent report state `{state}` is host-derived and cannot be claimed by a worker")]
    HostDerivedState { state: AgentReportState },
    #[error("agent report is for assignment `{found}`, not the admitted `{expected}`")]
    WrongAssignment { expected: String, found: String },
    #[error("agent report is for session `{found}`, not the admitted `{expected}`")]
    WrongSession { expected: String, found: String },
    #[error("agent report is for loop binding `{found}`, not the admitted `{expected}`")]
    WrongLoopBinding { expected: String, found: String },
    #[error("agent report sequence {found} is not above the admitted {expected_above}")]
    OutOfOrder { expected_above: u64, found: u64 },
    #[error("agent report sequence {sequence} was already admitted as `{admitted}`")]
    SequenceReused { sequence: u64, admitted: String },
    #[error("`{checkpoint}` is not a declared safe checkpoint of this loop")]
    UndeclaredCheckpoint { checkpoint: String },
}

/// The host side of one agent's report channel: admission, idempotency and cooperative yield.
///
/// Admission is synchronous and total — it validates, deduplicates and records, and never waits on
/// a consumer — so reporting can fail closed or elide, but cannot backpressure the agent it is
/// reporting on.
#[derive(Debug, Clone)]
pub struct AgentReportLedger {
    identity: AgentReportIdentity,
    checkpoints: BTreeSet<String>,
    budget: Option<BudgetProjection>,
    highest_sequence: u64,
    by_sequence: BTreeMap<u64, String>,
    acks: BTreeMap<String, AgentReportAck>,
    records: Vec<AgentReport>,
}

impl AgentReportLedger {
    /// Bind a ledger to the identity admitted at agent start.
    pub fn new(identity: AgentReportIdentity) -> Self {
        Self {
            identity,
            checkpoints: BTreeSet::new(),
            budget: None,
            highest_sequence: 0,
            by_sequence: BTreeMap::new(),
            acks: BTreeMap::new(),
            records: Vec::new(),
        }
    }

    /// Rebind after a restart, keeping the sequence floor already persisted upstream.
    pub fn resumed_from(identity: AgentReportIdentity, highest_sequence: u64) -> Self {
        let mut ledger = Self::new(identity);
        ledger.highest_sequence = highest_sequence;
        ledger
    }

    /// Declare the checkpoints at which this loop may cooperatively yield.
    pub fn declare_checkpoints(&mut self, names: impl IntoIterator<Item = impl Into<String>>) {
        self.checkpoints.extend(names.into_iter().map(Into::into));
    }

    /// Set the host's authoritative budget projection; it is stamped onto every admitted record.
    pub fn set_budget_projection(&mut self, projection: Option<BudgetProjection>) {
        self.budget = projection;
    }

    pub fn identity(&self) -> &AgentReportIdentity {
        &self.identity
    }

    pub fn records(&self) -> &[AgentReport] {
        &self.records
    }

    pub fn highest_sequence(&self) -> u64 {
        self.highest_sequence
    }

    /// Validate, deduplicate and record one report, returning its acknowledgement.
    pub fn admit(&mut self, report: AgentReport) -> Result<AgentReportAck, AgentReportRejection> {
        report.validate()?;
        self.identity.ensure_matches(&report.identity)?;

        if let Some(ack) = self.acks.get(&report.report_id) {
            let mut ack = ack.clone();
            ack.duplicate = true;
            return Ok(ack);
        }
        if let Some(admitted) = self.by_sequence.get(&report.sequence) {
            return Err(AgentReportRejection::SequenceReused {
                sequence: report.sequence,
                admitted: bounded(admitted),
            });
        }
        if report.sequence <= self.highest_sequence {
            return Err(AgentReportRejection::OutOfOrder {
                expected_above: self.highest_sequence,
                found: report.sequence,
            });
        }

        let ack = AgentReportAck {
            event_id: format!(
                "{}#{}:{}",
                self.identity.session_id, report.sequence, report.report_id
            ),
            sequence: report.sequence,
            duplicate: false,
        };
        let mut record = report;
        // Operational state is the host's projection over admitted state plus verified receipts,
        // so whatever the worker believed about its budget is replaced, not merged.
        record.budget = self.budget.clone();
        self.highest_sequence = record.sequence;
        self.by_sequence
            .insert(record.sequence, record.report_id.clone());
        self.acks.insert(record.report_id.clone(), ack.clone());
        self.records.push(record);
        Ok(ack)
    }

    /// Yield at a declared safe checkpoint: record a final `waiting` report and return the durable
    /// cursor a later resume must present.
    pub fn yield_at(
        &mut self,
        checkpoint: &str,
        summary: impl Into<String>,
    ) -> Result<AgentYieldOutcome, AgentReportRejection> {
        if !self.checkpoints.contains(checkpoint) {
            return Err(AgentReportRejection::UndeclaredCheckpoint {
                checkpoint: bounded(checkpoint),
            });
        }
        let summary = summary.into();
        let sequence = self.highest_sequence.saturating_add(1);
        let ack = self.admit(AgentReport::new(
            format!("yield:{sequence}"),
            sequence,
            self.identity.clone(),
            "yield",
            AgentReportState::Waiting,
            summary.clone(),
        ))?;
        Ok(AgentYieldOutcome {
            cursor: AgentYieldCursor {
                schema: AgentYieldCursor::SCHEMA.to_string(),
                checkpoint: checkpoint.to_string(),
                sequence: ack.sequence,
                identity: self.identity.clone(),
                budget: self.budget.clone(),
            },
            summary,
            ack,
            settled: self.budget.clone(),
        })
    }
}

/// Reject an empty or oversized model-authored field without echoing its bytes.
fn check_text(field: &'static str, value: &str, limit: usize) -> Result<(), AgentReportRejection> {
    if value.is_empty() {
        return Err(AgentReportRejection::EmptyField { field });
    }
    if value.len() > limit {
        return Err(AgentReportRejection::Oversized {
            field,
            bytes: value.len(),
            limit,
        });
    }
    Ok(())
}

/// Cap a value that appears in a diagnostic, on a character boundary.
fn bounded(value: &str) -> String {
    const MAX: usize = 120;
    if value.len() <= MAX {
        return value.to_string();
    }
    let end = (0..=MAX)
        .rev()
        .find(|i| value.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}…", &value[..end])
}

/// A compact, source-free rendering of a loop binding for a refusal message.
fn binding_fingerprint(binding: &AgentLoopBindingMetadata) -> String {
    let canonical = binding.canonicalized();
    let digest = &canonical.source_sha256[..canonical.source_sha256.len().min(12)];
    bounded(&format!(
        "{}@{}#{} entry={} ops=[{}] features=[{}]",
        canonical.profile,
        canonical.revision,
        digest,
        canonical.entry_point,
        canonical.required_operations.join(","),
        canonical.required_runtime_features.join(","),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentLoopRunnerKind;

    fn identity() -> AgentReportIdentity {
        AgentReportIdentity {
            agent_id: "writer-1".into(),
            session_id: "s_1".into(),
            parent_session: None,
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

    #[test]
    fn a_report_survives_a_serde_round_trip_without_widening() {
        let report = AgentReport::new(
            "r_1",
            1,
            identity(),
            "implement",
            AgentReportState::Active,
            "half done",
        )
        .with_units(AgentReportUnits {
            completed: 2,
            total: None,
        });
        let text = serde_json::to_string(&report).expect("serialize");
        assert!(!text.contains("budget"), "{text}");
        let back: AgentReport = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, report);
    }

    #[test]
    fn evidence_may_not_smuggle_command_output() {
        let report = AgentReport::new(
            "r_1",
            1,
            identity(),
            "validate",
            AgentReportState::Active,
            "targeted tests pass",
        )
        .with_evidence(["test: ok\nrunning 3 tests"]);
        assert!(matches!(
            report.validate().expect_err("embedded output"),
            AgentReportRejection::EmbeddedContent { field: "evidence" }
        ));
    }
}

//! `RiskApprover` — the risk-tier confirm gate between [`AllowApprover`](crate::AllowApprover)
//! (everything) and [`DenyApprover`](crate::DenyApprover) (nothing): **Allow < Risk < Deny**.
//!
//! It permits reads freely, auto-approves writes below a configurable [`Risk`] threshold, and
//! requires an explicit consent marker in the call's permission subjects for writes at/above that
//! threshold. This is an **approval policy layered on top of** the mandatory authorization envelope
//! (`Executor::dispatch`'s permission-rule + approval-prompt path) — it does NOT replace it. A tool
//! this approver never saw (because it wasn't in the registry snapshot at construction) still passes
//! through every other envelope check; it merely isn't gated by risk tier here.

use std::collections::HashMap;

use async_trait::async_trait;
use flux_spec::{Effect, IntentSet, Risk};

use crate::{ApprovalChoice, Approver, PlanApprovalRequest, ToolRegistry};

/// The default permission-subject marker a gated write's call must carry to be approved.
pub const DEFAULT_CONSENT_MARKER: &str = "user-confirmed";

/// The risk-aware [`Approver`]: keyed on a `name -> (has write effect, risk tier)` snapshot taken
/// from a [`ToolRegistry`] at construction time, so the policy reads exactly what the guarded tools
/// advertise via their [`ToolSpec`](flux_spec::ToolSpec). Tool names outside that snapshot — because
/// they weren't registered yet, or belong to a different surface entirely (e.g. the `"run plan"`
/// pseudo-name) — pass through unconditionally: this approver only governs the surface it was built
/// over, and the envelope still guards everything else.
pub struct RiskApprover {
    marker: String,
    threshold: Risk,
    specs: HashMap<String, (bool, Risk)>,
}

impl RiskApprover {
    /// Snapshot `(writes, risk)` for every tool currently in `registry`. Tools registered *after*
    /// construction are not in the snapshot and will be treated as unknown (pass-through) — rebuild
    /// the approver if the registry's surface changes.
    ///
    /// Defaults: consent marker [`DEFAULT_CONSENT_MARKER`] (`"user-confirmed"`), threshold
    /// [`Risk::High`] (gates `High` and `Destructive`; `Low`/`Medium` writes auto-approve). Override
    /// either with [`with_marker`](Self::with_marker) / [`with_threshold`](Self::with_threshold).
    pub fn new(registry: &ToolRegistry) -> Self {
        let specs = registry
            .names()
            .iter()
            .filter_map(|name| registry.get(name))
            .map(|tool| {
                let spec = tool.spec();
                (
                    spec.name,
                    (spec.effects.contains(&Effect::Write), spec.risk),
                )
            })
            .collect();
        Self {
            marker: DEFAULT_CONSENT_MARKER.to_string(),
            threshold: Risk::High,
            specs,
        }
    }

    /// Override the consent marker subject (default [`DEFAULT_CONSENT_MARKER`]).
    pub fn with_marker(mut self, marker: impl Into<String>) -> Self {
        self.marker = marker.into();
        self
    }

    /// Override the risk threshold at/above which writes gate (default [`Risk::High`]).
    pub fn with_threshold(mut self, threshold: Risk) -> Self {
        self.threshold = threshold;
        self
    }

    /// Whether a call with this tool's snapshot needs the consent marker.
    fn gates(&self, writes: bool, risk: Risk) -> bool {
        writes && risk >= self.threshold
    }
}

#[async_trait]
impl Approver for RiskApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        match self.specs.get(tool) {
            // A read — or a write below the gate threshold — never gates.
            Some((writes, risk)) if !self.gates(*writes, *risk) => ApprovalChoice::Allow,
            // A gated write executes only with the explicit consent marker on the invocation.
            Some(_) if subjects.iter().any(|s| s == &self.marker) => ApprovalChoice::Allow,
            Some(_) => ApprovalChoice::Deny,
            // Not part of the guarded snapshot (unregistered at construction, or a plan-level/
            // built-in pseudo-name) — pass through.
            None => ApprovalChoice::Allow,
        }
    }

    /// **Fail closed at the plan gate.** Approving a whole plan enters flux's approved scope, where
    /// the per-op gate is skipped — and plan approval cannot see per-call consent (the marker rides
    /// `permission_subjects`, which the plan preview doesn't collect). So a plan naming any gated
    /// write is denied outright rather than silently bypassing the per-op gate; plans of reads and
    /// ungated writes pass.
    async fn request_plan(&self, plan: &PlanApprovalRequest) -> ApprovalChoice {
        let has_gated_write = plan.ops.iter().any(
            |op| matches!(self.specs.get(op), Some((writes, risk)) if self.gates(*writes, *risk)),
        );
        if has_gated_write {
            ApprovalChoice::Deny
        } else {
            ApprovalChoice::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tool, ToolContext, ToolResult};
    use flux_core::Result as FluxResult;
    use flux_spec::ToolSpec;
    use serde_json::{json, Value};

    struct Labeled(&'static str, Vec<Effect>, Risk);

    #[async_trait]
    impl Tool for Labeled {
        fn spec(&self) -> ToolSpec {
            ToolSpec::read_only(self.0, "t", json!({"type": "object"}))
                .with_effects(self.1.clone())
                .with_risk(self.2)
        }

        async fn execute(&self, _ctx: &ToolContext, _p: Value) -> FluxResult<ToolResult> {
            Ok(ToolResult::ok("ran"))
        }
    }

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(std::sync::Arc::new(Labeled(
            "read_op",
            vec![Effect::Read],
            Risk::Low,
        )));
        registry.register(std::sync::Arc::new(Labeled(
            "low_write",
            vec![Effect::Write],
            Risk::Low,
        )));
        registry.register(std::sync::Arc::new(Labeled(
            "high_write",
            vec![Effect::Write],
            Risk::High,
        )));
        registry
    }

    fn confirmed() -> Vec<String> {
        vec![DEFAULT_CONSENT_MARKER.to_string()]
    }

    #[tokio::test]
    async fn a_read_is_never_gated() {
        let approver = RiskApprover::new(&registry());
        assert!(matches!(
            approver.request("read_op", &[], &IntentSet::new()).await,
            ApprovalChoice::Allow
        ));
    }

    #[tokio::test]
    async fn a_write_below_the_threshold_auto_approves() {
        let approver = RiskApprover::new(&registry());
        assert!(matches!(
            approver.request("low_write", &[], &IntentSet::new()).await,
            ApprovalChoice::Allow
        ));
    }

    #[tokio::test]
    async fn a_write_at_the_threshold_without_the_marker_is_denied() {
        let approver = RiskApprover::new(&registry());
        assert!(matches!(
            approver.request("high_write", &[], &IntentSet::new()).await,
            ApprovalChoice::Deny
        ));
    }

    #[tokio::test]
    async fn a_write_at_the_threshold_with_the_marker_is_approved() {
        let approver = RiskApprover::new(&registry());
        assert!(matches!(
            approver
                .request("high_write", &confirmed(), &IntentSet::new())
                .await,
            ApprovalChoice::Allow
        ));
    }

    #[tokio::test]
    async fn an_unknown_tool_name_passes_through() {
        let approver = RiskApprover::new(&registry());
        assert!(matches!(
            approver.request("run plan", &[], &IntentSet::new()).await,
            ApprovalChoice::Allow
        ));
    }

    #[tokio::test]
    async fn a_plan_containing_a_gated_write_is_denied_but_ungated_plans_pass() {
        let approver = RiskApprover::new(&registry());
        let gated = PlanApprovalRequest {
            ops: vec!["read_op".into(), "high_write".into()],
            ..Default::default()
        };
        assert!(matches!(
            approver.request_plan(&gated).await,
            ApprovalChoice::Deny
        ));

        let ungated = PlanApprovalRequest {
            ops: vec!["read_op".into(), "low_write".into()],
            ..Default::default()
        };
        assert!(matches!(
            approver.request_plan(&ungated).await,
            ApprovalChoice::Allow
        ));
    }

    /// The snapshot is taken at construction — a tool registered afterward is unknown to this
    /// approver and passes through (the envelope still guards it; this policy simply never saw it).
    #[tokio::test]
    async fn a_tool_added_after_construction_is_unknown_and_passes_through() {
        let mut reg = registry();
        let approver = RiskApprover::new(&reg);
        reg.register(std::sync::Arc::new(Labeled(
            "late_write",
            vec![Effect::Write],
            Risk::High,
        )));
        assert!(matches!(
            approver.request("late_write", &[], &IntentSet::new()).await,
            ApprovalChoice::Allow
        ));
    }

    /// Overriding the marker/threshold changes the gate: a lower threshold gates `Medium` writes
    /// too, and only the configured marker (not the default) satisfies consent.
    #[tokio::test]
    async fn marker_and_threshold_are_configurable() {
        let mut reg = ToolRegistry::new();
        reg.register(std::sync::Arc::new(Labeled(
            "medium_write",
            vec![Effect::Write],
            Risk::Medium,
        )));
        let approver = RiskApprover::new(&reg)
            .with_marker("ok-boss")
            .with_threshold(Risk::Medium);

        // Default marker no longer satisfies consent.
        assert!(matches!(
            approver
                .request("medium_write", &confirmed(), &IntentSet::new())
                .await,
            ApprovalChoice::Deny
        ));
        // The configured marker does.
        assert!(matches!(
            approver
                .request("medium_write", &["ok-boss".to_string()], &IntentSet::new())
                .await,
            ApprovalChoice::Allow
        ));
    }
}

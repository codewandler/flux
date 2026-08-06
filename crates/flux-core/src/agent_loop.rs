//! Bounded identity for the behavior harness admitted at an agent start.
//!
//! The executable loop source deliberately does not live in this contract. Start, status and
//! terminal receipts may carry this value without copying prompts or authored programs into every
//! projection; the owning runtime keeps (or reconstructs) the source behind `source_ref` and
//! verifies `source_sha256` before a provider call.

use serde::{Deserialize, Serialize};

/// The runtime family that can execute an admitted agent loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentLoopRunnerKind {
    /// A parsed Flux-Lang program executed by Flux's native loop host.
    NativeFlux,
    /// A named behavior profile implemented by a non-native task-agent backend.
    BackendProfile,
}

/// Source-free, receipt-safe identity of one resolved agent-loop binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLoopBindingMetadata {
    pub schema: String,
    /// Operator-meaningful logical profile id, for example `adaptive` or `implementation`.
    pub profile: String,
    /// Revision in the profile's own namespace. A digest remains authoritative for its bytes.
    pub revision: String,
    pub runner: AgentLoopRunnerKind,
    /// Immutable-by-digest source locator; never the source itself.
    pub source_ref: String,
    pub source_sha256: String,
    /// Named Flux flow or backend-defined lifecycle entry point.
    pub entry_point: String,
    /// Exact operations the loop program itself calls, sorted and deduplicated.
    #[serde(default)]
    pub required_operations: Vec<String>,
    /// Runtime contracts required to execute the loop, sorted and deduplicated.
    #[serde(default)]
    pub required_runtime_features: Vec<String>,
}

impl AgentLoopBindingMetadata {
    pub const SCHEMA: &'static str = "flux.agent-loop-binding/v1";

    /// Return the canonical receipt form. Operations and runtime features are set-valued contract
    /// fields; older receipts may preserve insertion order, so reconstruction normalizes them
    /// before deciding whether a live session is being switched.
    pub fn canonicalized(&self) -> Self {
        let mut canonical = self.clone();
        canonical.required_operations.sort();
        canonical.required_operations.dedup();
        canonical.required_runtime_features.sort();
        canonical.required_runtime_features.dedup();
        canonical
    }

    /// Compare binding identity while honoring the set semantics of the two required-* fields.
    pub fn equivalent_to(&self, other: &Self) -> bool {
        self.canonicalized() == other.canonicalized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_equivalence_ignores_legacy_set_order_and_duplicates_only() {
        let first = AgentLoopBindingMetadata {
            schema: AgentLoopBindingMetadata::SCHEMA.into(),
            profile: "implementation".into(),
            revision: "1".into(),
            runner: AgentLoopRunnerKind::NativeFlux,
            source_ref: "profile:implementation@1".into(),
            source_sha256: "a".repeat(64),
            entry_point: "work".into(),
            required_operations: vec!["read".into(), "edit".into(), "read".into()],
            required_runtime_features: vec!["native".into(), "ai".into()],
        };
        let mut reordered = first.canonicalized();
        reordered.required_operations.reverse();
        reordered.required_runtime_features.reverse();

        assert!(first.equivalent_to(&reordered));
        reordered.source_sha256 = "b".repeat(64);
        assert!(!first.equivalent_to(&reordered));
    }
}

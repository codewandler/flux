//! The optional tenant/agent context envelope carried by a run.
//!
//! A *stream* in this store is one session — and, for a downstream multi-tenant service, one
//! **run**. Its owning account, the agent identity that served it, and a cross-run correlation
//! id are fixed for the run's whole lifetime, so they live on the `streams` registry (set once
//! at session creation) rather than being stamped on every event row. Every field is optional:
//! a single-tenant CLI session leaves the whole envelope empty and behaves exactly as before.
//!
//! The value of this type is **timing** — a downstream consumer (e.g. a managed multi-tenant
//! agent service) can fold the append-only log into per-account transcripts as *projections*,
//! instead of bolting a parallel store beside it, only if the log was tagged when the run was
//! written. Tagging it after the fact is a migration.

use serde::{Deserialize, Serialize};

/// The context an account/agent run is tagged with. All fields are optional and additive: an
/// empty envelope (the default) is the single-tenant case and changes no behaviour.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventContext {
    /// The account (tenant) that owns the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The agent identity that served the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The agent version that served the run (so a transcript records which revision ran).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    /// A correlation id tying related runs together (e.g. an A2A `context_id` or a conversation
    /// id spanning several sessions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl EventContext {
    /// An envelope tagged with just an account — the common multi-tenant case.
    pub fn for_account(account: impl Into<String>) -> Self {
        Self {
            account: Some(account.into()),
            ..Self::default()
        }
    }

    /// True when no field is set — the single-tenant case. Reads on untagged streams carry this.
    pub fn is_empty(&self) -> bool {
        self.account.is_none()
            && self.agent_id.is_none()
            && self.agent_version.is_none()
            && self.correlation_id.is_none()
    }
}

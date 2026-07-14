//! Shared builder state for the safety envelope — the knobs [`ClientBuilder`](crate::ClientBuilder)
//! and [`FlowClientBuilder`](crate::FlowClientBuilder) have in common (permission rules, approval
//! policy, OS-sandbox posture), factored so the two front doors cannot drift apart.

use std::sync::Arc;

use flux_runtime::{AllowApprover, Approver, DenyApprover, ExecutionAuthorization};
use flux_secret::Redactor;
use flux_system::sandbox::{Sandbox, SandboxSettings};

/// The envelope half of a builder: permission rules, the approval policy, and the OS-sandbox
/// posture. Owned by both client builders; the fluent methods on each delegate here.
pub(crate) struct Envelope {
    pub(crate) allow: Vec<String>,
    pub(crate) deny: Vec<String>,
    pub(crate) auto_approve: bool,
    pub(crate) approver: Option<Arc<dyn Approver>>,
    pub(crate) sandbox: Option<Sandbox>,
    pub(crate) authorization: ExecutionAuthorization,
    pub(crate) redactor: Redactor,
}

impl Envelope {
    /// An envelope with the given pre-allowed rules (each door's read-only defaults).
    pub(crate) fn with_default_allow(allow: &[&str]) -> Self {
        Envelope {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: Vec::new(),
            auto_approve: false,
            approver: None,
            sandbox: None,
            authorization: ExecutionAuthorization::local(),
            redactor: Redactor::new(),
        }
    }

    /// An envelope with no implicit rules at all — for the full-control
    /// [`ClientBuilder::from_spec`](crate::ClientBuilder::from_spec) path, where the spec's own
    /// permissions are the whole story.
    pub(crate) fn bare() -> Self {
        Envelope::with_default_allow(&[])
    }

    /// The approval policy: an injected [`Approver`] wins; otherwise `auto_approve` picks the
    /// blanket allow, and the headless default is deny (there is no approval UI in a library).
    pub(crate) fn resolve_approver(&self) -> Arc<dyn Approver> {
        if let Some(approver) = &self.approver {
            return approver.clone();
        }
        if self.auto_approve {
            Arc::new(AllowApprover)
        } else {
            Arc::new(DenyApprover)
        }
    }

    /// The OS-sandbox posture: an injected [`Sandbox`] wins; otherwise resolve from the
    /// environment (`FLUX_SANDBOX=require` honored; off ⇒ disabled, safe default).
    pub(crate) fn resolve_sandbox(&self) -> Sandbox {
        self.sandbox
            .clone()
            .unwrap_or_else(|| Sandbox::resolve(SandboxSettings::from_env()))
    }
}

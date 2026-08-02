//! Shared builder state for the safety envelope — the knobs [`ClientBuilder`](crate::ClientBuilder)
//! and [`FlowClientBuilder`](crate::FlowClientBuilder) have in common (permission rules, approval
//! policy, OS-sandbox posture), factored so the two front doors cannot drift apart.
//!
//! # The autonomous posture is one choice, not three knobs (C-444)
//!
//! Approval, confinement and ceilings used to be independent here: an embedder could set
//! `auto_approve(true)` and silently get no OS sandbox and no resource ceiling — the configuration the
//! Pi comparison called a poor fit, reachable straight off the documented happy path.
//!
//! The fix is **not** to make autonomy harder. Running without per-effect approval is a valid posture
//! (C-463): research, security hardening and long exploration are cases where interrupting per effect
//! is the wrong design, and flux already ships that posture on the CLI (C-262 / C-410 — unattended
//! surfaces are fail-closed sandbox *plus* auto-approve). The fix is that **choosing it carries its
//! confinement and its ceiling with it**, because the envelope has exactly one stage with a human in
//! it. Varying that stage is choosing a posture; what replaces the human is policy, isolation and
//! budgets, so those must be present rather than optional.
//!
//! [`Envelope::resolve_sandbox`] and [`Envelope::resolve_resource_limits`] are where that coherence
//! lives, and both keep an explicit embedder decision authoritative — an escape hatch that is visible
//! in the embedder's own source, never an omission.

use std::sync::Arc;

use flux_runtime::{AllowApprover, Approver, DenyApprover, ExecutionAuthorization, ResourceLimits};
use flux_secret::Redactor;
use flux_system::sandbox::{Sandbox, SandboxMode, SandboxSettings};

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
    /// C-290: the host's ceilings on what the runtime *uses* — simultaneously executing tool calls
    /// and retained result bytes. `None` means the embedder stated nothing, which
    /// [`Envelope::resolve_resource_limits`] reads as "the posture decides" (C-444): unbounded under
    /// supervision, [`ResourceLimits::autonomous`] under auto-approval. `Some` is always honored
    /// verbatim, including a deliberately unbounded `ResourceLimits::new()`.
    pub(crate) resource_limits: Option<ResourceLimits>,
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
            resource_limits: None,
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

    /// Whether this envelope resolves to an **autonomous** posture — no human in the approval stage.
    ///
    /// Only the blanket `auto_approve` counts. An injected [`Approver`] does *not*: it is a policy the
    /// embedder wrote, it may well be interactive or risk-aware, and deciding on its behalf that it
    /// needs confinement would be guessing at code this crate cannot see. An embedder injecting an
    /// approver states the rest of its posture too.
    pub(crate) fn is_autonomous(&self) -> bool {
        self.auto_approve && self.approver.is_none()
    }

    /// The OS-sandbox posture: an explicitly injected [`Sandbox`] wins; an autonomous posture is
    /// raised to fail-closed `require` with the network closed; otherwise resolve from the environment
    /// (`FLUX_SANDBOX=require` honored; off ⇒ disabled, safe default).
    ///
    /// **The raise (C-444).** An auto-approving client has no human approval boundary, which is
    /// exactly the CLI's criterion for pinning a surface to `require` (C-262 / C-410). A library has no
    /// argv to classify, but it does know its own approval posture — so that is what it classifies.
    /// Confinement travels with the autonomy rather than being a second thing to remember.
    ///
    /// Two things keep this from being a trap. An explicit [`with_sandbox`](crate::ClientBuilder::with_sandbox)
    /// still wins outright, so an embedder who has provided isolation another way (an outer container,
    /// a VM, a disposable host) says so in one visible line. And an ambient `FLUX_SANDBOX` that is
    /// *stricter* is still honored — the raise sets a floor, it never lowers one.
    pub(crate) fn resolve_sandbox(&self) -> Sandbox {
        if let Some(sandbox) = &self.sandbox {
            return sandbox.clone();
        }
        let mut settings = SandboxSettings::from_env();
        if self.is_autonomous() {
            // A floor, not an override: `require` is already the strictest mode, and the network
            // narrows to closed unless the environment explicitly opened it — the same default the
            // CLI's unattended profile applies. When the prompt is gone, destination scope is part of
            // what is left doing the constraining.
            settings.mode = SandboxMode::Require;
            if std::env::var("FLUX_SANDBOX_NET").is_err() {
                settings.network = false;
            }
        }
        Sandbox::resolve(settings)
    }

    /// The runtime-use ceilings: whatever the embedder stated, else the posture's default (C-444).
    ///
    /// An embedder that called `resource_limits(..)` gets exactly that, including a deliberately
    /// unbounded `ResourceLimits::new()` — stating a ceiling is a decision and this never second-
    /// guesses it. Silence is what changes: an autonomous posture resolves to
    /// [`ResourceLimits::autonomous`] rather than to unbounded, because "unattended *and* unbounded"
    /// was the finding. A supervised client stays unbounded, as before.
    pub(crate) fn resolve_resource_limits(&self) -> ResourceLimits {
        match &self.resource_limits {
            Some(limits) => limits.clone(),
            None if self.is_autonomous() => ResourceLimits::autonomous(),
            None => ResourceLimits::new(),
        }
    }
}

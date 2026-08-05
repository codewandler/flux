//! Shared builder state for the safety envelope — the knobs [`ClientBuilder`](crate::ClientBuilder)
//! and [`FlowClientBuilder`](crate::FlowClientBuilder) have in common (permission rules, approval
//! policy, OS-sandbox posture), factored so the two front doors cannot drift apart.
//!
//! # The autonomous posture is one named choice, not three knobs (C-444, C-463)
//!
//! Approval, confinement and ceilings used to be independent here: an embedder could set
//! `auto_approve(true)` and silently get no OS sandbox and no resource ceiling — the configuration the
//! Pi comparison called a poor fit, reachable straight off the documented happy path.
//!
//! The fix is **not** to make autonomy harder. Running without per-effect approval is a valid posture:
//! research, security hardening and long exploration are cases where interrupting per effect is the
//! wrong design, and flux already ships that posture on the CLI (C-262 / C-410 — unattended surfaces
//! are fail-closed sandbox *plus* auto-approve). The fix is that **choosing it carries its confinement
//! and its ceiling with it**, because the envelope has exactly one stage with a human in it. Varying
//! that stage is choosing a posture; what replaces the human is policy, isolation and budgets, so
//! those must be present rather than optional.
//!
//! C-463 gave that choice a name. [`flux_runtime::AutonomyPosture`] *is* the coupling — approval
//! stance, sandbox floor and budget are three questions answered by one value — so this module no
//! longer decides them, it only resolves **which** posture applies and lets explicit embedder
//! decisions override the defaults it implies. [`Envelope::posture`] is that resolution;
//! [`Envelope::resolve_approver`], [`Envelope::resolve_sandbox`] and
//! [`Envelope::resolve_resource_limits`] read off it.
//!
//! Every one of those three keeps an explicit embedder decision authoritative — an escape hatch that
//! is visible in the embedder's own source, never an omission.

use std::sync::Arc;

use flux_core::{Error, Result};
use flux_runtime::{
    Approver, AutonomyPosture, ExecutionAuthorization, ResourceLimits, SandboxFloor,
};
use flux_secret::Redactor;
use flux_system::sandbox::{Sandbox, SandboxSettings};

/// The envelope half of a builder: permission rules, the approval policy, and the OS-sandbox
/// posture. Owned by both client builders; the fluent methods on each delegate here.
pub(crate) struct Envelope {
    pub(crate) allow: Vec<String>,
    pub(crate) deny: Vec<String>,
    pub(crate) auto_approve: bool,
    pub(crate) approver: Option<Arc<dyn Approver>>,
    /// C-463: the named posture, when the embedder chose one. `None` means it is inferred from the
    /// approval settings — see [`Envelope::posture`].
    pub(crate) posture: Option<AutonomyPosture>,
    pub(crate) sandbox: Option<Sandbox>,
    pub(crate) authorization: ExecutionAuthorization,
    pub(crate) redactor: Redactor,
    /// C-290: the host's ceilings on what the runtime *uses* — simultaneously executing tool calls
    /// and retained result bytes. `None` means the embedder stated nothing, which
    /// [`Envelope::resolve_resource_limits`] reads as "the posture decides" (C-444/C-463). `Some` is
    /// always honored verbatim, including a deliberately unbounded `ResourceLimits::new()`.
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
            posture: None,
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

    /// The [`AutonomyPosture`] this envelope runs under — the single value the other three
    /// resolvers read.
    ///
    /// An explicitly named posture wins. Otherwise it is **inferred**, and the inference is
    /// deliberately conservative:
    ///
    /// - `auto_approve(true)` is [`AutonomyPosture::BoundedAutonomy`] by definition — that pairing
    ///   (never prompt; fail-closed sandbox, network closed, autonomous ceilings) is exactly what the
    ///   flag has always meant on an unattended CLI surface. ⚠ **No flag day**: the flag keeps
    ///   working and keeps meaning the same thing, it simply now has a name.
    /// - An **injected** [`Approver`] gets the same posture, because the SDK cannot see whether it
    ///   prompts a human. It may; it may equally return `Allow` for everything. Treating that unknown
    ///   as supervised would let three lines of custom code recover the unconfined, unbounded
    ///   configuration C-444 removed. An embedder whose approver really is a human channel says so
    ///   with `.posture(AutonomyPosture::Supervised)`.
    /// - Neither: a library has no approval UI, so there is no human to ask and nothing pre-arranged
    ///   to allow — [`AutonomyPosture::Refusing`], which is what the headless default deny already
    ///   was.
    pub(crate) fn posture(&self) -> AutonomyPosture {
        if let Some(posture) = self.posture {
            return posture;
        }
        if self.auto_approve || self.approver.is_some() {
            AutonomyPosture::for_auto_approval()
        } else {
            AutonomyPosture::Refusing
        }
    }

    /// The approval policy: an injected [`Approver`] wins; otherwise the posture decides.
    ///
    /// The one refusal is [`AutonomyPosture::Supervised`] with nothing injected. That posture *is*
    /// a human channel, and a library has none — resolving it to an allow-all or a deny-all would
    /// mean the embedder's stated posture and the client's behavior disagree, which is the whole
    /// class of accident this type removes.
    pub(crate) fn resolve_approver(&self) -> Result<Arc<dyn Approver>> {
        if let Some(approver) = &self.approver {
            return Ok(approver.clone());
        }
        self.posture().approver(None).ok_or_else(|| {
            Error::Config(
                "the `supervised` autonomy posture asks a human before each guarded effect, and a \
                 library has no approval UI to ask through. Provide the channel with \
                 `.approver(..)`, or choose a posture that does not need one: `bounded-autonomy` \
                 (never prompt; policy, a fail-closed sandbox and budgets constrain instead), \
                 `exploratory`, or `refusing`."
                    .to_string(),
            )
        })
    }

    /// The OS-sandbox posture: an explicitly injected [`Sandbox`] wins; otherwise the environment
    /// (`FLUX_SANDBOX`) is resolved and then **raised to the posture's floor** (C-463).
    ///
    /// **The raise.** A posture with no human in the approval stage carries a fail-closed `require`
    /// floor with the network closed, because that is part of the same choice rather than a second
    /// thing to remember: when the prompt is gone, isolation and destination scope are what is left
    /// doing the constraining. A library has no argv to classify, but it does know its own posture —
    /// so that is what it classifies.
    ///
    /// Two things keep this from being a trap. An explicit [`with_sandbox`](crate::ClientBuilder::with_sandbox)
    /// still wins outright, so an embedder who has provided isolation another way (an outer container,
    /// a VM, a disposable host) says so in one visible line. And it is a **floor**: an ambient
    /// `FLUX_SANDBOX` that is *stricter* is still honored, and the network is only narrowed when the
    /// environment did not explicitly open it.
    pub(crate) fn resolve_sandbox(&self) -> Sandbox {
        if let Some(sandbox) = &self.sandbox {
            return sandbox.clone();
        }
        let mut settings = SandboxSettings::from_env();
        let floor: SandboxFloor = self.posture().sandbox_floor();
        settings.mode = floor.raise_mode(settings.mode);
        if !floor.network && std::env::var("FLUX_SANDBOX_NET").is_err() {
            settings.network = false;
        }
        Sandbox::resolve(settings)
    }

    /// The runtime-use ceilings: whatever the embedder stated, else the posture's budget (C-463).
    ///
    /// An embedder that called `resource_limits(..)` gets exactly that, including a deliberately
    /// unbounded `ResourceLimits::new()` — stating a ceiling is a decision and this never second-
    /// guesses it. Silence is what changes: a posture that never prompts resolves to something
    /// finite, because "unattended *and* unbounded" was the finding. A supervised client stays
    /// unbounded, as before — a human answering prompts is already the pacing constraint.
    pub(crate) fn resolve_resource_limits(&self) -> ResourceLimits {
        match &self.resource_limits {
            Some(limits) => limits.clone(),
            None => self.posture().budget(),
        }
    }
}

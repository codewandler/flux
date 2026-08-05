//! The autonomy posture: **one named choice** that carries its approval stance, its confinement
//! floor and its budget together (C-463).
//!
//! # Autonomy is a posture, not an absence of safety
//!
//! The envelope is *authorization → approval → guarded IO*, and **approval is the only stage of the
//! three with a human in it**. Varying that stage is choosing a posture. Removing either of the
//! other two would be a bug, and [`AutonomyPosture`] deliberately offers no way to do it: there is
//! nothing here that selects a substrate, widens a grant set, or silences the evidence log.
//!
//! So an agent running without a per-effect prompt is not an unguarded agent. Authorization still
//! decides, guarded IO still executes, evidence is still recorded. What changes is where the
//! constraint budget comes from: it moves off *human latency* and onto policy, isolation, budgets
//! and destination scope — all of which matter **more**, not less, once the prompt is gone.
//!
//! That is the entire reason this type exists as one value rather than three settings. Before it,
//! approval, confinement and ceilings were independent, so "stop asking" was reachable without
//! "confine harder" — the finding C-444 recorded from the SDK side. Reading all three off one
//! posture is what makes that combination unspellable.
//!
//! # The four postures
//!
//! Four argued postures, deliberately **not** an extensible preset scheme. A configurable generator
//! would let an operator reassemble exactly the incoherent combination this replaces.
//!
//! | posture | approval | relies on |
//! |---|---|---|
//! | [`Supervised`](AutonomyPosture::Supervised) | per effect | a human at a terminal |
//! | [`BoundedAutonomy`](AutonomyPosture::BoundedAutonomy) | none | policy + fail-closed sandbox + budgets |
//! | [`Exploratory`](AutonomyPosture::Exploratory) | none, and interruption is the harm | hard isolation + wide-but-bounded grants + full evidence |
//! | [`Refusing`](AutonomyPosture::Refusing) | denies everything | nothing beyond what was pre-authorised running at all |
//!
//! None of them is a degraded form of another. Each states what it *relies on*
//! ([`relies_on`](AutonomyPosture::relies_on)) and, because that alone would be marketing, what it
//! does **not** protect against ([`does_not_protect_against`](AutonomyPosture::does_not_protect_against)).

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use flux_core::Error;
use flux_system::sandbox::SandboxMode;

use crate::{AllowApprover, Approver, DenyApprover, ResourceLimits};

/// Who answers for one guarded effect.
///
/// This is the *stance*, not the channel: [`PerEffect`](Self::PerEffect) says a human is asked,
/// while which human and over what transport (a terminal prompt, the TUI, an HTTP approval queue)
/// belongs to the surface. That split is what lets a remote approver be "the supervised posture,
/// reachable over a network" instead of a fourth thing to reason about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalStance {
    /// A human is asked before each guarded effect.
    PerEffect,
    /// Nobody is asked. The constraint comes from policy, isolation and budgets instead.
    None,
    /// Nothing that reaches the approval stage runs.
    DenyAll,
}

/// The confinement floor a posture carries into [`flux_system`]'s sandbox resolution.
///
/// A **floor**, never an override: a surface resolves the strictest of every source that asks for
/// confinement, so a posture can raise the sandbox but an ambient `FLUX_SANDBOX=require` is never
/// lowered by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxFloor {
    /// The weakest sandbox mode this posture will accept.
    pub mode: SandboxMode,
    /// Whether sandboxed processes may reach the network by default. `false` narrows egress as part
    /// of the same choice; when the prompt is gone, destination scope is part of what is left doing
    /// the constraining.
    pub network: bool,
}

impl SandboxFloor {
    /// The stricter of this floor and an already-resolved `mode` — **tightest wins**.
    ///
    /// This is the only way a posture is allowed to touch a resolved sandbox mode. A posture may
    /// raise confinement; nothing about choosing one may lower it, or an ambient
    /// `FLUX_SANDBOX=require` could be softened by naming a posture.
    pub fn raise_mode(self, mode: SandboxMode) -> SandboxMode {
        const fn rank(mode: SandboxMode) -> u8 {
            match mode {
                SandboxMode::Off => 0,
                SandboxMode::On => 1,
                SandboxMode::Require => 2,
            }
        }
        if rank(self.mode) >= rank(mode) {
            self.mode
        } else {
            mode
        }
    }
}

/// A named autonomy posture — the whole choice, in one value.
///
/// See the [module documentation](self) for why this is one type rather than three settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutonomyPosture {
    /// Ask a human before each guarded effect. The daily driver, and the right answer in an
    /// unfamiliar repository.
    Supervised,
    /// Never prompt; constrain through authorization policy, a fail-closed sandbox with the network
    /// closed, and resource budgets. This is what `--yes` and `auto_approve(true)` have always
    /// selected on an unattended surface (C-262 / C-410) — it now has a name.
    BoundedAutonomy,
    /// Never prompt, and treat interruption as the harm rather than the safeguard. Research,
    /// security hardening and long exploration are jobs where stopping at every effect produces a
    /// broken agent, not a careful one. Confined like [`BoundedAutonomy`](Self::BoundedAutonomy)
    /// but with deliberately wider grants — network egress stays open and the ceilings are looser —
    /// and with the evidence trail left uncapped, because that trail is what this posture leans on.
    Exploratory,
    /// Refuse every effect that reaches the approval stage. What a served agent with nothing
    /// configured should be, rather than something that quietly picked a posture on the operator's
    /// behalf.
    Refusing,
}

impl AutonomyPosture {
    /// Every posture, in the order they are documented. Iterating this is how a caller proves it
    /// handled all four; there is deliberately no way to construct a fifth.
    pub const ALL: [AutonomyPosture; 4] = [
        AutonomyPosture::Supervised,
        AutonomyPosture::BoundedAutonomy,
        AutonomyPosture::Exploratory,
        AutonomyPosture::Refusing,
    ];

    /// The posture that `--yes` / `auto_approve(true)` selects.
    ///
    /// ⚠ **No flag day.** The existing spellings keep working and keep meaning exactly what they
    /// meant: an unattended surface pinned to a fail-closed sandbox with the network closed and
    /// autonomous ceilings. What changes is that the combination now has a name, so it can be
    /// stated, documented and argued with rather than inferred from a boolean.
    pub const fn for_auto_approval() -> Self {
        AutonomyPosture::BoundedAutonomy
    }

    /// The stable wire/CLI name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Supervised => "supervised",
            Self::BoundedAutonomy => "bounded-autonomy",
            Self::Exploratory => "exploratory",
            Self::Refusing => "refusing",
        }
    }

    /// Who answers for a guarded effect under this posture.
    pub const fn approval(self) -> ApprovalStance {
        match self {
            Self::Supervised => ApprovalStance::PerEffect,
            Self::BoundedAutonomy | Self::Exploratory => ApprovalStance::None,
            Self::Refusing => ApprovalStance::DenyAll,
        }
    }

    /// The confinement floor this posture carries.
    ///
    /// **The coherence rule, stated once:** every posture whose [`approval`](Self::approval) is
    /// [`ApprovalStance::None`] floors at [`SandboxMode::Require`]. That is not a policy applied on
    /// top of the posture — it is the posture. Dropping the prompt without raising confinement is
    /// the combination this type exists to make unspellable.
    ///
    /// [`Supervised`](Self::Supervised) and [`Refusing`](Self::Refusing) impose no floor of their
    /// own: the first has a human boundary and the second has no effects to confine, so both leave
    /// the sandbox to `FLUX_SANDBOX` / `[sandbox]` exactly as before. See
    /// [`does_not_protect_against`](Self::does_not_protect_against) for what `Refusing` is
    /// therefore *not* — refusal is an approval-stage boundary, not an isolation one.
    pub const fn sandbox_floor(self) -> SandboxFloor {
        match self {
            Self::Supervised | Self::Refusing => SandboxFloor {
                mode: SandboxMode::Off,
                network: true,
            },
            Self::BoundedAutonomy => SandboxFloor {
                mode: SandboxMode::Require,
                // Closed: with no prompt in the way, destination scope is part of the constraint.
                network: false,
            },
            Self::Exploratory => SandboxFloor {
                mode: SandboxMode::Require,
                // Open, deliberately. Research and security hardening are network jobs; confining
                // the host filesystem while cutting egress would not make this posture safer, it
                // would make it useless and push the operator to `--no-sandbox` instead.
                network: true,
            },
        }
    }

    /// The runtime-use ceilings this posture carries.
    ///
    /// A **default**, not a cap on the host: a surface that states its own ceilings keeps them.
    /// What this fixes is silence — "unattended *and* unbounded" was the actual C-444 finding, so a
    /// posture that never prompts resolves to something finite rather than to nothing.
    pub fn budget(self) -> ResourceLimits {
        match self {
            // A human answering prompts is the pacing constraint; adding a second one would refuse
            // work the operator is watching and has approved.
            Self::Supervised => ResourceLimits::new(),
            Self::BoundedAutonomy => ResourceLimits::autonomous(),
            Self::Exploratory => {
                ResourceLimits::new()
                    // Twice the autonomous fan-out: exploration is wide by design. Still finite —
                    // at most 32 × 16 = 512 simultaneous tool calls across the whole delegated tree.
                    .with_max_concurrent_tool_calls(32)
                    .with_max_live_agents(16)
                    .with_max_retained_result_bytes(256 * 1024 * 1024)
                // Evidence is deliberately NOT capped here. The other two constraints this posture
                // leans on are isolation and the audit trail; eliding payloads to save memory would
                // spend the one thing that makes a long unsupervised run accountable afterwards.
            }
            // Nothing that reaches the approval stage runs, so a ceiling would bound nothing. Left
            // unbounded rather than decorative.
            Self::Refusing => ResourceLimits::new(),
        }
    }

    /// The approver this posture runs, given the surface's per-effect human channel.
    ///
    /// [`Supervised`](Self::Supervised) **is** that channel, so a surface without one (a library, an
    /// HTTP listener with no approval transport configured) cannot offer the posture and gets
    /// `None` — never a silent downgrade to allow-all or deny-all under a name that promises a
    /// human. Every other posture is fully determined by the posture itself and ignores the
    /// argument.
    pub fn approver(self, human: Option<Arc<dyn Approver>>) -> Option<Arc<dyn Approver>> {
        match self {
            Self::Supervised => human,
            Self::BoundedAutonomy | Self::Exploratory => Some(Arc::new(AllowApprover)),
            Self::Refusing => Some(Arc::new(DenyApprover)),
        }
    }

    /// What constrains an agent running under this posture — the honest replacement for "how much
    /// safety is switched on", which is not a question any of these four answers.
    pub const fn relies_on(self) -> &'static str {
        match self {
            Self::Supervised => {
                "a human at a terminal, reading and answering each guarded effect before it lands"
            }
            Self::BoundedAutonomy => {
                "authorization policy, a fail-closed OS sandbox with the network closed, and \
                 resource budgets"
            }
            Self::Exploratory => {
                "hard isolation of the host, deliberately wide but bounded grants including network \
                 egress, and the complete evidence trail"
            }
            Self::Refusing => {
                "nothing running beyond what was already pre-authorised before the agent started"
            }
        }
    }

    /// ⚠ What this posture does **not** protect against.
    ///
    /// Every posture here is a legitimate choice, and every one of them leaves something to the
    /// operator. Naming that thing is not a warning against the choice — it is the difference
    /// between a posture an operator selected and one they assumed.
    pub const fn does_not_protect_against(self) -> &'static str {
        match self {
            Self::Supervised => {
                "approval fatigue. A prompt is a boundary only while it is being read, and a run \
                 that asks forty times gets forty reflexive answers. It also confines nothing \
                 between prompts: no OS sandbox is implied by a human being present."
            }
            Self::BoundedAutonomy => {
                "an authorised effect inside the workspace. Everything policy already grants \
                 happens without anyone seeing it first, so the working tree is the blast radius. \
                 Run it where losing the working tree is survivable — a branch, a worktree, a \
                 disposable checkout."
            }
            Self::Exploratory => {
                "exfiltration. Egress is open on purpose, and an agent that can read the workspace \
                 and reach the internet can move one to the other. What is isolated here is the \
                 host, not the data: point this at a disposable checkout rather than at a valuable \
                 repository holding live credentials."
            }
            Self::Refusing => {
                "anything that never reaches the approval stage. Refusal sits at that stage only — \
                 pre-authorised operations resolve before it, and native processes a surface starts \
                 before any effect is requested (plugin binaries at startup) never consult it at \
                 all. Pair it with `[sandbox] require` when that gap matters."
            }
        }
    }

    /// The one line a surface states at startup, so a log read six months later says which posture
    /// the run was under and what it was leaning on.
    pub fn announcement(self) -> String {
        let approval = match self.approval() {
            ApprovalStance::PerEffect => "each guarded effect is answered by a human",
            ApprovalStance::None => "guarded effects are not paused for a human",
            ApprovalStance::DenyAll => "guarded effects reaching the approval stage are refused",
        };
        format!(
            "Autonomy posture: {} — {approval}. Relies on {}.",
            self.name(),
            self.relies_on()
        )
    }
}

impl fmt::Display for AutonomyPosture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for AutonomyPosture {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept the underscore spelling too: a config key and a CLI value should not disagree
        // about one hyphen. Nothing else is guessed — an unknown name is refused, because silently
        // resolving a typo to a posture is exactly the class of accident this type removes.
        let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
        AutonomyPosture::ALL
            .into_iter()
            .find(|p| p.name() == normalized)
            .ok_or_else(|| {
                Error::Config(format!(
                    "unknown autonomy posture {s:?} (expected one of: {})",
                    AutonomyPosture::ALL
                        .iter()
                        .map(|p| p.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant that outlives any individual posture: this type answers *who approves*, *how
    /// confined* and *how much*, and nothing else. If a fourth question ever appears here, check it
    /// is not one of the two stages that must not vary.
    #[test]
    fn the_posture_answers_exactly_the_three_questions_it_should() {
        for posture in AutonomyPosture::ALL {
            let _: ApprovalStance = posture.approval();
            let _: SandboxFloor = posture.sandbox_floor();
            let _: ResourceLimits = posture.budget();
        }
    }

    #[test]
    fn an_underscore_spelling_resolves_and_a_typo_does_not() {
        assert_eq!(
            "bounded_autonomy".parse::<AutonomyPosture>().unwrap(),
            AutonomyPosture::BoundedAutonomy
        );
        assert_eq!(
            "  Exploratory ".parse::<AutonomyPosture>().unwrap(),
            AutonomyPosture::Exploratory
        );
        let err = "bounded".parse::<AutonomyPosture>().unwrap_err();
        assert!(err.to_string().contains("bounded-autonomy"), "{err}");
    }

    /// Exploratory is wider than bounded autonomy but still finite — the claim its documentation
    /// makes, pinned so a future edit to either set of numbers cannot quietly invert it.
    #[test]
    fn exploratory_is_wider_than_bounded_autonomy_and_still_bounded() {
        let bounded = AutonomyPosture::BoundedAutonomy.budget();
        let exploratory = AutonomyPosture::Exploratory.budget();
        assert!(!exploratory.is_unbounded());
        assert!(
            exploratory.max_concurrent_tool_calls() > bounded.max_concurrent_tool_calls(),
            "exploratory must permit more simultaneous work, not less"
        );
        assert!(
            exploratory.max_live_agents() > bounded.max_live_agents(),
            "exploratory must permit a wider delegated tree"
        );
        assert_eq!(
            exploratory.max_evidence_payload_bytes(),
            None,
            "exploratory leans on the full evidence trail; capping it would spend the thing that \
             makes a long unsupervised run accountable"
        );
    }
}

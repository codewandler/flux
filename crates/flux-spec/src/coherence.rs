//! Metadata-coherence invariants over a [`ToolSpec`] (C-191).
//!
//! # Why this module exists
//!
//! Approval gating is driven by what each operation *says about itself*: its [`Effect`]s, its
//! [`Risk`] tier, its [`Idempotency`], and the [`AccessKind`]s it reaches. Nothing cross-checks
//! those four against each other, so an op that starts life as [`ToolSpec::read_only`] — `Read` +
//! `Risk::Low` + `Idempotent`, an internally consistent preset — and *later gains a mutating
//! effect* without upgrading its tier compiles, ships, and clears a lower bar than it deserves. The
//! field design is sound; the failure mode is drift over time. This module turns that standing
//! trust assumption into a check.
//!
//! # Vocabulary
//!
//! [`Effect`] mixes two different things, and the invariants only make sense once they are named
//! apart:
//!
//! * **Directional effects** — [`Effect::Read`] observes state, [`Effect::Write`] mutates it.
//! * **Carrier effects** — [`Effect::Filesystem`], [`Effect::Network`], [`Effect::Process`],
//!   [`Effect::Browser`], [`Effect::LocalSystem`] name the *host resource reached*, not the
//!   direction of travel. `read` declares `[Read, Filesystem]`; `write` declares
//!   `[Write, Filesystem]`. A carrier alone says nothing about whether anything changed.
//!
//! Two carriers are bounded by an existing guard and are therefore consequence-free on their own:
//! `Filesystem` is confined to the workspace jail, and `Network` is confined by
//! `flux_system::net::guard_url` — and a network carrier paired with `Read` and no `Write` is a
//! fetch. The other three are a consequence *regardless of direction*: `Process` runs a program of
//! the operation's choosing, `LocalSystem` reaches host state outside the jail, and `Browser`
//! drives a real, session-bearing browser.
//!
//! **A spec is *consequence-bearing*** when
//!
//! 1. it declares **no** effects and names [`AccessKind::Process`], [`AccessKind::Connection`] or
//!    [`AccessKind::LocalSystem`] — an undeclared op holding a code-running or host-reaching
//!    capability is not inert; or
//! 2. it declares effects and that set leaves `{Read, Filesystem, Network}`, or names `Network`
//!    without `Read`.
//!
//! That is not a new classification: it is exactly the shape `flux-flow`'s `gather_safe`
//! (`crates/flux-flow/src/staged.rs`) refuses to run during evidence gathering — including the
//! detail that the access branch applies *only* to an empty effect set. That detail is load-bearing
//! rather than incidental, because `access` is not always an op-level fact: `flux-plugin` projects
//! it from the *plugin's* declared capabilities, so every op of a `process`-capable plugin carries
//! `AccessKind::Process` whether or not it runs anything. Reading access as a consequence
//! regardless of effects would condemn every read op of, say, the `kubernetes` plugin.
//!
//! # The invariants
//!
//! **I1 — the risk floor. A consequence-bearing spec must not declare [`Risk::Low`].**
//! `Risk::Low` is the tier every risk consumer reads as "nothing here worth a gate":
//! `flux-flow`'s `gather_safe` runs a `Risk::Low` op *before* the approval gate; the dispatcher's
//! op cache memoizes and replays only `Risk::Low` results; `RiskApprover::gates` auto-approves
//! anything below its threshold; and — the sharpest one — `PlanRisk::summary` renders the tier
//! verbatim into the sentence a human reads at the plan-approval prompt. An op that writes, runs a
//! program, reaches host state, drives a browser, or egresses unread while claiming `Risk::Low`
//! understates itself to the person approving it.
//!
//! **I2 — the destructive floor. A tool declaring the semantic effect `delete` or `money` must
//! declare [`Risk::Destructive`].** Those two tags are precisely what
//! `AuthorityRequirement::is_destructive` treats as destructive (they lower to the `flow.delete` /
//! `flow.money` actions, see [`FlowEffect::lower`](crate::FlowEffect::lower)), and
//! `Risk::Destructive` is what forces approval unconditionally in `Executor::dispatch` and raises
//! the destructive badge in the plan preview. An op that irreversibly deletes or moves money at a
//! lower tier gets neither. Over-declaring risk is always safe, so there is no converse rule.
//!
//! **I3 — the repeatability floor. A consequence-bearing spec must not declare
//! [`Idempotency::Idempotent`].** `Idempotent` is the claim "repeating this call is safe"; it is
//! what licenses the dispatcher's op cache to serve a stored result *instead of executing*, and
//! what any future retry/replay consumer must be able to trust. For an op that mutates, runs a
//! program, or egresses unread, the honest declaration is [`Idempotency::NonIdempotent`], or
//! [`Idempotency::Conditional`] when it is genuinely safe to repeat under stated conditions.
//! `Conditional` — not a loosened rule — is the escape hatch for "safely repeatable".
//!
//! # What is deliberately *not* an invariant here
//!
//! Effect↔access coherence (a filesystem effect without filesystem access, a write without a typed
//! write resource) is already enforced, with better diagnostics, by
//! `flux_runtime::authority_requirements_from_declaration`. This module does not restate it.

use crate::{AccessKind, Effect, Idempotency, Risk, ToolSpec};

/// Semantic-effect tags whose consequence class is irreversible or monetary — the two
/// `AuthorityRequirement::is_destructive` recognizes (`flow.delete` / `flow.money`).
const DESTRUCTIVE_TAGS: &[&str] = &["delete", "money"];

/// Operations exempted from one or more invariants, each with the reason it is legitimate rather
/// than drift. **Prefer fixing the declaration.** An entry here is a claim that the op's metadata is
/// already the most honest description available, and it needs to say why.
///
/// Empty is the goal state. Every entry below is an I1 exemption for the same, narrow shape.
const EXEMPT: &[Exemption] = &[
    // The three read-only `git` observers are consequence-bearing by classification — they declare
    // `AccessKind::Process`, and a code-running capability is a consequence — but their argv is
    // fixed by the op (`git status --short`, `git diff`, `git log`), never assembled from a
    // caller-supplied program name; the caller may only narrow the scope (a path, a limit). They
    // read the repository the agent is already working in and change nothing.
    //
    // Scope of the exemption: I1 only, i.e. "auto-approvable", which is the intended posture for
    // reading your own working tree. It is not a blanket pass — I3 still applies to all three, and
    // their results track the working tree rather than their input, so none of them may claim
    // `Idempotent`. Nor does the exemption reach the two guards that matter most: `gather_safe` and
    // the op cache both refuse an `Effect::Process` op on its effect set alone, regardless of tier.
    Exemption {
        op: "git_status",
        invariants: &["I1"],
        reason: "fixed argv `git status --short`; observes the working tree, mutates nothing",
    },
    Exemption {
        op: "git_diff",
        invariants: &["I1"],
        reason: "fixed argv `git diff`, caller may only restrict it to a path; read-only",
    },
    Exemption {
        op: "git_log",
        invariants: &["I1"],
        reason: "fixed argv `git log`, caller may only cap the entry count; read-only",
    },
];

/// One allowlist entry: an op name, the invariants it is excused from, and why.
struct Exemption {
    /// The op's `ToolSpec::name`, matched exactly.
    op: &'static str,
    /// Invariant ids this op is excused from (`"I1"`, `"I2"`, `"I3"`).
    invariants: &'static [&'static str],
    /// Why the declaration is honest as it stands. Not "it was already like that".
    #[allow(dead_code)]
    reason: &'static str,
}

/// Whether `spec` reaches something whose consequence outlives the call — see the module docs for
/// the derivation. Public because the invariants are only legible alongside the classification they
/// are built on.
pub fn is_consequence_bearing(spec: &ToolSpec) -> bool {
    if spec.effects.is_empty() {
        return spec.access.iter().any(|a| {
            matches!(
                a,
                AccessKind::Process | AccessKind::Connection | AccessKind::LocalSystem
            )
        });
    }
    let carriers_are_bounded = spec
        .effects
        .iter()
        .all(|e| matches!(e, Effect::Read | Effect::Filesystem | Effect::Network));
    let network_is_read =
        !spec.effects.contains(&Effect::Network) || spec.effects.contains(&Effect::Read);
    !(carriers_are_bounded && network_is_read)
}

fn exempt_from(op: &str, invariant: &str) -> bool {
    EXEMPT
        .iter()
        .any(|e| e.op == op && e.invariants.contains(&invariant))
}

/// Every metadata-coherence invariant `spec` violates, one human-readable sentence each. Empty
/// means the declaration is coherent.
///
/// `semantic_effects` is the tag list the tool advertises through
/// `flux_runtime::Tool::semantic_effects` (the [`FlowEffect`](crate::FlowEffect) tag vocabulary);
/// pass an empty slice for a spec that carries none. It is taken as plain strings for the same
/// reason the trait hook returns them that way — the tool seam must not need the language crate.
pub fn metadata_violations(spec: &ToolSpec, semantic_effects: &[String]) -> Vec<String> {
    let mut violations = Vec::new();
    let consequential = is_consequence_bearing(spec);

    if consequential && spec.risk == Risk::Low && !exempt_from(&spec.name, "I1") {
        violations.push(format!(
            "I1 (risk floor): `{}` declares effects {:?} / access {:?} — a consequence-bearing shape — \
             but carries `Risk::Low`, the tier the gather path, the op cache, and the approval prompt \
             all read as harmless",
            spec.name, spec.effects, spec.access
        ));
    }

    if let Some(tag) = semantic_effects
        .iter()
        .find(|t| DESTRUCTIVE_TAGS.contains(&t.as_str()))
    {
        if spec.risk != Risk::Destructive && !exempt_from(&spec.name, "I2") {
            violations.push(format!(
                "I2 (destructive floor): `{}` declares the semantic effect `{tag}` but carries \
                 `{:?}` rather than `Risk::Destructive`, so it is neither forced to approval nor \
                 badged destructive in the plan preview",
                spec.name, spec.risk
            ));
        }
    }

    if consequential
        && spec.idempotency == Idempotency::Idempotent
        && !exempt_from(&spec.name, "I3")
    {
        violations.push(format!(
            "I3 (repeatability floor): `{}` declares effects {:?} / access {:?} — a \
             consequence-bearing shape — but claims `Idempotency::Idempotent`; use \
             `NonIdempotent`, or `Conditional` when repeating really is safe",
            spec.name, spec.effects, spec.access
        ));
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AccessKind;
    use serde_json::json;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec::read_only(name, "d", json!({"type": "object"}))
    }

    #[test]
    fn the_read_only_preset_is_coherent() {
        assert!(metadata_violations(&spec("read"), &[]).is_empty());
    }

    #[test]
    fn a_read_shaped_filesystem_or_fetch_op_stays_low_risk() {
        let file_read = spec("read").with_effects(vec![Effect::Read, Effect::Filesystem]);
        assert!(metadata_violations(&file_read, &[]).is_empty());
        let fetch = spec("web.fetch").with_effects(vec![Effect::Read, Effect::Network]);
        assert!(metadata_violations(&fetch, &[]).is_empty());
    }

    #[test]
    fn a_mutating_op_may_not_keep_the_read_only_tier() {
        let drifted = spec("write")
            .with_effects(vec![Effect::Write, Effect::Filesystem])
            .with_access(vec![AccessKind::Filesystem]);
        let found = metadata_violations(&drifted, &[]);
        assert_eq!(
            found.len(),
            2,
            "risk floor and repeatability floor: {found:?}"
        );
        assert!(found[0].starts_with("I1 "), "{found:?}");
        assert!(found[1].starts_with("I3 "), "{found:?}");
    }

    #[test]
    fn a_code_running_capability_is_a_consequence_when_no_effect_is_declared() {
        let inert_looking = spec("plugin.op")
            .with_effects(Vec::new())
            .with_access(vec![AccessKind::Process]);
        assert!(is_consequence_bearing(&inert_looking));
        assert!(!metadata_violations(&inert_looking, &[]).is_empty());
    }

    /// The access branch must NOT reach a spec that declares its effects. `flux-plugin` projects
    /// `access` from the *plugin's* capabilities, not the op's, so every op of a `process`-capable
    /// plugin carries `AccessKind::Process` — including its pure reads. Treating that as a
    /// consequence would refuse to load whole plugins over ops that only read.
    #[test]
    fn plugin_wide_process_access_does_not_condemn_a_declared_read() {
        let plugin_read = spec("kubernetes.pod.list")
            .with_effects(vec![Effect::Read])
            .with_access(vec![AccessKind::Process, AccessKind::Network]);
        assert!(!is_consequence_bearing(&plugin_read));
        assert!(metadata_violations(&plugin_read, &[]).is_empty());
    }

    #[test]
    fn unread_network_egress_is_a_consequence() {
        let post = spec("http.post").with_effects(vec![Effect::Network]);
        assert!(is_consequence_bearing(&post));
    }

    #[test]
    fn a_deleting_op_must_declare_destructive_risk() {
        // I1 and I3 are already satisfied, so the only thing left to catch is the tier.
        let mut deletes = spec("tickets.purge")
            .with_effects(vec![Effect::Write])
            .with_risk(Risk::High);
        deletes.idempotency = Idempotency::NonIdempotent;
        let found = metadata_violations(&deletes, &["delete".to_string()]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("I2 "), "{found:?}");

        let fixed = deletes.with_risk(Risk::Destructive);
        assert!(metadata_violations(&fixed, &["delete".to_string()]).is_empty());
    }

    #[test]
    fn conditional_is_the_escape_hatch_for_a_repeatable_mutation() {
        let mut op = spec("git.stage")
            .with_effects(vec![Effect::Write])
            .with_risk(Risk::Medium);
        op.idempotency = Idempotency::Conditional;
        assert!(metadata_violations(&op, &[]).is_empty());
    }
}

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
//! **…or when it declares a consequential semantic effect (C-210).** The effect set is only one of
//! two channels an operation declares consequence through; the other is the
//! [`FlowEffect`](crate::FlowEffect) tag vocabulary, and a tag is consequential exactly when it
//! lowers to a write or to a policy action (see
//! [`FlowEffect::is_consequential`](crate::FlowEffect::is_consequential)). That channel is not
//! decorative: `flow.write_db` and `model.invoke` are the two authorities the default policy floor
//! grants *without* approval, so an op declaring `write_db` or `model` and nothing else clears both
//! the gate and — until C-210 — the classifier. [`is_consequence_bearing_with_effects`] is the
//! complete predicate and the one every seam should call; [`is_consequence_bearing`] is the
//! effect-set half, kept separate only because `flux-spec` is on the frozen protocol line.
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
    Exemption {
        op: "git_hunks",
        invariants: &["I1"],
        reason: "`git diff` with a parser attached (C-92) — fixed argv, caller may only restrict \
                 it to a path and a context radius; splits the result into addressable hunks and \
                 changes nothing. Same grounds and same scope as `git_diff` above: I1 only, and \
                 its result tracks the working tree, so I3 still forbids `Idempotent`.",
    },
];

/// One allowlist entry: an op name, the invariants it is excused from, and why.
struct Exemption {
    /// The op's `ToolSpec::name`, matched exactly.
    op: &'static str,
    /// Invariant ids this op is excused from (`"I1"`, `"I2"`, `"I3"`).
    invariants: &'static [&'static str],
    /// Why the declaration is honest as it stands. Not "it was already like that".
    ///
    /// Read by [`the_allowlist_is_well_formed`](tests::the_allowlist_is_well_formed) rather than by
    /// the check itself: an exemption with no stated reason is the failure mode this whole story
    /// exists to prevent, so it is a build failure, not a silently unused field.
    #[cfg_attr(not(test), allow(dead_code))]
    reason: &'static str,
}

/// Whether a *call* reaches something whose consequence outlives it — the complete classification,
/// and **the one to reach for** (C-210).
///
/// An operation declares consequence through two independent channels, and reading only one is the
/// exact defect this function exists to close:
///
/// * **The effect set** — [`is_consequence_bearing`], the `spec.effects` / `spec.access` shape.
/// * **The semantic-effect tags** — [`declares_consequential_effect`], what the op says it *means*.
///
/// `semantic_effects` is the tag list from `flux_runtime::Tool::semantic_effects`; pass an empty
/// slice for a spec that carries none. Note it is an *instance* fact, not a catalog one — `web.fetch`
/// declares `write_db` only when a record sink is actually wired — so classify the tool you hold,
/// never a spec pulled from a catalog listing.
///
/// This is the exact negation of the spec-shape branch of `flux-flow`'s `gather_safe`, which is the
/// correspondence C-191's invariants rest on; the two must keep moving together.
pub fn is_consequence_bearing_with_effects(spec: &ToolSpec, semantic_effects: &[String]) -> bool {
    is_consequence_bearing(spec) || declares_consequential_effect(semantic_effects)
}

/// Whether any tag in `semantic_effects` names a consequence that outlives the call — the tag half
/// of [`is_consequence_bearing_with_effects`].
///
/// Delegates the classification to [`FlowEffect::is_consequential`](crate::FlowEffect::is_consequential)
/// so the vocabulary carries its consequence class exactly once. Tags are taken as plain strings for
/// the same reason the trait hook returns them that way — the tool seam must not need the language
/// crate — and an **unrecognized tag is not consequential**: it cannot be lowered, so it reaches no
/// host effect and demands no authority. A typo'd tag is caught where tags are validated, not by
/// silently escalating every op that carries one.
pub fn declares_consequential_effect(semantic_effects: &[String]) -> bool {
    semantic_effects
        .iter()
        .filter_map(|tag| crate::FlowEffect::from_tag(tag))
        .any(crate::FlowEffect::is_consequential)
}

/// Whether `spec`'s **effect set** reaches something whose consequence outlives the call — see the
/// module docs for the derivation. Public because the invariants are only legible alongside the
/// classification they are built on.
///
/// This is one of two channels. It cannot see a consequence declared only as a semantic effect — a
/// durable `write_db`, a billable `model` call — so prefer [`is_consequence_bearing_with_effects`]
/// wherever the tags are in reach, which at every gather-safety and coherence seam they are.
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
    // Both channels (C-210): the effect-set shape *and* the semantic tags. Reading only the first
    // let an op declare a durable `write_db` while keeping a read-only tier.
    let consequential = is_consequence_bearing_with_effects(spec, semantic_effects);
    // Name the channel the verdict actually came from. An op whose effect set is an innocuous
    // `[Read, Network]` and whose consequence is a `write_db` tag would otherwise be told its
    // "shape" is the problem, sending the reader to edit the one field that is already correct.
    let because = if is_consequence_bearing(spec) {
        format!(
            "declares effects {:?} / access {:?} — a consequence-bearing shape",
            spec.effects, spec.access
        )
    } else {
        format!(
            "declares the semantic effect(s) {:?} — a consequence that outlives the call, even \
             though its effect set {:?} is inert",
            semantic_effects
                .iter()
                .filter(|t| crate::FlowEffect::from_tag(t)
                    .is_some_and(crate::FlowEffect::is_consequential))
                .collect::<Vec<_>>(),
            spec.effects
        )
    };

    if consequential && spec.risk == Risk::Low && !exempt_from(&spec.name, "I1") {
        violations.push(format!(
            "I1 (risk floor): `{}` {because} — but carries `Risk::Low`, the tier the gather path, \
             the op cache, and the approval prompt all read as harmless",
            spec.name
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
            "I3 (repeatability floor): `{}` {because} — but claims `Idempotency::Idempotent`; use \
             `NonIdempotent`, or `Conditional` when repeating really is safe",
            spec.name
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

    /// C-210: the tag half, derived from `lower()` rather than a hand-kept list. Written as a total
    /// match over the vocabulary so a new variant cannot be added without classifying it.
    #[test]
    fn every_semantic_effect_tag_states_its_own_consequence_class() {
        use crate::FlowEffect::*;
        #[allow(deprecated)]
        for effect in [
            Pure,
            Read,
            Model,
            Network,
            WriteFile,
            WriteDb,
            SendExternal,
            Delete,
            Money,
            Calendar,
            HumanVisible,
        ] {
            let expected = match effect {
                // Reaches nothing that outlives the call. `Network` is the deliberate one: an
                // unread egress is already caught by the effect-set branch, and classifying it on
                // both channels would let them disagree about a single fact.
                Pure | Read | Network | HumanVisible => false,
                // Lowers to `Effect::Write`, or to a `flow.*` / `model.invoke` policy action —
                // needing a policy decision is what "consequence" means here.
                _ => true,
            };
            assert_eq!(
                effect.is_consequential(),
                expected,
                "`{}` is misclassified",
                effect.tag()
            );
        }
    }

    #[test]
    fn a_declared_consequential_tag_makes_a_read_shaped_spec_consequence_bearing() {
        // `[Read, Network]` — gather-safe on its effect set alone, which is exactly the shipped
        // `web.fetch` shape. The tag is the only thing that can catch it.
        let fetch = spec("site.fetch").with_effects(vec![Effect::Read, Effect::Network]);
        assert!(
            !is_consequence_bearing(&fetch),
            "the effect-set half is blind here"
        );

        let wired = vec!["write_db".to_string()];
        assert!(declares_consequential_effect(&wired));
        assert!(is_consequence_bearing_with_effects(&fetch, &wired));

        // Both floors fire on the composed predicate — `read_only()` supplies `Risk::Low` *and*
        // `Idempotent`, and a durable write honestly satisfies neither.
        let violations = metadata_violations(&fetch, &wired);
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(violations[0].starts_with("I1 (risk floor)"));
        assert!(violations[1].starts_with("I3 (repeatability floor)"));
        // The diagnostic names the channel the verdict came from. Blaming the `[Read, Network]`
        // effect set here would send the reader to edit the one field that is already correct.
        for v in &violations {
            assert!(
                v.contains("semantic effect(s) [\"write_db\"]"),
                "must point at the tag, not the shape: {v}"
            );
        }

        // Inert and unrecognized tags change nothing: an unknown tag lowers to no effect and no
        // authority, so it must not silently escalate the op.
        for tag in ["read", "pure", "human_visible", "network", "not_a_real_tag"] {
            let tags = vec![tag.to_string()];
            assert!(!declares_consequential_effect(&tags), "`{tag}`");
            assert!(
                !is_consequence_bearing_with_effects(&fetch, &tags),
                "`{tag}`"
            );
        }
    }

    #[test]
    fn a_read_shaped_filesystem_or_fetch_op_stays_low_risk() {
        let file_read = spec("read").with_effects(vec![Effect::Read, Effect::Filesystem]);
        assert!(metadata_violations(&file_read, &[]).is_empty());
        // A fetch — `Network` paired with `Read`. (The shipped `web.fetch` declares `Network`
        // alone, which this rule does flag; it sits outside the catalog this story gates. See the
        // follow-up noted in `docs/stories/C-191-*.md`.)
        let fetch = spec("some.fetch").with_effects(vec![Effect::Read, Effect::Network]);
        assert!(metadata_violations(&fetch, &[]).is_empty());
    }

    /// An allowlist entry is a claim that a declaration is honest as it stands. A claim with no
    /// stated reason, or excusing an invariant that does not exist, is exactly the kind of
    /// unexamined trust this module was written to remove — so it fails the build.
    #[test]
    fn the_allowlist_is_well_formed() {
        for entry in EXEMPT {
            assert!(
                entry.reason.trim().len() >= 20,
                "allowlist entry for `{}` needs a real justification, got {:?}",
                entry.op,
                entry.reason
            );
            assert!(
                !entry.invariants.is_empty(),
                "allowlist entry for `{}` excuses no invariant",
                entry.op
            );
            for id in entry.invariants {
                assert!(
                    matches!(*id, "I1" | "I2" | "I3"),
                    "allowlist entry for `{}` names unknown invariant {id:?}",
                    entry.op
                );
            }
        }
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

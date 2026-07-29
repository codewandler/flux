//! Registry-wide `ToolSpec` metadata-coherence gate (C-191).
//!
//! # The invariant set, in prose
//!
//! Approval gating is driven entirely by what each operation declares about itself — its
//! `effects`, its `risk`, its `idempotency`, its `access`. Those four are trusted and never
//! cross-checked, so an operation that starts from `ToolSpec::read_only()` (`Read` + `Risk::Low` +
//! `Idempotent`, an internally consistent preset) and *later gains a mutating effect* without
//! upgrading its tier compiles, ships, and clears a lower approval bar than it deserves. The field
//! design is sound; the failure mode is drift. This test is the gate against that drift.
//!
//! ## Vocabulary — why "non-`Read` effect" is not the right line
//!
//! `Effect` mixes **directional** effects (`Read` observes, `Write` mutates) with **carrier**
//! effects (`Filesystem`, `Network`, `Process`, `Browser`, `LocalSystem`), which name the host
//! resource reached and say nothing about direction. `read` declares `[Read, Filesystem]`. So a
//! literal "any non-`Read` effect must not be `Risk::Low`" would condemn every file read in the
//! catalog; that reading is rejected here in favour of one derived from the semantics.
//!
//! `Filesystem` and `Network` are bounded by an existing guard (the workspace jail, the egress
//! guard), and a `Network` carrier paired with `Read` and no `Write` is a fetch. `Process`,
//! `LocalSystem` and `Browser` are a consequence regardless of direction: they run a program of
//! the op's choosing, reach host state outside the jail, or drive a real session-bearing browser.
//!
//! > **A spec is *consequence-bearing*** when it declares no effects and names
//! > `AccessKind::Process` / `Connection` / `LocalSystem` (an undeclared op holding a code-running
//! > capability is not inert), or when it *does* declare effects and that set leaves
//! > `{Read, Filesystem, Network}` or names `Network` without `Read`.
//!
//! That is exactly the shape `flux-flow`'s `gather_safe` refuses to run during evidence gathering —
//! including the detail that the access branch applies only to an empty effect set. That detail
//! matters: `flux-plugin` projects `access` from the *plugin's* capabilities rather than the op's,
//! so every op of a `process`-capable plugin carries `AccessKind::Process` whether or not it runs
//! anything, and reading access as a consequence regardless of effects would condemn every read op
//! the `kubernetes` plugin ships.
//!
//! ## The three invariants
//!
//! * **I1 — risk floor.** A consequence-bearing spec must not declare `Risk::Low`. `Risk::Low` is
//!   the tier `gather_safe` reads as "runnable before approval", the op cache reads as
//!   "replayable", `RiskApprover` reads as "below the gate", and `PlanRisk::summary` renders
//!   verbatim into the sentence a human sees at the approval prompt. Claiming it while mutating
//!   understates the operation to the person approving it.
//! * **I2 — destructive floor.** A tool declaring the semantic effect `delete` or `money` must
//!   declare `Risk::Destructive`. Those are the two tags `AuthorityRequirement::is_destructive`
//!   recognizes, and `Risk::Destructive` is what forces approval unconditionally and raises the
//!   destructive badge in the plan preview. Over-declaring risk is safe, so there is no converse.
//! * **I3 — repeatability floor.** A consequence-bearing spec must not declare
//!   `Idempotency::Idempotent`. That word licenses the op cache to serve a stored result *instead
//!   of executing*. `Conditional` is the escape hatch for a mutation that genuinely is safe to
//!   repeat — a stated condition, not a loosened rule.
//!
//! Effect↔access coherence is deliberately **not** restated here: it is already enforced with
//! better diagnostics by `flux_runtime::authority_requirements_from_declaration`.
//!
//! ## Where each invariant is enforced
//!
//! The encoding lives once, in `flux_spec::coherence`, together with the allowlist for any op whose
//! declaration is legitimately an exception. It is applied at two places, for two different reasons:
//!
//! * **Built-in ops — this test.** They are first-party code with a build of ours to run, so a
//!   build-time gate is the right instrument, exactly as the story asks ("a gate that runs on every
//!   build").
//! * **Plugin-supplied ops — `flux_plugin`'s `validate_op_coherence`, at load.** A plugin's
//!   metadata is authored outside this repo and there is no compile-time list of plugin ops to
//!   walk, so the check has to sit on the seam every plugin op crosses. `plugin_declarations`
//!   below is this suite's copy of that boundary case.
//!
//! Deliberately **not** enforced inside `ToolRegistry::try_register_from`. Registration is not a
//! trust boundary in this runtime: it is the seam first-party test fixtures use to construct
//! *deliberately* incoherent specs and assert the downstream gates still hold — `flux-runtime`'s
//! `a_write_below_the_threshold_auto_approves` registers a `Risk::Low` write precisely to prove
//! `RiskApprover` auto-approves it. Refusing those at registration would delete the defence-in-depth
//! tests rather than strengthen them, and metadata coherence is not the layer those gates rely on.

use serde_json::json;

use flux_runtime::ToolRegistry;
use flux_spec::{metadata_violations, AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_tools::try_register_builtins;

/// Every built-in operation, walked through the same registry the agent surface is assembled from.
#[test]
fn every_registered_builtin_spec_is_metadata_coherent() {
    let mut registry = ToolRegistry::new();
    try_register_builtins(&mut registry).expect("built-ins register");
    assert!(
        !registry.names().is_empty(),
        "the built-in pack registered nothing — this test would pass vacuously"
    );

    let mut violations = Vec::new();
    for name in registry.names() {
        let tool = registry.get(&name).expect("named tool resolves");
        violations.extend(metadata_violations(&tool.spec(), &tool.semantic_effects()));
    }

    assert!(
        violations.is_empty(),
        "{} operation(s) declare an incoherent metadata combination:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

fn drifted_write_op() -> ToolSpec {
    // Exactly the drift the story names: an op that began as `read_only` (hence `Risk::Low` +
    // `Idempotent`) and gained a mutating effect without upgrading either.
    ToolSpec::read_only(
        "drifted.write",
        "overwrite a file",
        json!({"type": "object"}),
    )
    .with_effects(vec![Effect::Write, Effect::Filesystem])
    .with_access(vec![AccessKind::Filesystem])
}

/// Failing-first proof: a deliberately mis-declared spec (mutating effect, `Risk::Low`) is caught.
#[test]
fn a_mutating_op_holding_the_read_only_risk_class_is_caught() {
    let violations = metadata_violations(&drifted_write_op(), &[]);
    assert!(
        violations.iter().any(|v| v.starts_with("I1 ")),
        "the risk floor must reject a mutating op at `Risk::Low`: {violations:?}"
    );
    assert!(
        violations.iter().any(|v| v.starts_with("I3 ")),
        "the repeatability floor must reject a mutating op claiming `Idempotent`: {violations:?}"
    );

    // Upgrading the tier and the repeatability claim — the fix a drifted op actually needs — makes
    // the same spec coherent, so the rule is a floor and not a ban on mutation.
    let mut fixed = drifted_write_op().with_risk(Risk::Medium);
    fixed.idempotency = Idempotency::NonIdempotent;
    assert!(metadata_violations(&fixed, &[]).is_empty());
}

/// I2: an op that irreversibly deletes must carry the tier that forces approval, whatever else it
/// declares. `acme.purge` is `Risk::High` and `NonIdempotent` — coherent under I1 and I3, and still
/// understating itself.
#[test]
fn a_deleting_op_below_the_destructive_tier_is_caught() {
    let mut spec = ToolSpec::read_only("acme.purge", "purge a record", json!({"type": "object"}))
        .with_effects(vec![Effect::Network])
        .with_access(vec![AccessKind::Network])
        .with_risk(Risk::High);
    spec.idempotency = Idempotency::NonIdempotent;

    let violations = metadata_violations(&spec, &["delete".to_string()]);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].starts_with("I2 "), "{violations:?}");

    let raised = spec.with_risk(Risk::Destructive);
    assert!(metadata_violations(&raised, &["delete".to_string()]).is_empty());
}

/// The plugin trust boundary. A plugin's declaration is authored outside this repo and there is no
/// compile-time list of plugin ops to walk, so the invariants are applied at load — the seam every
/// plugin op crosses — by `flux_plugin`'s `validate_op_coherence`. This suite cannot depend on
/// `flux-plugin` (it sits above `flux-tools`), so what is asserted here is the property that seam
/// relies on: the *same* `flux_spec::coherence` encoding, driven by a declaration shaped like the
/// one a manifest projects.
///
/// The corresponding refusal at the seam itself is asserted in
/// `flux-plugin`'s `a_mis_declared_plugin_operation_is_refused_at_load`.
#[test]
fn plugin_declarations_are_held_to_the_same_invariants() {
    // The shape `plugin_tool_spec` projects for an op that declares no effects: the loader
    // defaults it to `[Process, Network]` precisely because a plugin op could reach either. A
    // manifest that then declares `risk = "low"` is understating itself, and this is the case a
    // hand-written manifest hits most easily.
    let mut projected = ToolSpec::read_only(
        "acme.deploy",
        "ship the current build",
        json!({"type": "object"}),
    )
    .with_effects(vec![Effect::Process, Effect::Network])
    .with_access(vec![AccessKind::Process]);
    projected.idempotency = Idempotency::NonIdempotent;

    let violations = metadata_violations(&projected, &[]);
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].starts_with("I1 "), "{violations:?}");
    assert!(violations[0].contains("acme.deploy"), "{violations:?}");
}

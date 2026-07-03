//! L-29: the gather-phase read-only gate (`OpRegistry::mutating_ops_in`) must reject any op whose
//! effects are not a subset of `{Read}` — not just `Write`/`Destructive` — mirroring the optimizer's
//! `is_readonly_op` (`flux_lang::optimize`) and the plan-approval path's own notion of "mutating"
//! (`accumulate_risk`, `crates/flux-flow/src/runtime.rs`). Before the fix, an advertised
//! `Network`/`Process`/`Browser`/`LocalSystem` op slips through a `gather: true` "read-only
//! orientation" round undetected — see `docs/stories/L-29-gather-effect-gate.md`.

use flux_flow::ast::Node;
use flux_flow::registry::OpRegistry;
use flux_runtime::ToolRegistry;

/// The builtins plus the reflexive ops (`plan`/`run_plan`/`op.register`), which are kept out of
/// `register_builtins` but still registered/resolvable tools — exactly the kind of advertised,
/// non-`Read`-effect op the gather gate must catch.
fn registry_with_reflect_ops() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    flux_tools::register_builtins(&mut reg);
    flux_tools::register_reflect(&mut reg);
    reg
}

/// The story's named failing-first test: a `gather: true` plan calling an advertised `Network`-effect
/// op (`plan`, `effects: vec![Effect::Network]`) must be flagged mutating. Today `mutating_ops_in`
/// only tests `Risk::Destructive`/`Effect::Write`, so `plan` (`Risk::Low`, no `Write`) is invisible to
/// it — this assertion fails (red) until the fix lands.
#[test]
fn network_effect_op_is_flagged_mutating() {
    let reg = registry_with_reflect_ops();
    let ops = OpRegistry::new(&reg);
    let body = vec![Node::Call {
        op: "plan".into(),
        args: vec![],
    }];
    assert_eq!(
        ops.mutating_ops_in(&body),
        vec!["plan".to_string()],
        "an advertised Network-effect op must be treated as mutating for gather-phase purposes"
    );
}

/// The same gap for a `[Process, LocalSystem]` op (`bash` — the story's other named example,
/// "cargo/shell"): `Risk::High`/no `Write` effect, so it too is invisible to the pre-fix gate.
#[test]
fn process_and_local_system_effect_op_is_flagged_mutating() {
    let reg = registry_with_reflect_ops();
    let ops = OpRegistry::new(&reg);
    let body = vec![Node::Call {
        op: "bash".into(),
        args: vec![],
    }];
    assert_eq!(
        ops.mutating_ops_in(&body),
        vec!["bash".to_string()],
        "an advertised Process/LocalSystem-effect op must be treated as mutating for gather-phase \
         purposes"
    );
}

/// A genuinely read-only op (`Effect::Read` alone, no companion effect) must still pass — the fix
/// must not overreach into ops whose only declared effect is `Read`.
#[test]
fn read_only_op_is_not_flagged_mutating() {
    let reg = registry_with_reflect_ops();
    let ops = OpRegistry::new(&reg);
    let body = vec![Node::Call {
        op: "read".into(),
        args: vec![Node::Lit {
            value: serde_json::json!("x"),
        }],
    }];
    assert!(
        ops.mutating_ops_in(&body).is_empty(),
        "a plain Effect::Read op must not be flagged mutating"
    );
}

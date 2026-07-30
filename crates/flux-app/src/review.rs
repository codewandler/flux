//! Strict review as a product surface (flux L-13; `docs/designs/strict-review-flows.md` "Phase 4").
//!
//! **The invariant this module exists to guarantee:** the `review_code` journey and the direct/CLI
//! flow path must produce the SAME [`flux_tools::cognition::ReviewReport`] for the same inputs. That
//! is guaranteed *structurally*, not by convention: [`STRICT_REVIEW_FLOW_SRC`] is the ONE checked-in
//! `examples/strict_review.flux` file (L-10/L-12's already-landed protocol — read-only context
//! gather, a bounded 3-role reviewer fan-out under `task`, then the deterministic `review.aggregate`
//! op), embedded verbatim via `include_str!`. [`strict_review_op`] parses that exact text once into a
//! [`DraftAst`] and wraps it — unmodified — as a [`CompositeOpDecl`] named `strict_review`. The CLI's
//! `flux review` runs the same source text directly as a bare flow (`FlowClient::run_flow`); the
//! `review_code` journey below calls the wrapped composite op. Both paths bottom out in the identical
//! `DraftAst` executing through the identical `Executor::dispatch` envelope — there is no second,
//! hand-maintained copy of the review protocol anywhere.

use flux_lang::ast::{DraftAst, SymbolName};
use flux_lang::program::{CompositeOpDecl, CompositeOpMeta, JourneyDecl, Module, Program};

use flux_core::{Error, Result};
use flux_spec::{Effect, Risk};

/// The strict-review protocol's native-text source — the SAME checked-in file L-10/L-12 landed and
/// `crates/flux-sdk/tests/strict_review.rs` drives directly. Embedded so both the `flux review` CLI
/// command and the `review_code` journey ship it in the binary (no filesystem dependency at runtime).
pub const STRICT_REVIEW_FLOW_SRC: &str = include_str!("../../../examples/strict_review.flux");

/// The three built-in reviewer roles the flow's `task` fan-out targets, embedded from the SAME
/// committed `.flux/agents/review-*.md` files (L-14). Built-in strict-review callers use these
/// immutable definitions; project roles with the same names are ordinary agent roles and cannot
/// replace this protocol. Without these in the binary, `flux review` failed "unknown role:
/// review-security" in every repo but this one — the "self-contained, works in any repo" claim
/// depends on the roles shipping alongside the flow.
pub const REVIEW_ROLE_SOURCES: &[(&str, &str)] = &[
    (
        "review-security",
        include_str!("../../../.flux/agents/review-security.md"),
    ),
    (
        "review-correctness",
        include_str!("../../../.flux/agents/review-correctness.md"),
    ),
    (
        "review-maintainability",
        include_str!("../../../.flux/agents/review-maintainability.md"),
    ),
];

/// The parsed built-in reviewer [`Role`](flux_agent::Role)s. Callers seed these into their role
/// registry.
pub fn builtin_review_roles() -> Vec<flux_agent::Role> {
    REVIEW_ROLE_SOURCES
        .iter()
        .map(|(name, src)| {
            flux_agent::try_parse_role(src, name)
                .expect("checked-in built-in reviewer role must have valid metadata")
        })
        .collect()
}

/// Parse [`STRICT_REVIEW_FLOW_SRC`] and wrap it as a `strict_review` [`CompositeOpDecl`] — the exact
/// same params/body a bare `flux flow run examples/strict_review.flux` would execute, just addressable
/// as a callable op from a journey. Fails only if the checked-in file itself fails to parse (it is
/// exercised directly by `crates/flux-sdk/tests/strict_review.rs`, so this should never happen at
/// runtime; callers still propagate the error rather than panicking).
pub fn strict_review_op() -> Result<CompositeOpDecl> {
    let ast: DraftAst = match Module::parse_str(STRICT_REVIEW_FLOW_SRC)
        .map_err(|e| Error::Other(format!("parse strict_review flow: {e}")))?
    {
        Module::Flow(ast) => ast,
        Module::Program(_) => {
            return Err(Error::Other(
                "examples/strict_review.flux must be a bare flow, not a program".into(),
            ))
        }
    };
    Ok(CompositeOpDecl {
        name: "strict_review".to_string(),
        params: ast.params.clone(),
        returns: ast.returns.clone(),
        meta: CompositeOpMeta {
            description: "Strict multi-reviewer code review: read-only context gather, a bounded \
                          3-role reviewer fan-out, then deterministic aggregation into a typed \
                          ReviewReport."
                .to_string(),
            // Matches what the body's real ops require (`analyze_composites`'s transitive-surface
            // check): `task` (the reviewer fan-out) is Medium risk; `git_status`/`git_diff`/
            // `read_many` read the filesystem/process (git) — declaring anything narrower fails
            // analysis with a clear diagnostic naming the missing risk/effect.
            risk: Risk::Medium,
            effects: vec![Effect::Read, Effect::Filesystem, Effect::Process],
            ..CompositeOpMeta::default()
        },
        body: ast,
    })
}

/// The `review_code` journey: pure plumbing (no review logic of its own) that reads `files` from the
/// triggering event's payload — seeded as `$files` by `App`'s `seed_payload` (each top-level payload
/// field binds to its own symbol; see `hello.flux`'s `echo` journey for the same pattern with `$text`)
/// — and hands it straight to the ONE shared `strict_review` op. `flux app run <program>` (with this
/// program declared) runs it via a trigger; the hermetic test drives it directly through
/// `App::deliver`.
pub fn review_code_journey() -> JourneyDecl {
    JourneyDecl {
        name: "review_code".to_string(),
        agent: None,
        flow: DraftAst {
            name: Some("review_code".to_string()),
            params: Vec::new(),
            returns: None,
            body: vec![flux_lang::ast::Node::Return {
                value: Box::new(flux_lang::ast::Node::Call {
                    op: "strict_review".to_string(),
                    args: vec![flux_lang::ast::Node::Var {
                        name: SymbolName("files".to_string()),
                    }],
                }),
            }],
        },
    }
}

/// Build the full checked-in strict-review app program: the `strict_review` composite op (wrapping
/// [`STRICT_REVIEW_FLOW_SRC`] unmodified), the `review_code` journey that calls it, and a `review`
/// trigger so `flux app run <program>` wakes the journey on a `review` event
/// (`App::deliver("review", json!({"files": [...]}))`).
pub fn strict_review_program() -> Result<Program> {
    let op = strict_review_op()?;
    let journey = review_code_journey();
    Ok(Program {
        triggers: vec![flux_lang::program::TriggerDecl {
            name: "on_review".to_string(),
            on: "review".to_string(),
            run: journey.name.clone(),
            agent: None,
        }],
        journeys: vec![journey],
        ops: vec![op],
        ..Program::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_review_op_wraps_the_checked_in_flow_verbatim() {
        let op = strict_review_op().expect("wraps");
        assert_eq!(op.name, "strict_review");
        assert_eq!(op.params.len(), 1);
        assert_eq!(op.params[0].name.0, "files");
        // The wrapped body IS the parsed checked-in file's AST — same statement count, not a
        // hand-copied re-derivation.
        let bare = match Module::parse_str(STRICT_REVIEW_FLOW_SRC).unwrap() {
            Module::Flow(ast) => ast,
            Module::Program(_) => panic!("must be a bare flow"),
        };
        assert_eq!(op.body.body.len(), bare.body.len());
    }

    #[test]
    fn builtin_review_roles_ship_the_three_reviewers_toolless() {
        // L-14: `flux review` must work in ANY repo — the three reviewer roles the flow's `task`
        // fan-out targets ship in the binary. Each is the committed `.flux/agents/review-*.md`
        // declaring `tools: []` (read-nothing reviewers).
        let roles = builtin_review_roles();
        let names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "review-security",
                "review-correctness",
                "review-maintainability"
            ]
        );
        for r in &roles {
            assert_eq!(
                r.tools.as_deref(),
                Some(&[][..]),
                "{}: reviewers are toolless by contract",
                r.name
            );
            assert!(
                !r.prompt.trim().is_empty(),
                "{}: prompt body must be embedded",
                r.name
            );
        }
    }

    #[test]
    fn strict_review_program_declares_the_journey_and_op() {
        let program = strict_review_program().expect("builds");
        assert_eq!(program.journeys.len(), 1);
        assert_eq!(program.journeys[0].name, "review_code");
        assert_eq!(program.ops.len(), 1);
        assert_eq!(program.ops[0].name, "strict_review");
        assert!(program.flow_named("review_code").is_some());
    }
}

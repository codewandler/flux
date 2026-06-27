//! The analyzer. M1 validates the single-`call` grammar: the operation must be registered. Later
//! milestones add full name / type / effect / bounded-loop checking over the whole AST, lowering a
//! [`DraftAst`](crate::ast::DraftAst) into a typed [`HirFlow`](crate::ast::HirFlow).

use std::collections::HashSet;

use crate::ast::{DraftAst, FlowEffect, HirFlow, Node};
use crate::opspec::OpCatalog;

/// A single analyzer diagnostic, suitable for UI display or feeding back into the compile/repair
/// loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Validate that `op` names a registered operation (the M1 single-call grammar). Returns the
/// collected diagnostics on failure.
pub fn analyze_call(op: &str, ops: &dyn OpCatalog) -> Result<(), Vec<Diagnostic>> {
    if ops.lookup(op).is_some() {
        Ok(())
    } else {
        Err(vec![Diagnostic::new(format!("unknown operation: `{op}`"))])
    }
}

/// Validate every operation referenced anywhere in a flow against the catalog (the M2 whole-flow
/// check; richer type/effect checking comes later). Returns aggregated diagnostics on failure.
pub fn analyze_flow(ast: &DraftAst, ops: &dyn OpCatalog) -> Result<(), Vec<Diagnostic>> {
    let mut diags = Vec::new();
    for node in &ast.body {
        check_node(node, ops, &mut diags);
    }
    if diags.is_empty() {
        Ok(())
    } else {
        Err(diags)
    }
}

/// Lower a `DraftAst` to a typed [`HirFlow`]: run the whole-flow analysis (op resolution, grammar,
/// bounded loops, call arity) and gather the flow's semantic effect set. Full type inference over
/// expressions is a later milestone; today the HIR carries the validated body plus the gathered
/// effects an authorizer/optimizer reasons over.
pub fn lower(ast: &DraftAst, ops: &dyn OpCatalog) -> Result<HirFlow, Vec<Diagnostic>> {
    analyze_flow(ast, ops)?;
    Ok(HirFlow {
        name: ast.name.clone(),
        params: ast.params.clone(),
        returns: ast.returns.clone(),
        body: ast.body.clone(),
        effects: gather_effects(&ast.body, ops),
    })
}

/// The semantic effects a flow declares or implies: each `bind`/`memo`'s declared `effect`, plus the
/// effects implied by the host ops it `call`s (mapped from their host-resource [`Effect`]s). Deduped,
/// in first-seen order.
fn gather_effects(body: &[Node], ops: &dyn OpCatalog) -> Vec<FlowEffect> {
    let mut acc: Vec<FlowEffect> = Vec::new();
    let push = |e: FlowEffect, acc: &mut Vec<FlowEffect>| {
        if !acc.contains(&e) {
            acc.push(e);
        }
    };
    for_each_node(body, &mut |node| match node {
        Node::Bind {
            effect: Some(e), ..
        }
        | Node::Memo {
            effect: Some(e), ..
        } => push(*e, &mut acc),
        Node::Call { op, .. } => {
            if let Some(sig) = ops.lookup(op) {
                for e in sig.effects {
                    if let Some(f) = host_effect_to_flow(e) {
                        push(f, &mut acc);
                    }
                }
            }
        }
        _ => {}
    });
    acc
}

/// Map a host-resource [`Effect`] back to a representative semantic [`FlowEffect`] for HIR effect
/// gathering. Host effects with no clean semantic counterpart (process/browser/local) are skipped.
fn host_effect_to_flow(e: flux_spec::Effect) -> Option<FlowEffect> {
    use flux_spec::Effect;
    match e {
        Effect::Read => Some(FlowEffect::Read),
        Effect::Write | Effect::Filesystem => Some(FlowEffect::WriteFile),
        Effect::Network => Some(FlowEffect::Network),
        Effect::Process | Effect::Browser | Effect::LocalSystem => None,
    }
}

/// Visit every node in `body` and all its nested bodies (depth-first, pre-order), invoking `f` on
/// each. A single generic traversal reused for effect gathering and future HIR passes.
fn for_each_node(body: &[Node], f: &mut impl FnMut(&Node)) {
    for node in body {
        f(node);
        match node {
            Node::Bind { value, .. } | Node::Memo { value, .. } => {
                for_each_node(std::slice::from_ref(value), f)
            }
            Node::When {
                cond,
                then,
                otherwise,
            } => {
                for_each_node(std::slice::from_ref(cond), f);
                for_each_node(then, f);
                for_each_node(otherwise, f);
            }
            Node::Unless { cond, body } => {
                for_each_node(std::slice::from_ref(cond), f);
                for_each_node(body, f);
            }
            Node::Repeat { until, body, .. } | Node::Loop { until, body, .. } => {
                if let Some(u) = until {
                    for_each_node(std::slice::from_ref(u), f);
                }
                for_each_node(body, f);
            }
            Node::Each { source, body, .. } => {
                for_each_node(std::slice::from_ref(source), f);
                for_each_node(body, f);
            }
            Node::Assert { cond, .. } => for_each_node(std::slice::from_ref(cond), f),
            Node::Pipe { steps, .. } => for_each_node(steps, f),
            Node::Seq { body, .. }
            | Node::Retry { body, .. }
            | Node::Confirm { body, .. }
            | Node::Throttle { body, .. }
            | Node::Debounce { body, .. } => for_each_node(body, f),
            Node::Try { body, handler, .. } => {
                for_each_node(body, f);
                for_each_node(handler, f);
            }
            Node::Parallel { branches } => {
                for b in branches {
                    for_each_node(&b.body, f);
                }
            }
            Node::Race { branches, .. } => {
                for b in branches {
                    for_each_node(&b.body, f);
                }
            }
            Node::Verify { cmd, expect, .. } => {
                for_each_node(std::slice::from_ref(cmd), f);
                for_each_node(std::slice::from_ref(expect), f);
            }
            Node::Return { value } => for_each_node(std::slice::from_ref(value), f),
            Node::Call { args, .. } => for_each_node(args, f),
            Node::Jq { input, .. } => for_each_node(std::slice::from_ref(input), f),
            Node::Parse { value, .. } => for_each_node(std::slice::from_ref(value), f),
            // Leaf nodes (no nested node bodies).
            Node::Await { .. }
            | Node::Peek { .. }
            | Node::Var { .. }
            | Node::Lit { .. }
            | Node::Thing { .. }
            | Node::Expr { .. }
            | Node::Fmt { .. }
            | Node::Ctx { .. }
            | Node::CtxAppend { .. } => {}
        }
    }
}

/// Recursively validate the operations in a node and its children.
fn check_node(node: &Node, ops: &dyn OpCatalog, diags: &mut Vec<Diagnostic>) {
    match node {
        Node::Call { op, args } => {
            match ops.lookup(op) {
                None => diags.push(Diagnostic::new(format!("unknown operation: `{op}`"))),
                Some(sig) => {
                    // Arity: positional args bind to `required ++ optional`; a lone object argument
                    // is the whole named input, so it is exempt (matches `runtime::map_args_to_input`).
                    let lone_object =
                        matches!(args.as_slice(), [Node::Lit { value }] if value.is_object());
                    let max = sig.required_params.len() + sig.optional_params.len();
                    if !lone_object && args.len() > max {
                        diags.push(Diagnostic::new(format!(
                            "op `{op}` accepts at most {max} argument(s) but {} were supplied",
                            args.len()
                        )));
                    }
                }
            }
            for a in args {
                check_node(a, ops, diags);
            }
        }
        Node::Bind { value, .. } => check_node(value, ops, diags),
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            check_node(cond, ops, diags);
            for n in then.iter().chain(otherwise) {
                check_node(n, ops, diags);
            }
        }
        Node::Repeat { until, body, .. } => {
            if let Some(u) = until {
                check_node(u, ops, diags);
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Each { source, body, .. } => {
            check_node(source, ops, diags);
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Assert { cond, .. } => check_node(cond, ops, diags),
        Node::Pipe { steps, .. } => {
            for s in steps {
                if !matches!(s, Node::Call { .. }) {
                    diags.push(Diagnostic::new("`pipe` steps must be `call` nodes"));
                }
                check_node(s, ops, diags);
            }
        }
        Node::Seq { body, .. } => {
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Memo { value, .. } => check_node(value, ops, diags),
        Node::Parallel { branches } => {
            let mut seen: HashSet<&str> = HashSet::new();
            for b in branches {
                if !seen.insert(b.name.0.as_str()) {
                    diags.push(Diagnostic::new(format!(
                        "duplicate `parallel` branch name `${}`",
                        b.name.0
                    )));
                }
                if body_contains_return(&b.body) {
                    diags.push(Diagnostic::new(
                        "`return` is not allowed inside a `parallel` branch",
                    ));
                }
                for n in &b.body {
                    check_node(n, ops, diags);
                }
            }
        }
        Node::Return { value } => check_node(value, ops, diags),
        Node::Retry { body, .. } => {
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Try { body, handler, .. } => {
            for n in body.iter().chain(handler) {
                check_node(n, ops, diags);
            }
        }
        Node::Confirm { body, .. } => {
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Race { branches, .. } => {
            let mut seen: HashSet<&str> = HashSet::new();
            for b in branches {
                if !seen.insert(b.name.0.as_str()) {
                    diags.push(Diagnostic::new(format!(
                        "duplicate `race` branch name `${}`",
                        b.name.0
                    )));
                }
                for n in &b.body {
                    check_node(n, ops, diags);
                }
            }
        }
        Node::Throttle {
            max, name, body, ..
        } => {
            if *max == 0 {
                diags.push(Diagnostic::new("`throttle` requires a non-zero `max`"));
            }
            if name.is_empty() {
                diags.push(Diagnostic::new("`throttle` requires a non-empty `name`"));
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Debounce { name, body, .. } => {
            if name.is_empty() {
                diags.push(Diagnostic::new("`debounce` requires a non-empty `name`"));
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Loop {
            until,
            body,
            for_ms,
            ..
        } => {
            if *for_ms == 0 {
                diags.push(Diagnostic::new(
                    "`loop` requires a non-zero `for_ms` (unbounded loops are rejected)",
                ));
            }
            if let Some(u) = until {
                check_node(u, ops, diags);
            }
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Unless { cond, body } => {
            check_node(cond, ops, diags);
            for n in body {
                check_node(n, ops, diags);
            }
        }
        Node::Verify { cmd, expect, .. } => {
            check_node(cmd, ops, diags);
            check_node(expect, ops, diags);
        }
        Node::Expr { vars, .. } => {
            for v in vars.values() {
                check_node(v, ops, diags);
            }
        }
        Node::Fmt { .. } => {}
        Node::Jq { input, .. } => check_node(input, ops, diags),
        Node::Parse { value, as_type } => {
            const VALID: &[&str] = &["f64", "i64", "bool", "json", "string"];
            if !VALID.contains(&as_type.as_str()) {
                diags.push(Diagnostic::new(format!(
                    "`parse` as_type must be one of f64/i64/bool/json/string, got `{as_type}`"
                )));
            }
            check_node(value, ops, diags);
        }
        Node::Ctx { budget, .. } => {
            if matches!(budget, Some(0)) {
                diags.push(Diagnostic::new(
                    "`ctx` budget must be non-zero (a 0-char budget drops every member)",
                ));
            }
        }
        Node::Await { .. }
        | Node::Peek { .. }
        | Node::Var { .. }
        | Node::Lit { .. }
        | Node::Thing { .. }
        | Node::CtxAppend { .. } => {}
    }
}

/// Whether any statement in `body` is (or reaches, through nested control flow) a `return`. Used to
/// reject `return` inside a `parallel` branch, where which branch's return should win is ambiguous.
/// A nested `parallel`'s own branches are validated separately, so their returns don't count here.
fn body_contains_return(body: &[Node]) -> bool {
    body.iter().any(node_contains_return)
}

fn node_contains_return(node: &Node) -> bool {
    match node {
        Node::Return { .. } => true,
        Node::When {
            then, otherwise, ..
        } => body_contains_return(then) || body_contains_return(otherwise),
        Node::Repeat { body, .. } => body_contains_return(body),
        Node::Each { body, .. } => body_contains_return(body),
        Node::Seq { body, .. } => body_contains_return(body),
        Node::Retry { body, .. } => body_contains_return(body),
        Node::Try { body, handler, .. } => {
            body_contains_return(body) || body_contains_return(handler)
        }
        Node::Confirm { body, .. } => body_contains_return(body),
        Node::Loop { body, .. } => body_contains_return(body),
        Node::Race { branches, .. } => branches.iter().any(|b| body_contains_return(&b.body)),
        Node::Throttle { body, .. } => body_contains_return(body),
        Node::Debounce { body, .. } => body_contains_return(body),
        Node::Unless { body, .. } => body_contains_return(body),
        Node::Expr { .. } | Node::Fmt { .. } | Node::Jq { .. } => false,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opspec::OpSignature;

    /// A minimal in-memory [`OpCatalog`] for analyzer tests — keeps this module free of any
    /// dependency on the concrete tool registry (`flux-runtime`/`flux-tools`).
    struct MockCatalog(Vec<String>);

    impl OpCatalog for MockCatalog {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            self.0
                .iter()
                .find(|n| n.as_str() == name)
                .map(|n| OpSignature {
                    name: n.clone(),
                    description: String::new(),
                    effects: Vec::new(),
                    risk: flux_spec::Risk::Low,
                    idempotency: flux_spec::Idempotency::Idempotent,
                    required_params: Vec::new(),
                    optional_params: Vec::new(),
                })
        }
    }

    /// The handful of op names the analyzer tests reference.
    fn catalog() -> MockCatalog {
        MockCatalog(vec!["read".into(), "grep".into(), "write".into()])
    }

    /// A richer catalog whose ops carry effects + params, for the HIR lowering / arity tests.
    struct TypedCatalog;
    impl OpCatalog for TypedCatalog {
        fn lookup(&self, name: &str) -> Option<OpSignature> {
            let sig = |effects, required: &[&str], optional: &[&str]| OpSignature {
                name: name.into(),
                description: String::new(),
                effects,
                risk: flux_spec::Risk::Low,
                idempotency: flux_spec::Idempotency::Idempotent,
                required_params: required.iter().map(|s| s.to_string()).collect(),
                optional_params: optional.iter().map(|s| s.to_string()).collect(),
            };
            match name {
                "read" => Some(sig(vec![flux_spec::Effect::Read], &["path"], &[])),
                "write" => Some(sig(
                    vec![flux_spec::Effect::Write, flux_spec::Effect::Filesystem],
                    &["path", "content"],
                    &[],
                )),
                _ => None,
            }
        }
    }

    #[test]
    fn lower_gathers_effects_and_arity_is_checked() {
        use crate::ast::{Node, TypeRef};
        let ops = TypedCatalog;

        let ast = DraftAst {
            body: vec![
                Node::Bind {
                    name: "x".into(),
                    value: Box::new(Node::Call {
                        op: "read".into(),
                        args: vec![Node::Lit {
                            value: serde_json::json!("a"),
                        }],
                    }),
                    ty: None,
                    // a declared semantic effect is gathered verbatim
                    effect: Some(FlowEffect::Model),
                },
                Node::Call {
                    op: "write".into(),
                    args: vec![
                        Node::Lit {
                            value: serde_json::json!("p"),
                        },
                        Node::Lit {
                            value: serde_json::json!("c"),
                        },
                    ],
                },
            ],
            ..Default::default()
        };
        let hir: HirFlow = lower(&ast, &ops).unwrap();
        // Read (from `read`) + WriteFile (from `write`) + Model (declared) — deduped.
        assert!(hir.effects.contains(&FlowEffect::Read));
        assert!(hir.effects.contains(&FlowEffect::WriteFile));
        assert!(hir.effects.contains(&FlowEffect::Model));
        let _ = TypeRef::Any;

        // Too many positional args for `read` (1 param) is rejected at analysis.
        let over = DraftAst {
            body: vec![Node::Call {
                op: "read".into(),
                args: vec![
                    Node::Lit {
                        value: serde_json::json!("a"),
                    },
                    Node::Lit {
                        value: serde_json::json!("b"),
                    },
                ],
            }],
            ..Default::default()
        };
        let err = lower(&over, &ops).unwrap_err();
        assert!(err.iter().any(|d| d.message.contains("at most 1 argument")));
    }

    #[test]
    fn known_op_passes_and_unknown_op_fails() {
        let ops = catalog();

        assert!(analyze_call("read", &ops).is_ok());

        let err = analyze_call("does.not.exist", &ops).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("unknown operation"));
    }

    #[test]
    fn analyze_flow_validates_nested_calls() {
        use crate::ast::{DraftAst, Node};
        let ops = catalog();

        let good = DraftAst {
            body: vec![Node::Call {
                op: "read".into(),
                args: vec![],
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&good, &ops).is_ok());

        let bad = DraftAst {
            body: vec![Node::Return {
                value: Box::new(Node::Call {
                    op: "nope.op".into(),
                    args: vec![],
                }),
            }],
            ..Default::default()
        };
        assert!(analyze_flow(&bad, &ops).is_err());
    }

    #[test]
    fn analyze_validates_nested_calls_in_new_containers() {
        use crate::ast::{Branch, DraftAst, Node};
        let ops = catalog();

        // An unknown op reached only through `each`/`parallel` bodies is still caught.
        let bad = DraftAst {
            body: vec![
                Node::Each {
                    source: Box::new(Node::Lit {
                        value: serde_json::json!([1]),
                    }),
                    item: "x".into(),
                    body: vec![Node::Call {
                        op: "nope.each".into(),
                        args: vec![],
                    }],
                    collect: None,
                    flat: false,
                },
                Node::Parallel {
                    branches: vec![Branch {
                        name: "b".into(),
                        body: vec![Node::Call {
                            op: "nope.par".into(),
                            args: vec![],
                        }],
                    }],
                },
            ],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops).unwrap_err();
        assert_eq!(diags.len(), 2, "both nested unknown ops are reported");
    }

    #[test]
    fn analyze_rejects_pipe_with_a_non_call_step() {
        use crate::ast::{DraftAst, Node};
        let ops = catalog();

        let bad = DraftAst {
            body: vec![Node::Pipe {
                steps: vec![Node::Lit {
                    value: serde_json::json!("x"),
                }],
                bind: None,
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops).unwrap_err();
        assert!(diags.iter().any(|d| d.message.contains("pipe")));
    }

    #[test]
    fn analyze_rejects_parallel_return_inside_unless() {
        use crate::ast::{Branch, DraftAst, Node};
        let ops = catalog();

        // A `return` nested inside an `unless` body that lives inside a `parallel`
        // branch must still be detected — the bug was that `node_contains_return`
        // had no arm for `Node::Unless`, so it fell through to `_ => false`.
        let bad = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![Branch {
                    name: "b".into(),
                    body: vec![Node::Unless {
                        cond: Box::new(Node::Lit {
                            value: serde_json::json!(false),
                        }),
                        body: vec![Node::Return {
                            value: Box::new(Node::Lit {
                                value: serde_json::json!(1),
                            }),
                        }],
                    }],
                }],
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("return")),
            "a return nested inside unless inside a parallel branch must be rejected"
        );
    }

    #[test]
    fn analyze_rejects_parallel_return_and_duplicate_branch_names() {
        use crate::ast::{Branch, DraftAst, Node};
        let ops = catalog();

        let bad = DraftAst {
            body: vec![Node::Parallel {
                branches: vec![
                    Branch {
                        name: "dup".into(),
                        body: vec![Node::Return {
                            value: Box::new(Node::Lit {
                                value: serde_json::json!(1),
                            }),
                        }],
                    },
                    Branch {
                        name: "dup".into(),
                        body: vec![Node::Call {
                            op: "read".into(),
                            args: vec![],
                        }],
                    },
                ],
            }],
            ..Default::default()
        };
        let diags = analyze_flow(&bad, &ops).unwrap_err();
        assert!(
            diags.iter().any(|d| d.message.contains("return")),
            "a return inside a parallel branch is rejected"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("duplicate")),
            "a duplicate branch name is rejected"
        );
    }
}

//! The analyzer. M1 validates the single-`call` grammar: the operation must be registered. Later
//! milestones add full name / type / effect / bounded-loop checking over the whole AST, lowering a
//! [`DraftAst`](crate::ast::DraftAst) into a typed [`HirFlow`](crate::ast::HirFlow).

use crate::ast::{DraftAst, Node};
use crate::registry::OpRegistry;

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
pub fn analyze_call(op: &str, registry: &OpRegistry) -> Result<(), Vec<Diagnostic>> {
    if registry.get(op).is_some() {
        Ok(())
    } else {
        Err(vec![Diagnostic::new(format!("unknown operation: `{op}`"))])
    }
}

/// Validate every operation referenced anywhere in a flow against the registry (the M2 whole-flow
/// check; richer type/effect checking comes later). Returns aggregated diagnostics on failure.
pub fn analyze_flow(ast: &DraftAst, registry: &OpRegistry) -> Result<(), Vec<Diagnostic>> {
    let mut diags = Vec::new();
    for node in &ast.body {
        check_node(node, registry, &mut diags);
    }
    if diags.is_empty() {
        Ok(())
    } else {
        Err(diags)
    }
}

/// Recursively validate the operations in a node and its children.
fn check_node(node: &Node, registry: &OpRegistry, diags: &mut Vec<Diagnostic>) {
    match node {
        Node::Call { op, args } => {
            if let Err(mut e) = analyze_call(op, registry) {
                diags.append(&mut e);
            }
            for a in args {
                check_node(a, registry, diags);
            }
        }
        Node::Bind { value, .. } => check_node(value, registry, diags),
        Node::When {
            cond,
            then,
            otherwise,
        } => {
            check_node(cond, registry, diags);
            for n in then.iter().chain(otherwise) {
                check_node(n, registry, diags);
            }
        }
        Node::Repeat { until, body, .. } => {
            if let Some(u) = until {
                check_node(u, registry, diags);
            }
            for n in body {
                check_node(n, registry, diags);
            }
        }
        Node::Return { value } => check_node(value, registry, diags),
        Node::Await { .. } | Node::Var { .. } | Node::Lit { .. } | Node::Thing { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_runtime::ToolRegistry;

    #[test]
    fn known_op_passes_and_unknown_op_fails() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        let ops = OpRegistry::new(&reg);

        assert!(analyze_call("read", &ops).is_ok());

        let err = analyze_call("does.not.exist", &ops).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("unknown operation"));
    }

    #[test]
    fn analyze_flow_validates_nested_calls() {
        use crate::ast::{DraftAst, Node};
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        let ops = OpRegistry::new(&reg);

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
}

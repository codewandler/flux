//! The analyzer. M1 validates the single-`call` grammar: the operation must be registered. Later
//! milestones add full name / type / effect / bounded-loop checking over the whole AST, lowering a
//! [`DraftAst`](crate::ast::DraftAst) into a typed [`HirFlow`](crate::ast::HirFlow).

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
}

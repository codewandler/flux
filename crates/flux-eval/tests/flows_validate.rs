//! Pin the checked-in example flows to the live op set: every op a flow calls must exist in a registry
//! built from `register_builtins` + `register_eval_ops` + the `task` tool, and the AST must
//! deserialize. This fails CI if a checked-in flow drifts from the registered ops.

use std::sync::Arc;

use flux_runtime::ToolRegistry;

#[test]
fn example_flows_validate_against_the_registry() {
    let mut reg = ToolRegistry::new();
    flux_tools::register_builtins(&mut reg);
    flux_eval::register_eval_ops(&mut reg);
    reg.register(Arc::new(flux_orchestrate::TaskTool));
    let ops = flux_flow::registry::OpRegistry::new(&reg);

    for path in [
        "../../examples/improve-tbench.flux",
        "../../examples/eval-smoke.flux",
        // Exercises the P1+P2 surface end-to-end: ctx/ctx_append nodes + the pure cognition ops
        // (need/gaps/sort/top/cite) + a Named artifact-type hint — all against the live registry.
        "../../examples/cognition-research.flux",
    ] {
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let ast: flux_flow::ast::DraftAst =
            serde_json::from_str(&src).unwrap_or_else(|e| panic!("parse {path} as DraftAst: {e}"));
        flux_flow::analyze::analyze_flow(&ast, &ops).unwrap_or_else(|diags| {
            panic!("{path} references unknown ops / is invalid: {diags:?}")
        });
    }
}

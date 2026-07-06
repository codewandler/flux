//! Pin the checked-in example flows to the live op set: every op a flow calls must exist in a registry
//! built from `register_builtins` + `register_eval_ops` + the `task` tool, the AST must
//! deserialize, and the flow must pass the SAME gate `flux flow run` applies — `lower`, not just
//! `analyze_flow` (L-16/F9: lower adds the required-param/type walk). This fails CI if a checked-in
//! flow drifts from the registered ops or their signatures. (Earned the hard way: `improve_log`
//! grew a required `record` param; `analyze_flow` alone let the stale flow pass CI while the
//! runtime refused to start the improve loop.)

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
        "../../examples/improve-synthetic.flux",
        "../../examples/eval-smoke.flux",
        // Exercises the P1+P2 surface end-to-end: ctx/ctx_append nodes + the pure cognition ops
        // (need/gaps/sort/top/cite) + a Named artifact-type hint — all against the live registry.
        "../../examples/cognition-research.flux",
    ] {
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let ast: flux_flow::ast::DraftAst =
            serde_json::from_str(&src).unwrap_or_else(|e| panic!("parse {path} as DraftAst: {e}"));
        flux_flow::analyze::lower(&ast, &ops, &Default::default()).unwrap_or_else(|diags| {
            panic!("{path} fails the flow-run gate (unknown ops / missing required params / type conflicts): {diags:?}")
        });
    }
}

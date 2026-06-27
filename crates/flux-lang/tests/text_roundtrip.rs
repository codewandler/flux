//! Round-trip the real checked-in example flow through the text syntax: `parse(format(ast)) == ast`
//! on a non-trivial AST (incl. `ctx`/`ctx_append`, a `Named` type hint, and nested `repeat`), not just
//! the hand-built ASTs in the unit tests.

use std::path::PathBuf;

#[test]
fn cognition_research_example_text_round_trips() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/cognition-research.flux");
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let ast: flux_lang::ast::DraftAst =
        serde_json::from_str(&src).expect("example parses as DraftAst");

    let text = flux_lang::format::format(&ast);
    let back = flux_lang::parse::parse(&text)
        .unwrap_or_else(|e| panic!("parse the formatted text: {e}\n--- text ---\n{text}"));

    assert_eq!(
        ast, back,
        "text round-trip changed the AST\n--- text ---\n{text}"
    );
}

/// The P6b control-flow primitives round-trip through the `@json` escape (no native grammar yet), so
/// `parse(format(ast)) == ast` still holds for a flow that uses every one of them.
#[test]
fn p6b_control_flow_nodes_round_trip() {
    use flux_lang::ast::{DraftAst, FallbackBranch, MatchCase, Node, RouteCase, SymbolName};
    let lit = |v: &str| Node::Lit {
        value: serde_json::json!(v),
    };
    let call = |op: &str| Node::Call {
        op: op.into(),
        args: vec![],
    };

    let ast = DraftAst {
        body: vec![
            Node::Match {
                subject: Box::new(lit("k")),
                cases: vec![MatchCase {
                    value: lit("k"),
                    body: vec![call("a")],
                }],
                default: vec![call("b")],
            },
            Node::Route {
                selector: Box::new(call("pick")),
                cases: vec![RouteCase {
                    label: "x".into(),
                    body: vec![call("a")],
                }],
                default: vec![],
            },
            Node::Fallback {
                branches: vec![FallbackBranch {
                    body: vec![call("a")],
                }],
                bind: Some(SymbolName("w".into())),
            },
            Node::Timeout {
                ms: 500,
                body: vec![call("a")],
                bind: None,
            },
            Node::Budget {
                limit: 3,
                body: vec![call("a")],
                bind: None,
            },
        ],
        ..Default::default()
    };

    let text = flux_lang::format::format(&ast);
    let back = flux_lang::parse::parse(&text)
        .unwrap_or_else(|e| panic!("parse the formatted text: {e}\n--- text ---\n{text}"));
    assert_eq!(
        ast, back,
        "round-trip changed the AST\n--- text ---\n{text}"
    );
}

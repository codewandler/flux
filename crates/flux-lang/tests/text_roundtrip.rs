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

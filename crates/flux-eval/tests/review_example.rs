//! Structural contract for the project-adaptive `examples/review.flux` flow (L-129).
//!
//! The important property is a data boundary, not wording: raw repository evidence may reach the
//! classifier, but the dimension-selection model may read only the resulting classification. The
//! fan-out is authored at four branches so a model cannot silently multiply delegated spend.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn review_ast() -> Value {
    let src = std::fs::read_to_string(repo_root().join("examples/review.flux"))
        .expect("examples/review.flux must exist");
    let ast = match flux_flow::program::Module::parse_str(&src).expect("review.flux parses") {
        flux_flow::program::Module::Flow(ast) => ast,
        flux_flow::program::Module::Program(_) => panic!("review.flux must be a single flow"),
    };
    serde_json::to_value(ast).expect("review AST serializes")
}

fn body(ast: &Value) -> &[Value] {
    ast["body"].as_array().expect("flow body is an array")
}

fn bound_value<'a>(body: &'a [Value], name: &str) -> &'a Value {
    body.iter()
        .find(|node| node["kind"] == "bind" && node["name"] == name)
        .unwrap_or_else(|| panic!("flow must bind `{name}`"))
        .get("value")
        .expect("bind has a value")
}

fn named_call_fields<'a>(node: &'a Value, op: &str) -> &'a serde_json::Map<String, Value> {
    assert_eq!(node["kind"], "call", "expected a call node: {node}");
    assert_eq!(node["op"], op, "expected `{op}` call: {node}");
    node["args"][0]["fields"]
        .as_object()
        .expect("named call has one object argument")
}

fn collect_vars(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if map.get("kind") == Some(&Value::String("var".into())) {
                if let Some(Value::String(name)) = map.get("name") {
                    out.insert(name.clone());
                }
            }
            map.values().for_each(|value| collect_vars(value, out));
        }
        Value::Array(items) => items.iter().for_each(|value| collect_vars(value, out)),
        _ => {}
    }
}

fn count_calls(value: &Value, op: &str) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(
                map.get("kind") == Some(&Value::String("call".into()))
                    && map.get("op") == Some(&Value::String(op.into())),
            ) + map
                .values()
                .map(|value| count_calls(value, op))
                .sum::<usize>()
        }
        Value::Array(items) => items.iter().map(|value| count_calls(value, op)).sum(),
        _ => 0,
    }
}

#[test]
fn review_flux_preserves_the_classification_only_boundary_and_bounded_fanout() {
    let ast = review_ast();
    let body = body(&ast);

    let project_context = body
        .iter()
        .find(|node| node["kind"] == "ctx" && node["name"] == "project_context")
        .expect("flow must build the classifier context explicitly");
    assert_eq!(
        project_context["include"],
        serde_json::json!(["files", "history"])
    );
    assert!(
        project_context["budget"].is_null(),
        "the requested full inventory must not be dropped by a context budget"
    );
    assert_eq!(count_calls(&ast, "glob"), 1);
    assert_eq!(count_calls(&ast, "git_log"), 1);

    let classifier = named_call_fields(bound_value(body, "classifications"), "ai.extract");
    assert_eq!(classifier["from"]["kind"], "var");
    assert_eq!(classifier["from"]["name"], "project_context");

    let dimension_call = bound_value(body, "dimensions");
    let dimensions = named_call_fields(dimension_call, "ai.extract");
    assert_eq!(dimensions["from"]["kind"], "var");
    assert_eq!(dimensions["from"]["name"], "classification");
    let mut dimension_inputs = BTreeSet::new();
    collect_vars(dimension_call, &mut dimension_inputs);
    assert_eq!(
        dimension_inputs,
        BTreeSet::from(["classification".to_string()]),
        "dimension derivation must not receive the file inventory, Git history, or context pack"
    );

    let parallel = body
        .iter()
        .find(|node| node["kind"] == "parallel" && count_calls(node, "task") > 0)
        .expect("flow must contain a reviewer parallel block");
    let branches = parallel["branches"]
        .as_array()
        .expect("parallel branches are an array");
    assert_eq!(
        branches.len(),
        4,
        "review fan-out must stay bounded at four"
    );
    for branch in branches {
        assert_eq!(
            count_calls(&branch["body"], "task"),
            1,
            "each reviewer branch delegates exactly one task"
        );
    }

    let synthesis = named_call_fields(bound_value(body, "verdict"), "task");
    assert_eq!(synthesis["role"]["value"], "review-synthesizer");
    assert_eq!(count_calls(&ast, "join"), 1);
    assert_eq!(count_calls(&ast, "task"), 5);
}

#[test]
fn review_roles_are_checked_in_and_read_only() {
    let reviewer_src = std::fs::read_to_string(repo_root().join(".flux/agents/review-project.md"))
        .expect("review-project role must ship with the example");
    let reviewer = flux_orchestrate::try_parse_role(&reviewer_src, "review-project")
        .expect("review-project role parses");
    assert_eq!(
        reviewer.tools,
        Some(vec![
            "read".into(),
            "grep".into(),
            "glob".into(),
            "file_stat".into(),
            "git_log".into(),
        ])
    );

    let synth_src = std::fs::read_to_string(repo_root().join(".flux/agents/review-synthesizer.md"))
        .expect("review-synthesizer role must ship with the example");
    let synth = flux_orchestrate::try_parse_role(&synth_src, "review-synthesizer")
        .expect("review-synthesizer role parses");
    assert_eq!(synth.tools, Some(Vec::new()));
}

//! Regression guard (D-31): no in-process `ToolSpec` op may hand-write its JSON Schema.
//!
//! Every op's `input_schema` must be derived from a typed Rust struct via
//! `flux_spec::tool_input_schema::<T>()` (schemars), so the schema and the runtime parsing
//! cannot drift. This test scans the op-defining source files and fails if an `input_schema`
//! is assigned a raw `json!({...})` / `serde_json::json!({...})` literal, or a positional
//! `read_only(name, desc, json!({...}))` / `pure_spec` / `proc_spec` helper call passing a
//! `json!` schema — the deprecated hand-written form.
//!
//! Out of scope (correctly not real `ToolSpec` op declarations, or deferred): provider MCP
//! passthrough `json!({"type":"object"})`, plugin example binaries, and the `flux-lang`
//! composite-op schema *generator* (`opspec.rs`). Plugin `OperationSpec` ops are tracked as
//! a separate deferred story (host-kit needs `read_op_typed`/`write_op_typed` helpers).

use std::path::PathBuf;

/// The op-defining source files whose `input_schema` must be schemars-derived.
const OP_FILES: &[&str] = &[
    "crates/flux-tools/src/lib.rs",
    "crates/flux-tools/src/extra.rs",
    "crates/flux-tools/src/cargo.rs",
    "crates/flux-tools/src/evidence.rs",
    "crates/flux-tools/src/reflect.rs",
    "crates/flux-tools/src/cognition.rs",
    "crates/flux-tools/src/toolchains.rs",
    "crates/flux-eval/src/ops.rs",
    "crates/flux-eval/src/git.rs",
    "crates/flux-eval/src/gate.rs",
    "crates/flux-eval/src/aggregate.rs",
    "crates/flux-orchestrate/src/lib.rs",
];

fn crate_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(rel)
}

#[test]
fn no_hand_written_input_schema_remains() {
    let mut offenders: Vec<String> = Vec::new();
    for rel in OP_FILES {
        let path = crate_path(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

        // Walk the file outside `#[cfg(test)]` blocks: a hand-written schema in test code is a
        // mock, not a real op declaration. Skip from the first `#[cfg(test)]` to end-of-file.
        let non_test = match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => &src[..],
        };

        for (lineno, line) in non_test.lines().enumerate() {
            let trimmed = line.trim();
            // The struct-field form: `input_schema: json!({...}),`
            // (covers both `json!` and `serde_json::json!`).
            if trimmed.starts_with("input_schema:")
                && (trimmed.contains("json!(") || trimmed.contains("json !("))
                && !trimmed.contains("tool_input_schema")
            {
                offenders.push(format!("{}:{}: {}", rel, lineno + 1, trimmed));
            }
            // The positional helper form: a `read_only(`/`pure_spec(`/`proc_spec(` call whose
            // schema argument is a `json!` literal. We flag lines that call these helpers and
            // pass `json!` (the typed form passes `tool_input_schema::<...>()`).
            for helper in ["read_only(", "pure_spec(", "proc_spec("] {
                if trimmed.contains(helper)
                    && (trimmed.contains("json!(") || trimmed.contains("json !("))
                    && !trimmed.contains("tool_input_schema")
                {
                    offenders.push(format!("{}:{}: {}", rel, lineno + 1, trimmed));
                }
            }
        }
    }

    if !offenders.is_empty() {
        panic!(
            "D-31 regression: hand-written input_schema json! literal(s) found (use \
             flux_spec::tool_input_schema::<T>() with a #[derive(Deserialize, JsonSchema)] struct \
             instead):\n  - {}\n\nOffenders:\n{}",
            offenders.len(),
            offenders
                .iter()
                .map(|o| format!("  {o}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

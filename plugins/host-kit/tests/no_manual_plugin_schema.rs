//! Regression guard (D-36): no migrated plugin may hand-write its op `input_schema` as a
//! `json!({...})` literal via the local `so(props, required)` helper. Every migrated plugin's
//! op schema must be derived from a typed struct via `host_kit::read_op_typed::<T>` /
//! `write_op_typed::<T>` (schemars), so the schema and the handler's field reads cannot drift.
//!
//! This is the plugin-side counterpart of `crates/flux-tools/tests/no_manual_schema.rs` (D-34).
//! It is **scoped to the migrated plugin set** (`MIGRATED_PLUGINS`): a plugin joins the set once
//! it has fully moved off `so(...)`. As more plugins migrate under D-36, add them here — the
//! guard then fails if a migrated plugin reintroduces a hand-written schema. Plugins not yet in
//! the set are left alone (their `so(...)` is the unmigrated state, tracked in the story).
//!
//! What this flags across every Rust source file in a migrated plugin's `src/` tree:
//!   - a local `fn so(` definition (the hand-written-schema helper must be deleted on migration),
//!   - any `so(` call passing a `json!` literal (the deprecated op-schema form).
//!
//! It does **not** flag `read_op_typed`/`write_op_typed` (the schemars-derived form) or the
//! unrelated `so` substring inside doc comments / string literals — the check is line-oriented
//! on the `fn so` def and on `so(` calls that also contain `json!`.

use std::path::{Path, PathBuf};

/// Plugins that have fully migrated to schemars-derived op schemas (D-36). Each is verified to
/// contain no hand-written `so(...)` op schemas. Grow this set as more plugins migrate; a plugin
/// must not be listed here until its `so` helper is deleted and all ops use `*_op_typed`.
const MIGRATED_PLUGINS: &[&str] = &[
    "homer",
    "gitlab",
    "slack",
    "sql",
    "asterisk",
    "grafana",
    "prometheus",
    "loki",
    "alertmanager",
];

/// Every first-party integration must make its executable contract choice explicit. Closed
/// contracts use `operation_typed`; intentionally open or transitional contracts use
/// `operation_flexible`. The deprecated `operation` alias must not hide that distinction.
const FIRST_PARTY_PLUGINS: &[&str] = &[
    "alertmanager",
    "asterisk",
    "aws",
    "confluence",
    "docker",
    "gitlab",
    "grafana",
    "homer",
    "huggingface",
    "jira",
    "kubernetes",
    "loki",
    "onepassword",
    "opsgenie",
    "prometheus",
    "slack",
    "sql",
    "vault",
    "websearch",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut pending = vec![dir.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read source directory {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("read source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    sources
}

fn non_test_source(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    }
}

#[test]
fn first_party_plugins_do_not_use_deprecated_value_only_registration() {
    let mut offenders = Vec::new();
    for name in FIRST_PARTY_PLUGINS {
        let src_dir = workspace_root().join(name).join("src");
        for path in rust_sources(&src_dir) {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let compact: String = src.chars().filter(|ch| !ch.is_whitespace()).collect();
            if !compact.contains(".operation(") {
                continue;
            }
            let detail = src.lines().enumerate().find_map(|(lineno, line)| {
                let compact_line: String = line.chars().filter(|ch| !ch.is_whitespace()).collect();
                compact_line
                    .contains(".operation(")
                    .then(|| format!("{}:{}: {}", path.display(), lineno + 1, line.trim()))
            });
            offenders.push(detail.unwrap_or_else(|| {
                format!(
                    "{}: deprecated registration spans multiple lines",
                    path.display()
                )
            }));
        }
    }

    if !offenders.is_empty() {
        panic!(
            "C-68 regression: deprecated value-only `.operation(...)` registration found in a \
             first-party plugin. Use `.operation_typed::<Input, Output>(...)` for a closed \
             contract, or `.operation_flexible(...)` for a documented open payload:\n\nOffenders:\n{}",
            offenders
                .iter()
                .map(|offender| format!("  {offender}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
fn no_hand_written_plugin_schema_remains() {
    let mut offenders: Vec<String> = Vec::new();
    for name in MIGRATED_PLUGINS {
        let src_dir = workspace_root().join(name).join("src");
        for path in rust_sources(&src_dir) {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

            // Walk only non-test code: a hand-written schema in tests is a mock, not a real op
            // declaration. Skip from the first `#[cfg(test)]` to end-of-file in each module.
            for (lineno, line) in non_test_source(&src).lines().enumerate() {
                let trimmed = line.trim();
                // The local `so` helper definition itself — must be deleted on migration.
                if trimmed.starts_with("fn so(") {
                    offenders.push(format!("{}:{}: {}", path.display(), lineno + 1, trimmed));
                    continue;
                }
                // A hand-written JSON Schema object literal — the deprecated form, whether passed
                // through a `so(...)` helper (gitlab/homer-style) or inlined directly into
                // `read_op`/`write_op` (slack-style `json!({"type":"object","properties":...})`).
                // Flag any `json!(` literal that looks like a schema object (has `"type"` and
                // `"object"`/`"properties"`), excluding the typed helpers.
                if (line.contains("json!(") || line.contains("json !("))
                    && !line.contains("read_op_typed")
                    && !line.contains("write_op_typed")
                    && (line.contains("\"type\"") || line.contains("'type'"))
                    && (line.contains("\"object\"") || line.contains("\"properties\""))
                {
                    offenders.push(format!("{}:{}: {}", path.display(), lineno + 1, trimmed));
                }
            }
        }
    }

    if !offenders.is_empty() {
        panic!(
            "D-36 regression: hand-written op input_schema via so(json!{{...}}) found in a \
             migrated plugin (use host_kit::read_op_typed::<T>() / write_op_typed::<T>() with a \
             #[derive(Deserialize, schemars::JsonSchema)] struct instead):\n\nOffenders:\n{}",
            offenders
                .iter()
                .map(|o| format!("  {o}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// A migrated plugin must actually use the typed helpers — guard against a partial migration
/// that deleted `so` but left ops on the raw `read_op(name, desc, value)` form.
#[test]
fn migrated_plugins_use_typed_op_helpers() {
    for name in MIGRATED_PLUGINS {
        let src_dir = workspace_root().join(name).join("src");
        let uses_typed_helpers = rust_sources(&src_dir).into_iter().any(|path| {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let non_test = non_test_source(&src);
            non_test.contains("read_op_typed::<") || non_test.contains("write_op_typed::<")
        });
        assert!(
            uses_typed_helpers,
            "D-36: migrated plugin `{name}` declares no typed op helpers \
             (read_op_typed::<T> / write_op_typed::<T>) — incomplete migration"
        );
    }
}

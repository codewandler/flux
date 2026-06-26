//! The normalized benchmark task format.
//!
//! A [`TaskSpec`] is benchmark-agnostic: a [`BenchmarkAdapter`](crate::adapter::BenchmarkAdapter)
//! either ships these directly (the `local`/`mock` adapters) or generates them from an external
//! benchmark's manifest (the terminal-bench / SWE-bench importers at M5). The runner materializes a
//! workspace from [`Setup`], runs flux against [`TaskSpec::prompt`], then grades [`TaskSpec::criterion`]
//! **outside** the agent so the agent cannot "pass" by editing its own grader.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The kind of task — used for weighting/reporting and for routing to the right importer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Coding,
    Dev,
    Engineering,
    Shell,
}

/// A single seed file written into a fresh workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeedFile {
    pub path: String,
    pub content: String,
}

/// How to materialize the workspace a task runs in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Setup {
    /// A bare empty directory (greenfield tasks).
    Empty,
    /// A set of inline seed files.
    Files { files: Vec<SeedFile> },
    /// Copy a checked-in fixture directory into the workspace.
    Copy { from: String },
    /// Clone a git repo at a revision (+ optional patch). Materialized by the external adapters (M5).
    GitRef {
        repo: String,
        rev: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patch: Option<String>,
    },
}

/// A verifiable pass/fail check, evaluated in the workspace after the agent finishes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Criterion {
    /// Run a command (argv, whitespace-tokenized — no shell) in the workspace; pass iff its exit
    /// code equals `expect_exit` (default 0).
    Command {
        run: String,
        #[serde(default)]
        expect_exit: i32,
    },
    /// A file must exist and (optionally) match. With multiple matchers, all must hold.
    FileContent {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        equals: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<String>,
    },
    /// All sub-criteria must pass.
    All { of: Vec<Criterion> },
}

fn default_weight() -> f64 {
    1.0
}

fn default_timeout() -> u64 {
    300
}

/// A single benchmark task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Stable, unique-within-a-suite id (e.g. `"coding/fix-failing-test"`).
    pub id: String,
    pub category: Category,
    #[serde(default = "default_weight")]
    pub weight: f64,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    pub setup: Setup,
    /// The instruction handed to flux verbatim.
    pub prompt: String,
    pub criterion: Criterion,
    /// Extra environment for the spawned flux child (e.g. `FLUX_MOCK_BASH` for the offline suite).
    /// These are caller-controlled, not model-controlled.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Per-task model override (e.g. `"mock"`); falls back to the suite/run default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_spec_toml_round_trips() {
        // NOTE: all top-level keys must precede any [table] header (TOML rule).
        let toml_src = r##"
id = "shell/count-todos"
category = "shell"
description = "Count TODO markers and write the number."
timeout_secs = 120
prompt = "Count TODO lines and write only that number to COUNT.txt."

[setup]
kind = "files"
files = [
  { path = "a.py", content = "x=1 # TODO\n" },
  { path = "b.py", content = "# TODO\n# TODO\n" },
]

[criterion]
kind = "file_content"
path = "COUNT.txt"
regex = '^\s*3\s*$'

[env]
FLUX_MOCK_BASH = "printf 3 > COUNT.txt"
"##;
        let spec: TaskSpec = toml::from_str(toml_src).unwrap();
        assert_eq!(spec.id, "shell/count-todos");
        assert_eq!(spec.category, Category::Shell);
        assert_eq!(spec.weight, 1.0); // default applied
        assert_eq!(spec.timeout_secs, 120);
        assert_eq!(
            spec.env.get("FLUX_MOCK_BASH").map(String::as_str),
            Some("printf 3 > COUNT.txt")
        );
        match &spec.setup {
            Setup::Files { files } => assert_eq!(files.len(), 2),
            other => panic!("unexpected setup: {other:?}"),
        }
        match &spec.criterion {
            Criterion::FileContent { path, regex, .. } => {
                assert_eq!(path, "COUNT.txt");
                assert!(regex.is_some());
            }
            other => panic!("unexpected criterion: {other:?}"),
        }
    }

    #[test]
    fn criterion_all_round_trips_via_json() {
        let c = Criterion::All {
            of: vec![
                Criterion::Command {
                    run: "cargo test --quiet".into(),
                    expect_exit: 0,
                },
                Criterion::FileContent {
                    path: "out.txt".into(),
                    equals: None,
                    contains: Some("ok".into()),
                    regex: None,
                },
            ],
        };
        let j = serde_json::to_value(&c).unwrap();
        let back: Criterion = serde_json::from_value(j).unwrap();
        assert_eq!(c, back);
    }
}

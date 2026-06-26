//! The `local` adapter: a set of [`TaskSpec`]s run by driving flux in a temp workspace.
//!
//! Its built-in `mock` suite ([`LocalAdapter::mock`]) needs no network or credentials — it drives the
//! offline `-m mock` provider — so the whole eval/improvement loop has a CI-able, deterministic
//! offline slice. Real local suites load from a directory of TOML task files ([`LocalAdapter::from_dir`]).

use async_trait::async_trait;

use flux_core::{Error, Result};

use crate::adapter::{BenchmarkAdapter, Filter, RunContext};
use crate::metrics::RunResult;
use crate::runner;
use crate::spec::{Category, Criterion, Setup, TaskSpec};

/// A directory- or code-defined suite of [`TaskSpec`]s.
pub struct LocalAdapter {
    name: String,
    tasks: Vec<TaskSpec>,
}

impl LocalAdapter {
    /// A suite from an explicit task list.
    pub fn new(name: impl Into<String>, tasks: Vec<TaskSpec>) -> Self {
        Self {
            name: name.into(),
            tasks,
        }
    }

    /// Load a suite from a directory of `*.toml` task files (recursively).
    pub fn from_dir(name: impl Into<String>, dir: impl AsRef<std::path::Path>) -> Result<Self> {
        let mut tasks = Vec::new();
        let mut stack = vec![dir.as_ref().to_path_buf()];
        while let Some(d) = stack.pop() {
            let rd = std::fs::read_dir(&d)
                .map_err(|e| Error::Other(format!("read suite dir {}: {e}", d.display())))?;
            for entry in rd {
                let path = entry.map_err(|e| Error::Other(e.to_string()))?.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                    let txt = std::fs::read_to_string(&path)
                        .map_err(|e| Error::Other(format!("read {}: {e}", path.display())))?;
                    let spec: TaskSpec = toml::from_str(&txt)
                        .map_err(|e| Error::Other(format!("parse {}: {e}", path.display())))?;
                    tasks.push(spec);
                }
            }
        }
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Self::new(name, tasks))
    }

    /// The built-in offline suite (drives `-m mock`; no network). Two passing tasks and one
    /// deliberately-unsatisfiable task, so a run exercises both pass and fail grading.
    pub fn mock() -> Self {
        let tasks = vec![
            TaskSpec {
                id: "mock/write-file".into(),
                category: Category::Coding,
                weight: 1.0,
                description: "The mock provider writes flux-mock.txt.".into(),
                timeout_secs: 60,
                setup: Setup::Empty,
                prompt: "Create the file.".into(),
                criterion: Criterion::FileContent {
                    path: "flux-mock.txt".into(),
                    equals: None,
                    contains: Some("created by flux mock".into()),
                    regex: None,
                },
                env: Default::default(),
                model: Some("mock".into()),
            },
            TaskSpec {
                id: "mock/bash-count".into(),
                category: Category::Shell,
                weight: 1.0,
                description: "Mock runs a bash command that writes COUNT.txt.".into(),
                timeout_secs: 60,
                setup: Setup::Empty,
                prompt: "Write the count to COUNT.txt.".into(),
                criterion: Criterion::FileContent {
                    path: "COUNT.txt".into(),
                    equals: None,
                    contains: None,
                    regex: Some(r"^\s*3\s*$".into()),
                },
                env: [(
                    "FLUX_MOCK_BASH".to_string(),
                    "printf 3 > COUNT.txt".to_string(),
                )]
                .into_iter()
                .collect(),
                model: Some("mock".into()),
            },
            TaskSpec {
                id: "mock/expected-fail".into(),
                category: Category::Coding,
                weight: 1.0,
                description: "Unsatisfiable by the mock provider — proves fail grading.".into(),
                timeout_secs: 60,
                setup: Setup::Empty,
                prompt: "Create the file.".into(),
                criterion: Criterion::FileContent {
                    path: "flux-mock.txt".into(),
                    equals: None,
                    contains: Some("this content never appears".into()),
                    regex: None,
                },
                env: Default::default(),
                model: Some("mock".into()),
            },
            TaskSpec {
                id: "mock/bash-fail".into(),
                category: Category::Shell,
                weight: 1.0,
                description: "Mock runs a failing bash command — produces a tool error to mine."
                    .into(),
                timeout_secs: 60,
                setup: Setup::Empty,
                prompt: "run it".into(),
                criterion: Criterion::FileContent {
                    path: "never.txt".into(),
                    equals: None,
                    contains: Some("nope".into()),
                    regex: None,
                },
                env: [("FLUX_MOCK_BASH".to_string(), "exit 3".to_string())]
                    .into_iter()
                    .collect(),
                model: Some("mock".into()),
            },
        ];
        Self::new("mock", tasks)
    }

    fn get(&self, id: &str) -> Option<&TaskSpec> {
        self.tasks.iter().find(|t| t.id == id)
    }
}

#[async_trait]
impl BenchmarkAdapter for LocalAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn list_tasks(&self, filter: &Filter) -> Result<Vec<String>> {
        let all: Vec<String> = self.tasks.iter().map(|t| t.id.clone()).collect();
        Ok(filter.select(&all))
    }

    fn weight_of(&self, task_id: &str) -> f64 {
        self.get(task_id).map(|t| t.weight).unwrap_or(1.0)
    }

    async fn run_task(&self, task_id: &str, ctx: &RunContext<'_>) -> Result<RunResult> {
        let spec = self
            .get(task_id)
            .ok_or_else(|| Error::Other(format!("unknown task `{task_id}`")))?;
        runner::run_local_task(spec, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_suite_lists_all_tasks() {
        let a = LocalAdapter::mock();
        let ids = a.list_tasks(&Filter::default()).unwrap();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"mock/write-file".to_string()));
        assert!(ids.contains(&"mock/bash-fail".to_string()));
    }

    #[test]
    fn checked_in_suite_parses() {
        // The repo's `suites/` must load (every task TOML parses). Path is relative to the crate dir.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../suites");
        let suite =
            LocalAdapter::from_dir("local", &dir).unwrap_or_else(|e| panic!("load suites/: {e}"));
        let ids = suite.list_tasks(&Filter::default()).unwrap();
        assert!(
            ids.iter().any(|id| id == "coding/fix-add"),
            "expected coding/fix-add in {ids:?}"
        );
        assert!(ids.iter().any(|id| id == "shell/count-todos"));
    }

    #[test]
    fn filter_selects_and_limits() {
        let a = LocalAdapter::mock();
        let one = a
            .list_tasks(&Filter {
                ids: vec!["mock/bash-count".into()],
                limit: 0,
            })
            .unwrap();
        assert_eq!(one, vec!["mock/bash-count".to_string()]);

        let capped = a
            .list_tasks(&Filter {
                ids: vec![],
                limit: 2,
            })
            .unwrap();
        assert_eq!(capped.len(), 2);
    }
}

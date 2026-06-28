//! The offline `mock` adapter: a set of [`TaskSpec`]s run by driving flux in a temp workspace.
//!
//! Its built-in `mock` suite ([`LocalAdapter::mock`]) needs no network or credentials — it drives the
//! offline `-m mock` provider — so the eval/improvement machinery has a CI-able, deterministic offline
//! slice (`examples/eval-smoke.flux`). Real benchmarks run through the terminal-bench adapter.

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

    /// The synthetic coding-riddle suite: short, self-contained problems with known answers, graded by
    /// running the produced script and matching its stdout. Unlike the offline `mock` suite this drives
    /// a *real* model and is a diagnostic workload (it surfaces tool-use friction, retry loops, etc.).
    /// Tasks assume `python3` on PATH; the criterion fails cleanly otherwise.
    pub fn synthetic() -> Self {
        let json = include_str!("../../assets/synthetic-suite.json");
        let tasks: Vec<TaskSpec> = serde_json::from_str(json)
            .expect("embedded synthetic-suite.json must be valid TaskSpecs");
        Self::new("synthetic", tasks)
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

    #[test]
    fn synthetic_suite_loads_all_riddles() {
        let a = LocalAdapter::synthetic();
        let ids = a.list_tasks(&Filter::default()).unwrap();
        assert_eq!(ids.len(), 16);
        assert!(ids.iter().all(|id| id.starts_with("synthetic/")));
        assert!(ids.contains(&"synthetic/two-sum".to_string()));
    }
}

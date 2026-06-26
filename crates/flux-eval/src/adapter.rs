//! The benchmark-adapter seam.
//!
//! The runner is benchmark-agnostic: it asks an adapter for task ids and to run one. The `local`
//! adapter ([`crate::adapters::local::LocalAdapter`]) ships [`TaskSpec`](crate::spec::TaskSpec)s and
//! runs flux in a temp workspace; the external adapters (terminal-bench, SWE-bench Lite) at M5 own
//! their Docker orchestration behind the same trait.

use std::path::Path;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use flux_core::Result;

use crate::metrics::RunResult;

/// Selects which tasks of an adapter to run.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Restrict to these task ids (empty = all).
    pub ids: Vec<String>,
    /// Cap the number of tasks (0 = no cap).
    pub limit: usize,
}

impl Filter {
    /// Apply the filter to a full id list, preserving order.
    pub fn select(&self, all: &[String]) -> Vec<String> {
        let mut out: Vec<String> = if self.ids.is_empty() {
            all.to_vec()
        } else {
            all.iter()
                .filter(|id| self.ids.contains(id))
                .cloned()
                .collect()
        };
        if self.limit > 0 && out.len() > self.limit {
            out.truncate(self.limit);
        }
        out
    }
}

/// What a single task run is given: the binary under test, the default model, and a cancel token.
pub struct RunContext<'a> {
    /// The flux binary to drive (the *rebuilt* one inside the improvement loop).
    pub flux_bin: &'a Path,
    /// Model used when a [`TaskSpec`](crate::spec::TaskSpec) doesn't override it.
    pub default_model: &'a str,
    pub cancel: &'a CancellationToken,
}

/// A source of benchmark tasks the runner can drive flux against.
#[async_trait]
pub trait BenchmarkAdapter: Send + Sync {
    /// Adapter name as used in a suite spec (`"mock"`, `"local"`, `"terminal-bench"`, …).
    fn name(&self) -> &str;

    /// All task ids this adapter can run (before filtering).
    fn list_tasks(&self, filter: &Filter) -> Result<Vec<String>>;

    /// The scoring weight of a task (default 1.0).
    fn weight_of(&self, _task_id: &str) -> f64 {
        1.0
    }

    /// Run one task and return its result.
    async fn run_task(&self, task_id: &str, ctx: &RunContext<'_>) -> Result<RunResult>;
}

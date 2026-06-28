//! The `multi` adapter: run several benchmark adapters behind **one combined score**.
//!
//! Each member's task ids are namespaced `"<member>:<id>"`, so a single eval can grade flux on, say,
//! the synthetic riddles *and* terminal-bench at once — the broadest signal, and the one least prone
//! to overfitting a single benchmark. The combined report also carries a per-member score breakdown
//! (computed in `ops::run_eval`) so the improvement loop can refuse a candidate that lifts the mean
//! while *regressing* one member (see `score_compare_multi`).

use async_trait::async_trait;

use flux_core::{Error, Result};

use crate::adapter::{BenchmarkAdapter, Filter, RunContext};
use crate::metrics::RunResult;

/// Several adapters behind one combined score. `subs` is `(member-name, adapter)`; the member name is
/// the routing prefix for task ids.
pub struct MultiAdapter {
    subs: Vec<(String, Box<dyn BenchmarkAdapter>)>,
}

impl MultiAdapter {
    pub fn new(subs: Vec<(String, Box<dyn BenchmarkAdapter>)>) -> Self {
        Self { subs }
    }

    /// Resolve a namespaced `"<member>:<id>"` task id to its owning adapter + the un-namespaced id.
    fn member_of<'s, 't>(
        &'s self,
        task_id: &'t str,
    ) -> Option<(&'s dyn BenchmarkAdapter, &'t str)> {
        let (member, rest) = task_id.split_once(':')?;
        let sub = self.subs.iter().find(|(n, _)| n == member)?;
        Some((sub.1.as_ref(), rest))
    }
}

#[async_trait]
impl BenchmarkAdapter for MultiAdapter {
    fn name(&self) -> &str {
        "multi"
    }

    fn list_tasks(&self, filter: &Filter) -> Result<Vec<String>> {
        // Union of every member's tasks, namespaced; the caller's filter applies to the namespaced ids.
        let mut all = Vec::new();
        for (member, sub) in &self.subs {
            for id in sub.list_tasks(&Filter::default())? {
                all.push(format!("{member}:{id}"));
            }
        }
        Ok(filter.select(&all))
    }

    fn weight_of(&self, task_id: &str) -> f64 {
        self.member_of(task_id)
            .map(|(sub, rest)| sub.weight_of(rest))
            .unwrap_or(1.0)
    }

    async fn prepare(&self, ctx: &RunContext<'_>) -> Result<()> {
        for (_, sub) in &self.subs {
            sub.prepare(ctx).await?;
        }
        Ok(())
    }

    async fn run_task(&self, task_id: &str, ctx: &RunContext<'_>) -> Result<RunResult> {
        let (sub, rest) = self
            .member_of(task_id)
            .ok_or_else(|| Error::Other(format!("multi: no member owns task `{task_id}`")))?;
        let mut r = sub.run_task(rest, ctx).await?;
        // Re-namespace so the combined report keeps members distinct (and per-member scoring can
        // partition cases by the `<member>:` prefix).
        r.task_id = task_id.to_string();
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::LocalAdapter;

    #[test]
    fn lists_namespaced_union_and_routes() {
        let m = MultiAdapter::new(vec![
            ("syn".into(), Box::new(LocalAdapter::synthetic())),
            ("mock".into(), Box::new(LocalAdapter::mock())),
        ]);
        let ids = m.list_tasks(&Filter::default()).unwrap();
        assert_eq!(ids.len(), 16 + 4);
        assert!(ids.contains(&"syn:synthetic/two-sum".to_string()));
        assert!(ids.contains(&"mock:mock/write-file".to_string()));
        // Routing: a namespaced id resolves to its member; an unknown one does not.
        assert!(m.member_of("syn:synthetic/two-sum").is_some());
        assert!(m.member_of("nope:x").is_none());
    }
}

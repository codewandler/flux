//! The `terminal-bench` adapter: real headroom for the self-improvement loop.
//!
//! flux is registered as a terminal-bench *custom agent* ([`bench/terminal_bench/flux_agent.py`]),
//! and `tb run` drives the Docker containers + grades (authoritative). This adapter shells out to
//! `tb run` for one task (one attempt) and parses its `results.json` into a [`RunResult`], so the
//! existing trials → [`CaseOutcome`](crate::metrics::CaseOutcome) → score path is unchanged.
//!
//! The binary the agent installs into each container is the **static musl** flux build
//! (`target/x86_64-unknown-linux-musl/release/flux`) — portable across task images. For the improve
//! loop, that musl binary must be rebuilt from the candidate source before the candidate eval (so the
//! benchmark measures the changed flux); the loop's flux_binary config points at it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result, Usage};
use flux_system::{System, Workspace};

use crate::adapter::{BenchmarkAdapter, Filter, RunContext};
use crate::metrics::RunResult;
use crate::util::str_field;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Drives terminal-bench via its `tb` CLI with flux as a custom agent.
pub struct TerminalBenchAdapter {
    tasks: Vec<String>,
    dataset: String,
    flux_binary: String,
    tb_bin: String,
    agent_import_path: String,
    pythonpath: String,
    timeout_secs: u64,
}

impl TerminalBenchAdapter {
    /// Build from an `eval_run` suite object: `{adapter:"terminal-bench", tasks:[...], flux_binary,
    /// dataset?, tb_bin?, agent_import_path?, pythonpath?, timeout_secs?}`. `flux_binary` (the static
    /// musl build) and at least one task are required.
    pub fn from_params(params: &Value) -> Result<Self> {
        let tasks: Vec<String> = params
            .get("tasks")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let flux_binary = str_field(params, "flux_binary")
            .ok_or_else(|| {
                Error::Other(
                    "terminal-bench: `flux_binary` (path to the static musl flux build) is required"
                        .to_string(),
                )
            })?
            .to_string();
        Ok(Self {
            tasks,
            dataset: str_field(params, "dataset")
                .unwrap_or("terminal-bench-core")
                .to_string(),
            flux_binary,
            tb_bin: str_field(params, "tb_bin").unwrap_or("tb").to_string(),
            agent_import_path: str_field(params, "agent_import_path")
                .unwrap_or("flux_agent:FluxAgent")
                .to_string(),
            pythonpath: str_field(params, "pythonpath")
                .unwrap_or("bench/terminal_bench")
                .to_string(),
            timeout_secs: params
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(1800),
        })
    }
}

/// Read tb's `results.json` (a `BenchmarkResults`) and pull this task's trial outcome.
fn parse_results(dir: &std::path::Path, task_id: &str) -> Option<(bool, u64, u64, Option<String>)> {
    // tb writes `<output>/<run-id>/results.json`; search a couple of levels for it.
    let mut candidates = vec![dir.join("results.json")];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                candidates.push(e.path().join("results.json"));
            }
        }
    }
    let path = candidates.into_iter().find(|p| p.exists())?;
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let results = json.get("results")?.as_array()?;
    // Prefer the entry matching this task; else the first.
    let entry = results
        .iter()
        .find(|r| r.get("task_id").and_then(|v| v.as_str()) == Some(task_id))
        .or_else(|| results.first())?;
    let resolved = entry.get("is_resolved").and_then(|v| v.as_bool()).unwrap_or(false);
    let input = entry.get("total_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let output = entry.get("total_output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let failure = entry
        .get("failure_mode")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "none" && *s != "unset")
        .map(String::from);
    Some((resolved, input, output, failure))
}

#[async_trait]
impl BenchmarkAdapter for TerminalBenchAdapter {
    fn name(&self) -> &str {
        "terminal-bench"
    }

    fn list_tasks(&self, filter: &Filter) -> Result<Vec<String>> {
        // terminal-bench is heavy (a Docker image per task), so we require explicit task ids rather
        // than auto-listing the whole dataset.
        let ids = if !filter.ids.is_empty() {
            filter.select(&filter.ids.clone())
        } else {
            filter.select(&self.tasks)
        };
        if ids.is_empty() {
            return Err(Error::Other(
                "terminal-bench: specify task ids via the suite `tasks` array or eval_run `tasks`"
                    .to_string(),
            ));
        }
        Ok(ids)
    }

    async fn run_task(&self, task_id: &str, ctx: &RunContext<'_>) -> Result<RunResult> {
        let started = Instant::now();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let out = std::env::temp_dir().join(format!("flux-tbench-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&out).map_err(|e| Error::Other(e.to_string()))?;

        let argv: Vec<String> = vec![
            self.tb_bin.clone(),
            "run".into(),
            "--dataset".into(),
            self.dataset.clone(),
            "--task-id".into(),
            task_id.to_string(),
            "--n-attempts".into(),
            "1".into(),
            "--agent-import-path".into(),
            self.agent_import_path.clone(),
            "--model".into(),
            ctx.default_model.to_string(),
            "--agent-kwarg".into(),
            format!("flux_binary={}", self.flux_binary),
            "--output-path".into(),
            out.to_string_lossy().to_string(),
            "--no-livestream".into(),
        ];

        // tb needs PATH (to find `tb`/`docker`), PYTHONPATH (to import the flux agent), and provider
        // creds. SAFE_ENV forwards PATH/HOME; augment PATH with ~/.local/bin (uv tool installs there).
        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!(
            "{}/.local/bin:{}",
            home,
            std::env::var("PATH").unwrap_or_default()
        );
        let mut env: Vec<(String, String)> = vec![
            ("PATH".into(), path),
            ("PYTHONPATH".into(), self.pythonpath.clone()),
        ];
        for key in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"] {
            if let Ok(v) = std::env::var(key) {
                env.push((key.into(), v));
            }
        }

        // tb runs in the repo/worktree root (it manages its own dataset cache + Docker).
        let cwd = std::env::current_dir().map_err(|e| Error::Other(e.to_string()))?;
        let sys = System::new(
            Workspace::new(&cwd).map_err(|e| Error::Other(format!("tb workspace: {e}")))?,
        );
        let run = sys
            .run_with_env(&argv, &env, Duration::from_secs(self.timeout_secs))
            .await;
        let wall_ms = started.elapsed().as_millis() as u64;

        match run {
            Err(e) => {
                let msg = e.to_string();
                let mut r = RunResult::failed(task_id, wall_ms, format!("tb run: {msg}"));
                r.timed_out = msg.contains("timed out");
                Ok(r)
            }
            Ok(output) => {
                if let Some((resolved, input, output_tok, failure)) = parse_results(&out, task_id) {
                    let tokens = if input + output_tok > 0 {
                        Some(Usage {
                            input_tokens: input,
                            output_tokens: output_tok,
                            ..Default::default()
                        })
                    } else {
                        None
                    };
                    Ok(RunResult {
                        task_id: task_id.to_string(),
                        passed: resolved,
                        iterations: 0,
                        tool_calls: 0,
                        tool_errors: 0,
                        tokens,
                        wall_ms,
                        session_id: None,
                        session_db: None,
                        flow_db: None,
                        timed_out: false,
                        note: failure,
                    })
                } else {
                    // No parseable results — surface tb's tail for debugging.
                    let tail: String = output
                        .stdout
                        .lines()
                        .chain(output.stderr.lines())
                        .rev()
                        .take(8)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    Ok(RunResult::failed(
                        task_id,
                        wall_ms,
                        format!("tb run: no results.json parsed (exit {}): {tail}", output.exit_code),
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_params_requires_flux_binary() {
        assert!(TerminalBenchAdapter::from_params(&serde_json::json!({"tasks": ["x"]})).is_err());
        let a = TerminalBenchAdapter::from_params(&serde_json::json!({
            "tasks": ["hello-world"], "flux_binary": "/bin/flux"
        }))
        .unwrap();
        assert_eq!(a.name(), "terminal-bench");
        assert_eq!(a.dataset, "terminal-bench-core");
    }

    #[test]
    fn parse_results_reads_is_resolved_and_tokens() {
        let dir = std::env::temp_dir().join(format!("tb-parse-test-{}", std::process::id()));
        let run = dir.join("2026-run-abc");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(
            run.join("results.json"),
            serde_json::json!({
                "results": [
                    {"task_id": "hello-world", "is_resolved": true,
                     "total_input_tokens": 1200, "total_output_tokens": 300,
                     "failure_mode": "none"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let (resolved, input, output, failure) = parse_results(&dir, "hello-world").unwrap();
        assert!(resolved);
        assert_eq!(input, 1200);
        assert_eq!(output, 300);
        assert!(failure.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}

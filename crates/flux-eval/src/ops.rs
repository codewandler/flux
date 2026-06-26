//! The core eval Flux-Lang ops: `eval_run`, `eval_sessions`, `painpoints_collect`, `eval_adopt`,
//! `score_compare`.
//!
//! Each op is a `flux_runtime::Tool`, so it runs through the same `Executor::dispatch` envelope as
//! every other operation — no new bypass surface. Op results are JSON encoded into the canonical
//! `content` string (what a flow binds to a `$symbol`); consumer ops parse their input back out.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_core::{Error, Result};
use flux_flow::state::FlowStore;
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};

use crate::adapter::{BenchmarkAdapter, Filter, RunContext};
use crate::adapters::LocalAdapter;
use crate::metrics::RunResult;
use crate::painpoint;
use crate::score::{report_is_better, SuiteScore};
use crate::util::{arg, json_result, str_field};

// ---------------------------------------------------------------------------
// eval_run
// ---------------------------------------------------------------------------

/// `eval_run(adapter)` — run a benchmark suite and return a report with per-case results + a score.
pub struct EvalRunTool;

#[async_trait]
impl Tool for EvalRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "eval_run".into(),
            description: "Run a benchmark suite against the flux binary and return a JSON report \
                          {adapter, pass_rate, scalar, total, passed, mean_*, cases:[…]}. \
                          `adapter` is \"mock\" (offline, built-in) or \"local\" (load *.toml from \
                          `dir`); external adapters land at M5."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "adapter": {"type": "string", "description": "mock | local | terminal-bench | swebench-lite"},
                    "tasks": {"type": "array", "items": {"type": "string"}, "description": "restrict to these task ids"},
                    "limit": {"type": "integer", "description": "cap the number of tasks (0 = all)"},
                    "model": {"type": "string", "description": "default model when a task doesn't override"},
                    "dir": {"type": "string", "description": "suite directory for the `local` adapter"},
                    "flux_bin": {"type": "string", "description": "path to the flux binary under test (default: current binary)"}
                },
                "required": ["adapter"]
            }),
            output_schema: None,
            effects: vec![Effect::Process, Effect::Read],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let adapter_name = str_field(&params, "adapter").unwrap_or("mock").to_string();
        let ids: Vec<String> = params
            .get("tasks")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let filter = Filter { ids, limit };
        let default_model = str_field(&params, "model").unwrap_or("sonnet").to_string();

        let adapter: Box<dyn BenchmarkAdapter> = match adapter_name.as_str() {
            "mock" => Box::new(LocalAdapter::mock()),
            "local" => {
                let dir = str_field(&params, "dir").ok_or_else(|| {
                    Error::Other("eval_run: the `local` adapter requires a `dir`".to_string())
                })?;
                Box::new(LocalAdapter::from_dir("local", dir)?)
            }
            other => {
                return Ok(ToolResult::error(format!(
                    "eval_run: adapter {other:?} is not available yet \
                     (terminal-bench / swebench-lite land at M5)"
                )))
            }
        };

        // Resolve the binary under test. A relative `flux_bin` (e.g. "target/debug/flux", the
        // *rebuilt* binary inside the improve loop) is made absolute against the current dir, since the
        // eval child runs with its cwd set to a task tempdir. Default: the running binary.
        let flux_bin: PathBuf = match str_field(&params, "flux_bin") {
            Some(p) => {
                let pb = PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else {
                    std::env::current_dir()
                        .map_err(|e| Error::Other(format!("eval_run: current dir: {e}")))?
                        .join(pb)
                }
            }
            None => std::env::current_exe()
                .map_err(|e| Error::Other(format!("eval_run: locate flux binary: {e}")))?,
        };

        let cancel = CancellationToken::new();
        let rc = RunContext {
            flux_bin: &flux_bin,
            default_model: &default_model,
            cancel: &cancel,
        };

        let task_ids = adapter.list_tasks(&filter)?;
        let mut results: Vec<RunResult> = Vec::with_capacity(task_ids.len());
        for id in &task_ids {
            let r = match adapter.run_task(id, &rc).await {
                Ok(r) => r,
                Err(e) => RunResult::failed(id, 0, e.to_string()),
            };
            results.push(r);
        }

        let score = SuiteScore::from_results(&results, |id| adapter.weight_of(id));
        let cases = serde_json::to_value(&results).map_err(|e| Error::Other(e.to_string()))?;
        let report = json!({
            "adapter": adapter.name(),
            "pass_rate": score.pass_rate,
            "scalar": score.scalar(),
            "total": score.total,
            "passed": score.passed,
            "mean_tool_errors": score.mean_tool_errors,
            "mean_iterations": score.mean_iterations,
            "mean_wall_ms": score.mean_wall_ms,
            "cases": cases,
        });
        let view = format!(
            "eval[{}] {}/{} passed · score {} · mean_iters {:.1} · mean_errors {:.1}",
            adapter.name(),
            score.passed,
            score.total,
            score.scalar(),
            score.mean_iterations,
            score.mean_tool_errors,
        );
        json_result(&report, view)
    }
}

// ---------------------------------------------------------------------------
// eval_sessions
// ---------------------------------------------------------------------------

/// `eval_sessions(report)` — project the per-case session references `[{id, db, task_id}]` out of an
/// `eval_run` report, so review/mining can consume them.
pub struct EvalSessionsTool;

#[async_trait]
impl Tool for EvalSessionsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "eval_sessions",
            "Extract the session references [{id, db, task_id}] from an eval_run report.",
            json!({
                "type": "object",
                "properties": { "report": {"type": "string", "description": "an eval_run report (JSON)"} },
                "required": ["report"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let report = arg(&params, "report");
        let empty = Vec::new();
        let cases = report
            .get("cases")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        let sessions: Vec<Value> = cases
            .iter()
            .filter_map(|c| {
                let id = c.get("session_id").and_then(|v| v.as_str())?;
                // The mining source is the RunEvent trace (flow.db), not the message log.
                let db = c.get("flow_db").and_then(|v| v.as_str())?;
                let task_id = c.get("task_id").and_then(|v| v.as_str()).unwrap_or(id);
                Some(json!({ "id": id, "db": db, "task_id": task_id }))
            })
            .collect();
        let view = format!("{} session(s)", sessions.len());
        json_result(&Value::Array(sessions), view)
    }
}

// ---------------------------------------------------------------------------
// painpoints_collect
// ---------------------------------------------------------------------------

/// `painpoints_collect(sessions)` — deterministically mine pain-points from the referenced sessions.
pub struct PainpointsCollectTool;

#[async_trait]
impl Tool for PainpointsCollectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "painpoints_collect",
            "Mine pain-points (tool errors, retry loops, missing tools, churn, …) from a list of \
             session references [{id, db}] and return them as JSON.",
            json!({
                "type": "object",
                "properties": { "sessions": {"type": "string", "description": "session references (JSON array)"} },
                "required": ["sessions"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let sessions = arg(&params, "sessions");
        let empty = Vec::new();
        let arr = sessions.as_array().unwrap_or(&empty);
        let mut all: Vec<painpoint::PainPoint> = Vec::new();
        for s in arr {
            let (Some(id), Some(db)) = (
                s.get("id").and_then(|v| v.as_str()),
                s.get("db").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let task_id = s.get("task_id").and_then(|v| v.as_str()).unwrap_or(id);
            let Ok(store) = FlowStore::open(db) else {
                continue;
            };
            let events = store.events(id).unwrap_or_default();
            all.extend(painpoint::mine(task_id, &events));
        }
        let view = format!("{} pain-point(s) mined", all.len());
        let content = serde_json::to_string(&all).map_err(|e| Error::Other(e.to_string()))?;
        Ok(ToolResult::ok_view(content, view))
    }
}

// ---------------------------------------------------------------------------
// eval_adopt
// ---------------------------------------------------------------------------

/// `eval_adopt(report)` — identity over a report. Lets a `when`/`then` branch end on a `call` (the AST
/// has no bare assignment) when adopting the candidate report as the new baseline.
pub struct EvalAdoptTool;

#[async_trait]
impl Tool for EvalAdoptTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "eval_adopt",
            "Return an eval report unchanged (used to re-bind the baseline after adopting a candidate).",
            json!({
                "type": "object",
                "properties": { "report": {"type": "string", "description": "an eval_run report (JSON)"} },
                "required": ["report"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let report = arg(&params, "report");
        let scalar = report.get("scalar").and_then(|v| v.as_u64()).unwrap_or(0);
        json_result(&report, format!("baseline ← candidate (score {scalar})"))
    }
}

// ---------------------------------------------------------------------------
// eval_scalar
// ---------------------------------------------------------------------------

/// `eval_scalar(report)` — the report's integer score scalar as a plain string (e.g. `"667"`), for
/// embedding in a commit message or tag name via `{{...}}`.
pub struct EvalScalarTool;

#[async_trait]
impl Tool for EvalScalarTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "eval_scalar",
            "Return an eval report's score scalar as a plain string (e.g. \"667\").",
            json!({
                "type": "object",
                "properties": { "report": {"type": "string", "description": "an eval_run report (JSON)"} },
                "required": ["report"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let report = arg(&params, "report");
        let scalar = report.get("scalar").and_then(|v| v.as_u64()).unwrap_or(0);
        Ok(ToolResult::ok(scalar.to_string()))
    }
}

// ---------------------------------------------------------------------------
// score_compare
// ---------------------------------------------------------------------------

/// `score_compare(baseline, candidate)` — `"true"` iff `candidate` is strictly better than `baseline`
/// (lexicographic: pass-rate, then fewer tool-errors, then fewer iterations). The string boolean is
/// read by a `when` condition's truthiness.
pub struct ScoreCompareTool;

#[async_trait]
impl Tool for ScoreCompareTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "score_compare",
            "Return \"true\" iff the candidate eval report is strictly better than the baseline.",
            json!({
                "type": "object",
                "properties": {
                    "baseline": {"type": "string", "description": "baseline eval_run report (JSON)"},
                    "candidate": {"type": "string", "description": "candidate eval_run report (JSON)"}
                },
                "required": ["baseline", "candidate"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let baseline = arg(&params, "baseline");
        let candidate = arg(&params, "candidate");
        let better = report_is_better(&candidate, &baseline);
        let view = format!(
            "candidate {} baseline (cand {} vs base {})",
            if better { "BEATS" } else { "does not beat" },
            candidate
                .get("scalar")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            baseline.get("scalar").and_then(|v| v.as_u64()).unwrap_or(0),
        );
        // Canonical content is a string boolean so `when` reads it via json_truthy.
        Ok(ToolResult::ok_view(
            if better { "true" } else { "false" },
            view,
        ))
    }
}

// ---------------------------------------------------------------------------
// change_implement
// ---------------------------------------------------------------------------

/// `change_implement(tasks)` — spawn a `worker` sub-agent per derived task to implement it.
///
/// This is an op (not `each { task(...) }`) because op results are stored as JSON **strings**, so
/// `each` can't iterate a model-produced task list. The op parses the list and drives the workers via
/// `ctx.spawner` (the same seam `task` uses); each worker is scoped + non-destructive (SubAgentApprover).
pub struct ChangeImplementTool;

#[async_trait]
impl Tool for ChangeImplementTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "change_implement".into(),
            description: "Implement each derived task by spawning a `worker` sub-agent; returns a \
                          per-task summary. Input is a JSON array of tasks (objects or strings)."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": { "tasks": {"type": "string", "description": "tasks to implement (JSON array)"} },
                "required": ["tasks"]
            }),
            output_schema: None,
            effects: Vec::new(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let tasks = crate::aggregate::extract_array(&arg(&params, "tasks"));
        let Some(spawner) = &ctx.spawner else {
            return Ok(ToolResult::error(
                "change_implement: no sub-agent spawner configured",
            ));
        };
        let cancel = CancellationToken::new();
        let mut results: Vec<Value> = Vec::with_capacity(tasks.len());
        for t in &tasks {
            // A task may be an object {task, ...} or a bare string.
            let desc = t
                .get("task")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    t.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| t.to_string())
                });
            let prompt = format!(
                "Implement exactly this task and nothing else, then report what you changed:\n{desc}"
            );
            match spawner.spawn("worker", &prompt, &cancel).await {
                Ok(text) => results.push(json!({ "task": desc, "ok": true, "result": text })),
                Err(e) => {
                    results.push(json!({ "task": desc, "ok": false, "error": e.to_string() }))
                }
            }
        }
        let view = format!("implemented {} task(s)", results.len());
        json_result(
            &json!({ "implemented": results.len(), "results": results }),
            view,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_specs_have_expected_names() {
        assert_eq!(EvalRunTool.spec().name, "eval_run");
        assert_eq!(EvalSessionsTool.spec().name, "eval_sessions");
        assert_eq!(PainpointsCollectTool.spec().name, "painpoints_collect");
        assert_eq!(EvalAdoptTool.spec().name, "eval_adopt");
        assert_eq!(ScoreCompareTool.spec().name, "score_compare");
    }

    #[test]
    fn eval_sessions_projects_refs_from_a_report() {
        let report = json!({
            "cases": [
                {"task_id": "t/a", "session_id": "s_1", "flow_db": "/tmp/x/.flux/flow.db", "pass": true},
                {"task_id": "t/b", "session_id": null, "flow_db": null, "pass": false}
            ]
        });
        let params = json!({ "report": report.to_string() });
        let report_v = arg(&params, "report");
        let cases = report_v.get("cases").and_then(|v| v.as_array()).unwrap();
        let refs: Vec<_> = cases
            .iter()
            .filter_map(|c| {
                let id = c.get("session_id").and_then(|v| v.as_str())?;
                let db = c.get("flow_db").and_then(|v| v.as_str())?;
                Some((id.to_string(), db.to_string()))
            })
            .collect();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, "s_1");
    }
}

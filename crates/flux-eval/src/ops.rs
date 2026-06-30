//! The core eval Flux-Lang ops: `eval_run`, `eval_sessions`, `painpoints_collect`, `eval_adopt`,
//! `score_compare`.
//!
//! Each op is a `flux_runtime::Tool`, so it runs through the same `Executor::dispatch` envelope as
//! every other operation — no new bypass surface. Op results are JSON encoded into the canonical
//! `content` string (what a flow binds to a `$symbol`); consumer ops parse their input back out.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_core::{Error, Result};
use flux_events::EventStore;
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{tool_input_schema, AccessKind, Effect, Idempotency, Risk, ToolSpec};

use crate::adapter::{BenchmarkAdapter, Filter, RunContext};
use crate::adapters::LocalAdapter;
use crate::metrics::{CaseOutcome, RunResult};
use crate::painpoint;
use crate::score::{report_is_better, SuiteScore};
use crate::util::{arg, json_result, str_field};

// ---------------------------------------------------------------------------
// eval_run
// ---------------------------------------------------------------------------

/// `eval_run(adapter)` — run a benchmark suite and return a report with per-case results + a score.
pub struct EvalRunTool;

/// Arguments for the `eval_run` op. `adapter` selects the suite; the remaining keys are read by
/// `run_eval` and the adapters it builds. Note: this op accepts adapter-specific keys not enumerated
/// here (e.g. terminal-bench's `flux_binary`/`dataset`/…, `multi`'s `members`), so it is intentionally
/// an open object (no `deny_unknown_fields`).
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct EvalRunInput {
    /// mock | synthetic | terminal-bench | multi (swebench-lite lands later)
    #[allow(dead_code)]
    adapter: String,
    /// restrict to these task ids
    #[serde(default)]
    #[allow(dead_code)]
    tasks: Option<Vec<String>>,
    /// cap the number of tasks (0 = all)
    #[serde(default)]
    #[allow(dead_code)]
    limit: Option<u64>,
    /// default model when a task doesn't override
    #[serde(default)]
    #[allow(dead_code)]
    model: Option<String>,
    /// path to the flux binary under test (default: current binary)
    #[serde(default)]
    #[allow(dead_code)]
    flux_bin: Option<String>,
}

#[async_trait]
impl Tool for EvalRunTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "eval_run".into(),
            description: "Run a benchmark suite against the flux binary and return a JSON report \
                          {adapter, pass_rate, scalar, total, passed, mean_*, cases:[…]}. \
                          `adapter` is \"mock\" (offline fixture), \"synthetic\" (real-model coding \
                          riddles), \"terminal-bench\" (the real Docker benchmark), or \"multi\" \
                          (several behind one combined score); swebench-lite lands later."
                .into(),
            input_schema: tool_input_schema::<EvalRunInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::Read],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let report = run_eval(params).await?;
        let view = report_view(&report);
        json_result(&report, view)
    }
}

/// Run a benchmark suite and return its JSON report — shared by the `eval_run` op and the `flux eval`
/// CLI so both drive the exact same adapters + scoring. `params` mirrors the op's input schema
/// (`adapter`, `tasks`, `limit`, `model`, `flux_bin`, `trials`, plus adapter-specific keys).
pub async fn run_eval(params: Value) -> Result<Value> {
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

    let adapter: Box<dyn BenchmarkAdapter> = if adapter_name == "multi" {
        let members = params
            .get("members")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut subs: Vec<(String, Box<dyn BenchmarkAdapter>)> = Vec::new();
        for m in &members {
            let name = m.get("adapter").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::Other("eval_run: each multi member needs an `adapter`".into())
            })?;
            subs.push((name.to_string(), build_adapter(name, m)?));
        }
        if subs.is_empty() {
            return Err(Error::Other(
                "eval_run: adapter \"multi\" needs a non-empty `members` list".into(),
            ));
        }
        Box::new(crate::adapters::MultiAdapter::new(subs))
    } else {
        build_adapter(&adapter_name, &params)?
    };

    // Resolve the binary under test. A relative `flux_bin` (e.g. "target/debug/flux", the *rebuilt*
    // binary inside the improve loop) is made absolute against the current dir, since the eval child
    // runs with its cwd set to a task tempdir. Default: the running binary.
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

    let watch = params
        .get("watch")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let cancel = CancellationToken::new();
    let rc = RunContext {
        flux_bin: &flux_bin,
        default_model: &default_model,
        cancel: &cancel,
        watch,
    };

    // Trials per task: >1 averages out single-run model noise so a "win" is real, not luck.
    let trials = params
        .get("trials")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as usize;

    // Per-eval setup (e.g. the terminal-bench adapter rebuilds the static musl binary so a candidate
    // eval reflects the worker's edits).
    adapter
        .prepare(&rc)
        .await
        .map_err(|e| Error::Other(format!("eval_run: adapter prepare failed: {e}")))?;

    let task_ids = adapter.list_tasks(&filter)?;
    let mut cases: Vec<CaseOutcome> = Vec::with_capacity(task_ids.len());
    for id in &task_ids {
        let mut runs: Vec<RunResult> = Vec::with_capacity(trials);
        for _ in 0..trials {
            let r = match adapter.run_task(id, &rc).await {
                Ok(r) => r,
                Err(e) => RunResult::failed(id, 0, e.to_string()),
            };
            runs.push(r);
        }
        cases.push(CaseOutcome::from_trials(id, &runs));
    }

    let score = SuiteScore::from_cases(&cases, |id| adapter.weight_of(id));
    let cases_json = serde_json::to_value(&cases).map_err(|e| Error::Other(e.to_string()))?;
    let mut report = json!({
        "adapter": adapter.name(),
        "trials": trials,
        "pass_rate": score.pass_rate,
        "mean_check_pass_rate": score.mean_check_pass_rate,
        "scalar": score.scalar(),
        "total": score.total,
        "passed": score.passed,
        "mean_tool_errors": score.mean_tool_errors,
        "mean_iterations": score.mean_iterations,
        "mean_tokens": score.mean_tokens,
        "mean_wall_ms": score.mean_wall_ms,
        "cases": cases_json,
    });
    // For a combined run, attach the per-member score breakdown so the keep-gate can refuse a
    // candidate that lifts the mean while regressing one member (see `score_compare_multi`).
    if adapter.name() == "multi" {
        report["members"] = member_scores(&cases, adapter.as_ref());
    }
    Ok(report)
}

/// Construct a leaf benchmark adapter by name from its params — shared by `run_eval` and the `multi`
/// adapter's member construction.
fn build_adapter(name: &str, params: &Value) -> Result<Box<dyn BenchmarkAdapter>> {
    Ok(match name {
        "mock" => Box::new(LocalAdapter::mock()),
        "synthetic" => Box::new(LocalAdapter::synthetic()),
        "terminal-bench" => Box::new(crate::adapters::TerminalBenchAdapter::from_params(params)?),
        other => {
            return Err(Error::Other(format!(
                "eval_run: adapter {other:?} is not available yet (swebench-lite lands later)"
            )))
        }
    })
}

/// Per-member sub-scores for a `multi` report: partition cases by their `"<member>:"` id prefix and
/// score each subset, so the keep-gate can require that no member regressed.
fn member_scores(cases: &[CaseOutcome], adapter: &dyn BenchmarkAdapter) -> Value {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, Vec<CaseOutcome>> = BTreeMap::new();
    for c in cases {
        let member = c
            .task_id
            .split_once(':')
            .map(|(m, _)| m.to_string())
            .unwrap_or_else(|| "?".to_string());
        groups.entry(member).or_default().push(c.clone());
    }
    let mut out = serde_json::Map::new();
    for (member, gcases) in groups {
        let s = SuiteScore::from_cases(&gcases, |id| adapter.weight_of(id));
        out.insert(
            member,
            json!({
                "pass_rate": s.pass_rate,
                "mean_check_pass_rate": s.mean_check_pass_rate,
                "scalar": s.scalar(),
                "total": s.total,
                "passed": s.passed,
            }),
        );
    }
    Value::Object(out)
}

/// One-line human summary of an eval report — the op's `view` and the CLI header.
pub fn report_view(report: &Value) -> String {
    let f = |k: &str| report.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let u = |k: &str| report.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let adapter = report
        .get("adapter")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    format!(
        "eval[{}] {}/{} tasks pass-all · checks {:.0}% · score {} · {} trial(s) · mean_iters {:.1} · mean_errors {:.1}",
        adapter,
        u("passed"),
        u("total"),
        f("mean_check_pass_rate") * 100.0,
        u("scalar"),
        u("trials"),
        f("mean_iterations"),
        f("mean_tool_errors"),
    )
}

// ---------------------------------------------------------------------------
// eval_sessions
// ---------------------------------------------------------------------------

/// `eval_sessions(report)` — project the per-case session references `[{id, db, task_id}]` out of an
/// `eval_run` report, so review/mining can consume them.
pub struct EvalSessionsTool;

/// Arguments for the `eval_report`-shaped ops (`eval_sessions`, `eval_report_md`, `eval_adopt`,
/// `eval_scalar`): a single `report` JSON string.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReportInput {
    /// an eval_run report (JSON)
    #[allow(dead_code)]
    report: String,
}

#[async_trait]
impl Tool for EvalSessionsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "eval_sessions",
            "Extract the session references [{id, db, task_id}] from an eval_run report.",
            tool_input_schema::<ReportInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let report = arg(&params, "report");
        let empty = Vec::new();
        let cases = report
            .get("cases")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        // Each case carries every trial's session ref; flatten them. The mining source is the
        // RunEvent trace (flow.db), not the message log.
        let sessions: Vec<Value> = cases
            .iter()
            .filter_map(|c| c.get("sessions").and_then(|v| v.as_array()))
            .flatten()
            .filter_map(|s| {
                let id = s.get("id").and_then(|v| v.as_str())?;
                let db = s.get("flow_db").and_then(|v| v.as_str())?;
                let task_id = s.get("task_id").and_then(|v| v.as_str()).unwrap_or(id);
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

/// Arguments for the session-list ops (`painpoints_collect`, `sessions_digest`): a single `sessions`
/// JSON-array string.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SessionsInput {
    /// session references (JSON array)
    #[allow(dead_code)]
    sessions: String,
}

#[async_trait]
impl Tool for PainpointsCollectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "painpoints_collect",
            "Mine pain-points (tool errors, retry loops, missing tools, churn, …) from a list of \
             session references [{id, db}] and return them as JSON.",
            tool_input_schema::<SessionsInput>(),
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
            let Ok(store) = EventStore::open(db) else {
                continue;
            };
            let events = store.run_trace(id).unwrap_or_default();
            all.extend(painpoint::mine(task_id, &events));
        }
        let view = format!("{} pain-point(s) mined", all.len());
        let content = serde_json::to_string(&all).map_err(|e| Error::Other(e.to_string()))?;
        Ok(ToolResult::ok_view(content, view))
    }
}

// ---------------------------------------------------------------------------
// eval_report_md
// ---------------------------------------------------------------------------

/// `eval_report_md(report)` — render an `eval_run` report as a categorized human-readable Markdown
/// digest (headline score, per-task table, mined pain-points). Pain-points are mined internally from
/// the report's session refs, so callers pass only the report.
pub struct EvalReportMdTool;

#[async_trait]
impl Tool for EvalReportMdTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "eval_report_md",
            "Render an eval_run report as a categorized human-readable Markdown report (headline \
             score, per-task table, mined pain-points). Input: {report} (the eval_run JSON).",
            tool_input_schema::<ReportInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let report = arg(&params, "report");
        let md = crate::report::render_markdown(&report);
        let view = format!("rendered {} chars of markdown", md.len());
        Ok(ToolResult::ok_view(md, view))
    }
}

// ---------------------------------------------------------------------------
// sessions_digest
// ---------------------------------------------------------------------------

/// `sessions_digest(sessions)` — render each session's RunEvent trace into a compact transcript, so the
/// reviewer reasons over *what the agent did*, not just pass/fail. Returns plain text (for `{{digest}}`).
pub struct SessionsDigestTool;

#[async_trait]
impl Tool for SessionsDigestTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "sessions_digest",
            "Render each session's RunEvent trace into a compact transcript for review.",
            tool_input_schema::<SessionsInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        const MAX_CHARS: usize = 8000;
        let sessions = arg(&params, "sessions");
        let empty = Vec::new();
        let arr = sessions.as_array().unwrap_or(&empty);
        let mut out = String::new();
        let mut n = 0;
        for s in arr {
            let (Some(id), Some(db)) = (
                s.get("id").and_then(|v| v.as_str()),
                s.get("db").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let task_id = s.get("task_id").and_then(|v| v.as_str()).unwrap_or(id);
            let Ok(store) = EventStore::open(db) else {
                continue;
            };
            let events = store.run_trace(id).unwrap_or_default();
            out.push_str(&format!(
                "## {task_id} (session {id})\n{}\n",
                crate::transcript::render_run_trace(&events, 40)
            ));
            n += 1;
            if out.len() >= MAX_CHARS {
                out.push_str("\n… (digest truncated)\n");
                break;
            }
        }
        Ok(ToolResult::ok_view(
            out,
            format!("digest of {n} session(s)"),
        ))
    }
}

// ---------------------------------------------------------------------------
// improve_log
// ---------------------------------------------------------------------------

/// `improve_log(record)` — append a timestamped round record to `.flux/eval/improve-log.jsonl` so a
/// human can audit what the loop tried, whether the grader was tampered with, and the gate outcome.
pub struct ImproveLogTool;

/// Arguments for the `improve_log` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImproveLogInput {
    /// round record (any JSON object)
    #[allow(dead_code)]
    record: serde_json::Map<String, Value>,
}

#[async_trait]
impl Tool for ImproveLogTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "improve_log".into(),
            description:
                "Append a timestamped round record to .flux/eval/improve-log.jsonl (audit trail)."
                    .into(),
            input_schema: tool_input_schema::<ImproveLogInput>(),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Low,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![".flux/eval/improve-log.jsonl".to_string()]
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let record = arg(&params, "record");
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let entry = json!({ "ts_ms": ts_ms, "round": record });
        let line = format!(
            "{}\n",
            serde_json::to_string(&entry).map_err(|e| Error::Other(e.to_string()))?
        );
        ctx.system
            .append_file(".flux/eval/improve-log.jsonl", &line)
            .await?;
        Ok(ToolResult::ok_view(
            line.trim().to_string(),
            "logged round to .flux/eval/improve-log.jsonl",
        ))
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
            tool_input_schema::<ReportInput>(),
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
            tool_input_schema::<ReportInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let report = arg(&params, "report");
        let scalar = report.get("scalar").and_then(|v| v.as_u64()).unwrap_or(0);
        Ok(ToolResult::ok(scalar.to_string()))
    }
}

// ---------------------------------------------------------------------------
// grade
// ---------------------------------------------------------------------------

/// `grade(criterion) -> "true"|"false"` — evaluate a verifiable pass/fail [`Criterion`] against the
/// CURRENT workspace, reusing the eval harness's own [`runner::grade`](crate::runner::grade) so a flow's
/// graded stop-condition uses the exact same check the benchmark does (no divergence). The criterion is
/// a `{kind: "command"|"file_content"|"all", …}` object; the string boolean is read by a `when`
/// condition's truthiness — `when grade(@json{…}) -> $done = true` is the evidence-based early stop.
pub struct GradeTool;

/// Arguments for the `grade` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GradeInput {
    /// a Criterion: {kind:"command",run,expect_exit?} | {kind:"file_content",path,equals?/contains?/regex?} | {kind:"all",of:[…]}
    #[allow(dead_code)]
    criterion: serde_json::Map<String, Value>,
}

#[async_trait]
impl Tool for GradeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "grade",
            "Evaluate a pass/fail criterion against the current workspace; returns \"true\" or \
             \"false\". `criterion` is {kind: \"command\"|\"file_content\"|\"all\", …} — the same check \
             the eval harness uses. A `command` criterion runs a check command (e.g. `cargo test`).",
            tool_input_schema::<GradeInput>(),
        )
        // A `command` criterion shells out to a checker, so be honest about the process effect.
        .with_effects(vec![Effect::Read, Effect::Process])
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let criterion: crate::spec::Criterion =
            serde_json::from_value(arg(&params, "criterion"))
                .map_err(|e| Error::Other(format!("grade: invalid criterion: {e}")))?;
        let pass = crate::runner::grade(&criterion, ctx.system.as_ref()).await?;
        Ok(ToolResult::ok(
            if pass { "true" } else { "false" }.to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// score_compare
// ---------------------------------------------------------------------------

/// `score_compare(baseline, candidate)` — `"true"` iff `candidate` is strictly better than `baseline`
/// (lexicographic: pass-rate, then fewer tool-errors, then fewer iterations). The string boolean is
/// read by a `when` condition's truthiness.
pub struct ScoreCompareTool;

/// Arguments for the report-comparison ops (`score_compare`, `score_compare_multi`): a `baseline`
/// and `candidate` eval_run report, each a JSON string.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BaselineCandidateInput {
    /// baseline eval_run report (JSON)
    #[allow(dead_code)]
    baseline: String,
    /// candidate eval_run report (JSON)
    #[allow(dead_code)]
    candidate: String,
}

#[async_trait]
impl Tool for ScoreCompareTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "score_compare",
            "Return \"true\" iff the candidate eval report is strictly better than the baseline.",
            tool_input_schema::<BaselineCandidateInput>(),
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
// score_compare_multi
// ---------------------------------------------------------------------------

/// Pure keep-decision for a `multi` report: returns `(keep, regressed_member)`. `keep` is true iff the
/// combined candidate is strictly better AND no baseline member regressed (pass_rate & check-rate must
/// not drop, and no member may disappear). Extracted from the op so it is unit-testable.
fn multi_keep(baseline: &Value, candidate: &Value) -> (bool, Option<String>) {
    const EPS: f64 = 1e-9;
    let f = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let empty = serde_json::Map::new();
    let base_members = baseline
        .get("members")
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    let cand_members = candidate
        .get("members")
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    for (name, bscore) in base_members {
        match cand_members.get(name) {
            None => return (false, Some(format!("{name} (missing)"))),
            Some(cscore) => {
                if f(cscore, "pass_rate") + EPS < f(bscore, "pass_rate")
                    || f(cscore, "mean_check_pass_rate") + EPS < f(bscore, "mean_check_pass_rate")
                {
                    return (false, Some(name.clone()));
                }
            }
        }
    }
    (report_is_better(candidate, baseline), None)
}

/// `score_compare_multi(baseline, candidate)` — like `score_compare`, but for `multi` reports: return
/// "true" iff the combined candidate is strictly better AND **no member regressed**. This stops a
/// candidate that lifts the combined mean while silently regressing one benchmark from being kept.
pub struct ScoreCompareMultiTool;

#[async_trait]
impl Tool for ScoreCompareMultiTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "score_compare_multi",
            "Return \"true\" iff the candidate multi-eval report is strictly better overall AND no \
             member benchmark regressed (pass_rate & check-rate ≥ baseline). Use as the keep-gate for \
             combined evals so a gain on one benchmark can't mask a regression on another.",
            tool_input_schema::<BaselineCandidateInput>(),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let baseline = arg(&params, "baseline");
        let candidate = arg(&params, "candidate");
        let (keep, regressed) = multi_keep(&baseline, &candidate);
        let view = match (&regressed, keep) {
            (Some(m), _) => format!("REJECT: member `{m}` regressed"),
            (None, true) => "KEEP: combined better, no member regressed".to_string(),
            (None, false) => "REJECT: combined not strictly better".to_string(),
        };
        Ok(ToolResult::ok_view(
            if keep { "true" } else { "false" },
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

/// Arguments for the `change_implement` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ChangeImplementInput {
    /// tasks to implement (JSON array)
    #[allow(dead_code)]
    tasks: String,
    /// cap on tasks implemented this round (0 = all)
    #[serde(default)]
    #[allow(dead_code)]
    limit: Option<u64>,
}

#[async_trait]
impl Tool for ChangeImplementTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "change_implement".into(),
            description: "Implement each derived task by spawning a `worker` sub-agent; returns a \
                          per-task summary. Input is a JSON array of tasks (objects or strings)."
                .into(),
            input_schema: tool_input_schema::<ChangeImplementInput>(),
            output_schema: None,
            effects: Vec::new(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut tasks = crate::aggregate::extract_array(&arg(&params, "tasks"));
        // Blast-radius cap: implement at most `limit` tasks this round (0 = all).
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if limit > 0 && tasks.len() > limit {
            tasks.truncate(limit);
        }
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
        assert_eq!(ScoreCompareMultiTool.spec().name, "score_compare_multi");
        assert_eq!(EvalReportMdTool.spec().name, "eval_report_md");
        assert_eq!(GradeTool.spec().name, "grade");
    }

    /// The combined-eval keep-gate must reject a candidate that lifts the overall mean while
    /// regressing one member benchmark — otherwise a gain on one eval masks a loss on another.
    #[test]
    fn multi_keep_rejects_a_masked_member_regression() {
        let base = serde_json::json!({
            "pass_rate": 0.5,
            "members": {
                "syn": {"pass_rate": 0.4, "mean_check_pass_rate": 0.4},
                "tb":  {"pass_rate": 0.6, "mean_check_pass_rate": 0.6}
            }
        });
        // Combined mean rises, but `tb` regressed (0.6 → 0.3) — must be rejected.
        let masked = serde_json::json!({
            "pass_rate": 0.6,
            "members": {
                "syn": {"pass_rate": 0.9, "mean_check_pass_rate": 0.9},
                "tb":  {"pass_rate": 0.3, "mean_check_pass_rate": 0.3}
            }
        });
        let (keep, who) = multi_keep(&base, &masked);
        assert!(!keep);
        assert_eq!(who.as_deref(), Some("tb"));

        // A candidate that improves overall and regresses nothing is kept.
        let good = serde_json::json!({
            "pass_rate": 0.7,
            "members": {
                "syn": {"pass_rate": 0.5, "mean_check_pass_rate": 0.5},
                "tb":  {"pass_rate": 0.9, "mean_check_pass_rate": 0.9}
            }
        });
        let (keep2, who2) = multi_keep(&base, &good);
        assert!(keep2);
        assert!(who2.is_none());
    }

    /// The `grade` op wraps `runner::grade`, returning the `"true"`/`"false"` string a `when` reads.
    #[tokio::test]
    async fn grade_op_checks_a_file_content_criterion() {
        use flux_system::{System, Workspace};
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!("flux-grade-op-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("out.txt"), "hello world").unwrap();
        let ctx = ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())));

        let pass = GradeTool
            .execute(
                &ctx,
                json!({ "criterion": { "kind": "file_content", "path": "out.txt", "contains": "hello" } }),
            )
            .await
            .unwrap();
        assert_eq!(pass.content, "true", "matching criterion grades true");

        let fail = GradeTool
            .execute(
                &ctx,
                json!({ "criterion": { "kind": "file_content", "path": "out.txt", "contains": "nope" } }),
            )
            .await
            .unwrap();
        assert_eq!(fail.content, "false", "non-matching criterion grades false");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eval_sessions_flattens_trial_refs_from_cases() {
        // A multi-trial report: each case carries a `sessions` array (one ref per trial).
        let report = json!({
            "cases": [
                { "task_id": "t/a", "pass_rate": 0.5, "sessions": [
                    {"id": "s_1", "flow_db": "/tmp/a1/.flux/flow.db", "task_id": "t/a"},
                    {"id": "s_2", "flow_db": "/tmp/a2/.flux/flow.db", "task_id": "t/a"}
                ]},
                { "task_id": "t/b", "pass_rate": 0.0, "sessions": [] }
            ]
        });
        let report_v = arg(&json!({ "report": report.to_string() }), "report");
        let empty = Vec::new();
        let cases = report_v
            .get("cases")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        let refs: Vec<_> = cases
            .iter()
            .filter_map(|c| c.get("sessions").and_then(|v| v.as_array()))
            .flatten()
            .filter_map(|s| {
                let id = s.get("id").and_then(|v| v.as_str())?;
                let db = s.get("flow_db").and_then(|v| v.as_str())?;
                Some((id.to_string(), db.to_string()))
            })
            .collect();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, "s_1");
    }
}

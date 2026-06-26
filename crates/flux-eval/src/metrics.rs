//! Per-task run results and metric extraction.
//!
//! The pure-DAG engine persists only `user → assistant(text)` to the message log — raw op calls never
//! re-enter history — so tool-call/error counts come from flux-flow's **RunEvent trace** (flow.db),
//! not the message log. Iterations (turns) come from the message log (assistant-message count).

use std::path::PathBuf;

use flux_core::{Message, Role, Usage};
use flux_flow::ast::RunEvent;

/// The outcome of running one benchmark task.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunResult {
    pub task_id: String,
    pub passed: bool,
    /// Turns (assistant messages in the log) — a proxy for plan rounds.
    pub iterations: u32,
    /// Op invocations (`StepStarted` events).
    pub tool_calls: u32,
    /// Failed ops (`StepFailed` events).
    pub tool_errors: u32,
    /// Token usage, once the engine surfaces it (None until the usage-capture slice).
    pub tokens: Option<Usage>,
    pub wall_ms: u64,
    pub session_id: Option<String>,
    /// The child's isolated session store (`~/.flux/sessions.db`).
    pub session_db: Option<PathBuf>,
    /// The child's isolated RunEvent store (`~/.flux/flow.db`) — the source for pain-point mining.
    pub flow_db: Option<PathBuf>,
    pub timed_out: bool,
    /// A short note when the run errored before/around grading (spawn failure, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl RunResult {
    /// A failed result that never produced a session (spawn error, timeout before any turn, …).
    pub fn failed(task_id: impl Into<String>, wall_ms: u64, note: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            passed: false,
            iterations: 0,
            tool_calls: 0,
            tool_errors: 0,
            tokens: None,
            wall_ms,
            session_id: None,
            session_db: None,
            flow_db: None,
            timed_out: false,
            note: Some(note.into()),
        }
    }
}

/// A session reference (one trial's stores), for later mining.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionRef {
    pub id: String,
    /// The RunEvent store (flow.db) — the mining source.
    pub flow_db: String,
    pub task_id: String,
}

/// A task's outcome aggregated over `trials` runs — pass-rate over noise, mean metrics, and every
/// trial's session ref (so mining sees all trials). This is what scoring + the report use.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaseOutcome {
    pub task_id: String,
    pub trials: u32,
    pub passes: u32,
    /// `passes / trials` — the per-task signal that absorbs single-run model noise.
    pub pass_rate: f64,
    pub mean_tool_errors: f64,
    pub mean_iterations: f64,
    pub mean_tokens: f64,
    pub mean_wall_ms: f64,
    pub timed_out_any: bool,
    pub sessions: Vec<SessionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CaseOutcome {
    /// Aggregate one task's trial results.
    pub fn from_trials(task_id: &str, runs: &[RunResult]) -> Self {
        let trials = runs.len() as u32;
        let n = (trials.max(1)) as f64;
        let passes = runs.iter().filter(|r| r.passed).count() as u32;
        let sum_errors: u64 = runs.iter().map(|r| r.tool_errors as u64).sum();
        let sum_iters: u64 = runs.iter().map(|r| r.iterations as u64).sum();
        let sum_wall: u64 = runs.iter().map(|r| r.wall_ms).sum();
        let sum_tokens: f64 = runs
            .iter()
            .filter_map(|r| r.tokens.as_ref().map(|u| u.total() as f64))
            .sum();
        let sessions = runs
            .iter()
            .filter_map(|r| match (&r.session_id, &r.flow_db) {
                (Some(id), Some(db)) => Some(SessionRef {
                    id: id.clone(),
                    flow_db: db.to_string_lossy().to_string(),
                    task_id: task_id.to_string(),
                }),
                _ => None,
            })
            .collect();
        CaseOutcome {
            task_id: task_id.to_string(),
            trials,
            passes,
            pass_rate: if trials > 0 {
                passes as f64 / trials as f64
            } else {
                0.0
            },
            mean_tool_errors: sum_errors as f64 / n,
            mean_iterations: sum_iters as f64 / n,
            mean_tokens: sum_tokens / n,
            mean_wall_ms: sum_wall as f64 / n,
            timed_out_any: runs.iter().any(|r| r.timed_out),
            sessions,
            note: runs.iter().find_map(|r| r.note.clone()),
        }
    }
}

/// Turn count: assistant messages in the replayed conversation.
pub fn iterations_from_messages(messages: &[Message]) -> u32 {
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .count() as u32
}

/// `(tool_calls, tool_errors)` from a session's RunEvent trace.
pub fn metrics_from_events(events: &[RunEvent]) -> (u32, u32) {
    let mut calls = 0;
    let mut errors = 0;
    for ev in events {
        match ev {
            RunEvent::StepStarted { .. } => calls += 1,
            RunEvent::StepFailed { .. } => errors += 1,
            _ => {}
        }
    }
    (calls, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_flow::ast::StepId;

    #[test]
    fn iterations_counts_assistant_messages() {
        let messages = vec![
            Message::user_text("go"),
            Message::assistant_text("step 1"),
            Message::user_text("more"),
            Message::assistant_text("step 2"),
        ];
        assert_eq!(iterations_from_messages(&messages), 2);
    }

    #[test]
    fn metrics_from_events_counts_steps_and_failures() {
        let events = vec![
            RunEvent::StepStarted {
                step: StepId("a".into()),
                op: "grep".into(),
                input_hash: "h1".into(),
            },
            RunEvent::StepFailed {
                step: StepId("a".into()),
                error: "boom".into(),
            },
            RunEvent::StepStarted {
                step: StepId("b".into()),
                op: "read".into(),
                input_hash: "h2".into(),
            },
        ];
        assert_eq!(metrics_from_events(&events), (2, 1));
    }

    #[test]
    fn case_outcome_aggregates_trials() {
        let mk = |passed, errors, iters, id: &str, db: &str| RunResult {
            task_id: "t".into(),
            passed,
            iterations: iters,
            tool_calls: iters,
            tool_errors: errors,
            tokens: None,
            wall_ms: 10,
            session_id: Some(id.into()),
            session_db: None,
            flow_db: Some(db.into()),
            timed_out: false,
            note: None,
        };
        let runs = vec![
            mk(true, 0, 2, "s_1", "/a/flow.db"),
            mk(false, 1, 4, "s_2", "/b/flow.db"),
        ];
        let c = CaseOutcome::from_trials("t", &runs);
        assert_eq!(c.trials, 2);
        assert_eq!(c.passes, 1);
        assert!((c.pass_rate - 0.5).abs() < 1e-9);
        assert!((c.mean_tool_errors - 0.5).abs() < 1e-9);
        assert_eq!(c.sessions.len(), 2);
        assert_eq!(c.sessions[0].id, "s_1");
        assert_eq!(c.sessions[0].flow_db, "/a/flow.db");
    }
}

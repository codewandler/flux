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
}

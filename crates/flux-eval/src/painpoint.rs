//! Deterministic pain-point mining over a session's **RunEvent trace** (flux-flow's flow.db).
//!
//! The pure-DAG engine records every op as `StepStarted{op,input_hash}` and a failure as
//! `StepFailed{error}` — that trace, not the message log, is where tool calls/errors live. We mine:
//! tool errors, missing-tool reaches (a call to an op the registry lacks), and retry loops (the same
//! op re-issued with identical input — identical input ⇒ identical `StepId`, so this is precise).

use std::collections::HashMap;

use serde::Serialize;

use flux_flow::ast::{RunEvent, StepId};

/// The category of a mined pain-point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PainKind {
    /// An op returned an error.
    ToolError,
    /// The agent reached for an op the registry doesn't have (a missing-capability signal).
    ToolNotFound,
    /// The same op was re-issued with identical input.
    RetryLoop,
}

/// One mined pain-point.
#[derive(Debug, Clone, Serialize)]
pub struct PainPoint {
    pub task_id: String,
    pub kind: PainKind,
    /// 1 (friction) … 5 (blocking).
    pub severity: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub evidence: String,
    pub occurrences: u32,
    /// How this was found — always `"mined"` for deterministic detection.
    pub source: String,
}

fn snippet(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn is_missing_tool(err: &str) -> bool {
    let e = err.to_lowercase();
    [
        "unknown op",
        "unknown tool",
        "unknown operation",
        "not callable",
        "no such tool",
        "not found in registry",
    ]
    .iter()
    .any(|s| e.contains(s))
}

fn push_error_points(
    out: &mut Vec<PainPoint>,
    task_id: &str,
    by_tool: HashMap<String, (String, u32)>,
    kind: PainKind,
    severity: u8,
) {
    let mut tools: Vec<(String, (String, u32))> = by_tool.into_iter().collect();
    tools.sort_by(|a, b| a.0.cmp(&b.0));
    for (tool, (evidence, count)) in tools {
        out.push(PainPoint {
            task_id: task_id.to_string(),
            kind,
            severity,
            tool: Some(tool),
            evidence,
            occurrences: count,
            source: "mined".to_string(),
        });
    }
}

/// Mine pain-points from one task's RunEvent trace.
pub fn mine(task_id: &str, events: &[RunEvent]) -> Vec<PainPoint> {
    let mut step_op: HashMap<StepId, (String, String)> = HashMap::new();
    let mut calls: Vec<(String, String)> = Vec::new(); // (op, input_hash) in order
    let mut tool_errors: HashMap<String, (String, u32)> = HashMap::new();
    let mut missing: HashMap<String, (String, u32)> = HashMap::new();

    for ev in events {
        match ev {
            RunEvent::StepStarted {
                step,
                op,
                input_hash,
            } => {
                step_op.insert(step.clone(), (op.clone(), input_hash.clone()));
                calls.push((op.clone(), input_hash.clone()));
            }
            RunEvent::StepFailed { step, error } => {
                let op = step_op
                    .get(step)
                    .map(|(o, _)| o.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                let bucket = if is_missing_tool(error) {
                    &mut missing
                } else {
                    &mut tool_errors
                };
                let entry = bucket.entry(op).or_insert_with(|| (snippet(error, 160), 0));
                entry.1 += 1;
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    push_error_points(&mut out, task_id, missing, PainKind::ToolNotFound, 4);
    push_error_points(&mut out, task_id, tool_errors, PainKind::ToolError, 3);

    // Retry loops: runs of length ≥ 2 of an identical (op, input_hash).
    let mut i = 0;
    while i < calls.len() {
        let mut j = i + 1;
        while j < calls.len() && calls[j] == calls[i] {
            j += 1;
        }
        let run = j - i;
        if run >= 2 {
            out.push(PainPoint {
                task_id: task_id.to_string(),
                kind: PainKind::RetryLoop,
                severity: 2,
                tool: Some(calls[i].0.clone()),
                evidence: format!("`{}` re-issued {run}× with identical input", calls[i].0),
                occurrences: run as u32,
                source: "mined".to_string(),
            });
        }
        i = j.max(i + 1);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(step: &str, op: &str, hash: &str) -> RunEvent {
        RunEvent::StepStarted {
            step: StepId(step.into()),
            op: op.into(),
            input_hash: hash.into(),
        }
    }
    fn failed(step: &str, error: &str) -> RunEvent {
        RunEvent::StepFailed {
            step: StepId(step.into()),
            error: error.into(),
        }
    }

    #[test]
    fn classifies_tool_error_vs_missing_tool() {
        let events = vec![
            started("s1", "grep", "h1"),
            failed("s1", "regex error: unbalanced parenthesis"),
            started("s2", "frobnicate", "h2"),
            failed("s2", "unknown op `frobnicate`"),
        ];
        let pp = mine("t/a", &events);
        let err = pp.iter().find(|p| p.kind == PainKind::ToolError).unwrap();
        assert_eq!(err.tool.as_deref(), Some("grep"));
        let missing = pp
            .iter()
            .find(|p| p.kind == PainKind::ToolNotFound)
            .unwrap();
        assert_eq!(missing.tool.as_deref(), Some("frobnicate"));
        assert_eq!(missing.severity, 4);
    }

    #[test]
    fn detects_retry_loop_of_identical_calls() {
        // identical input ⇒ identical StepId (input_hash-derived) — three re-issues.
        let events = vec![
            started("s", "grep", "hX"),
            started("s", "grep", "hX"),
            started("s", "grep", "hX"),
        ];
        let pp = mine("t/b", &events);
        let retry = pp.iter().find(|p| p.kind == PainKind::RetryLoop).unwrap();
        assert_eq!(retry.tool.as_deref(), Some("grep"));
        assert_eq!(retry.occurrences, 3);
    }

    #[test]
    fn clean_trace_yields_nothing() {
        let events = vec![started("s1", "read", "h1"), started("s2", "write", "h2")];
        assert!(mine("t/c", &events).is_empty());
    }
}

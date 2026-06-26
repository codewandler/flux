//! Aggregate pain-points (deterministic mining + LLM review) into ranked improvement candidates, and
//! the small loop-control ops the flow uses to iterate over them.
//!
//! Clustering is deterministic: mined pain-points group by `(kind, tool)`, review findings group by a
//! normalized area; each cluster becomes a candidate weighted by breadth × severity × √occurrences, so
//! the most pervasive, severe issues sort first. The LLM review text is parsed tolerantly (it may be a
//! bare JSON array or prose with an array embedded).

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::Result;
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::ToolSpec;

use crate::util::{arg, json_result};

/// Tolerantly extract a JSON array from a value that may be an array already, or a string that is
/// (or contains) a JSON array — mirrors `flux_orchestrate::parse_subtasks`'s leniency for LLM output.
pub fn extract_array(v: &Value) -> Vec<Value> {
    if let Some(a) = v.as_array() {
        return a.clone();
    }
    if let Some(s) = v.as_str() {
        if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(s) {
            return a;
        }
        // Fall back to the first '[' … last ']' span.
        if let (Some(i), Some(j)) = (s.find('['), s.rfind(']')) {
            if j > i {
                if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&s[i..=j]) {
                    return a;
                }
            }
        }
    }
    Vec::new()
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Default)]
struct Cluster {
    kind: String,
    tool: Option<String>,
    tasks: BTreeSet<String>,
    occurrences: u32,
    max_severity: u8,
    examples: Vec<String>,
}

fn title_for(c: &Cluster) -> String {
    let tool = c.tool.as_deref().unwrap_or("the agent");
    match c.kind.as_str() {
        "tool_error" => format!("Reduce errors from `{tool}`"),
        "retry_loop" => format!("Avoid retry loops on `{tool}`"),
        "tool_not_found" => {
            format!("Provide a tool for `{tool}` (agent reached for a missing tool)")
        }
        "max_iterations" => "Help tasks finish within the iteration budget".to_string(),
        "read_edit_churn" => format!("Reduce re-reads of files before editing (`{tool}`)"),
        "timeout" => "Reduce tasks that time out".to_string(),
        "review" => c
            .examples
            .first()
            .cloned()
            .unwrap_or_else(|| format!("Address: {tool}")),
        other => format!("Address `{other}` ({tool})"),
    }
}

/// Cluster pain-points (mined) + review findings into ranked candidate JSON objects.
pub fn aggregate(mined: &[Value], reviewed: &[Value]) -> Vec<Value> {
    let mut clusters: BTreeMap<String, Cluster> = BTreeMap::new();

    for p in mined {
        let kind = p
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let tool = p.get("tool").and_then(|v| v.as_str()).map(String::from);
        let task = p
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let sev = p.get("severity").and_then(|v| v.as_u64()).unwrap_or(1) as u8;
        let occ = p.get("occurrences").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        let ev = p
            .get("evidence")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let key = format!("{kind}:{}", tool.clone().unwrap_or_default());
        let c = clusters.entry(key).or_default();
        if c.kind.is_empty() {
            c.kind = kind;
            c.tool = tool;
        }
        if !task.is_empty() {
            c.tasks.insert(task);
        }
        c.occurrences += occ;
        c.max_severity = c.max_severity.max(sev);
        if c.examples.len() < 3 && !ev.is_empty() {
            c.examples.push(ev);
        }
    }

    for r in reviewed {
        let area = r
            .get("area")
            .or_else(|| r.get("missing_capability"))
            .or_else(|| r.get("implicated_tool"))
            .and_then(|v| v.as_str())
            .unwrap_or("review")
            .to_string();
        let symptom = r
            .get("symptom")
            .or_else(|| r.get("summary"))
            .or_else(|| r.get("suggested_fix"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let sev = r.get("severity").and_then(|v| v.as_u64()).unwrap_or(2) as u8;
        let key = format!("review:{}", normalize(&area));
        let c = clusters.entry(key).or_default();
        if c.kind.is_empty() {
            c.kind = "review".to_string();
            c.tool = Some(area);
        }
        c.occurrences += 1;
        c.max_severity = c.max_severity.max(sev);
        if c.examples.len() < 3 && !symptom.is_empty() {
            c.examples.push(symptom);
        }
    }

    let mut out: Vec<Value> = clusters
        .into_iter()
        .map(|(key, c)| {
            let breadth = c.tasks.len().max(1) as f64;
            let weight = breadth * (c.max_severity as f64) * (c.occurrences as f64).sqrt().max(1.0);
            json!({
                "id": key,
                "kind": c.kind,
                "tool": c.tool,
                "title": title_for(&c),
                "rationale": format!(
                    "{} occurrence(s) across {} task(s), max severity {}",
                    c.occurrences, breadth as u32, c.max_severity
                ),
                "weight": weight,
                "occurrences": c.occurrences,
                "max_severity": c.max_severity,
                "examples": c.examples,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let wa = a.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let wb = b.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.0);
        wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

// ---------------------------------------------------------------------------
// ops
// ---------------------------------------------------------------------------

/// `improvements_aggregate(mined, reviewed)` — cluster pain-points into ranked candidates.
pub struct ImprovementsAggregateTool;

#[async_trait]
impl Tool for ImprovementsAggregateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "improvements_aggregate",
            "Cluster mined pain-points and LLM review findings into ranked improvement candidates.",
            json!({
                "type": "object",
                "properties": {
                    "mined": {"type": "string", "description": "deterministic pain-points (JSON array)"},
                    "reviewed": {"type": "string", "description": "LLM review findings (JSON array or prose)"}
                },
                "required": ["mined", "reviewed"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mined = extract_array(&arg(&params, "mined"));
        let reviewed = extract_array(&arg(&params, "reviewed"));
        let candidates = aggregate(&mined, &reviewed);
        let view = format!("{} improvement candidate(s)", candidates.len());
        json_result(&Value::Array(candidates), view)
    }
}

/// `candidates_empty(candidates)` — `"true"` when there are no candidates (the loop's `until` guard).
pub struct CandidatesEmptyTool;

#[async_trait]
impl Tool for CandidatesEmptyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "candidates_empty",
            "Return \"true\" iff the candidate list is empty.",
            json!({
                "type": "object",
                "properties": { "candidates": {"type": "string", "description": "candidates (JSON array)"} },
                "required": ["candidates"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let candidates = extract_array(&arg(&params, "candidates"));
        let empty = candidates.is_empty();
        Ok(ToolResult::ok_view(
            if empty { "true" } else { "false" },
            format!("{} candidate(s) remaining", candidates.len()),
        ))
    }
}

/// `candidates_advance(candidates)` — drop the consumed (first) candidate, return the rest.
pub struct CandidatesAdvanceTool;

#[async_trait]
impl Tool for CandidatesAdvanceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "candidates_advance",
            "Drop the first (consumed) candidate and return the remaining candidates.",
            json!({
                "type": "object",
                "properties": { "candidates": {"type": "string", "description": "candidates (JSON array)"} },
                "required": ["candidates"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut candidates = extract_array(&arg(&params, "candidates"));
        if !candidates.is_empty() {
            candidates.remove(0);
        }
        let view = format!("{} candidate(s) remaining", candidates.len());
        json_result(&Value::Array(candidates), view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_array_handles_array_string_and_embedded() {
        assert_eq!(extract_array(&json!([1, 2])).len(), 2);
        assert_eq!(extract_array(&json!("[{\"a\":1}]")).len(), 1);
        assert_eq!(
            extract_array(&json!("here you go: [\"x\",\"y\"] thanks")).len(),
            2
        );
        assert!(extract_array(&json!("no array here")).is_empty());
    }

    #[test]
    fn aggregate_clusters_and_ranks() {
        let mined = vec![
            json!({"task_id":"t/a","kind":"tool_error","tool":"grep","severity":3,"occurrences":2,"evidence":"regex error"}),
            json!({"task_id":"t/b","kind":"tool_error","tool":"grep","severity":4,"occurrences":1,"evidence":"bad pattern"}),
            json!({"task_id":"t/a","kind":"retry_loop","tool":"glob","severity":2,"occurrences":3,"evidence":"glob 3x"}),
        ];
        let reviewed = vec![
            json!({"area":"test runner","symptom":"no way to run a single test","severity":4}),
        ];
        let cands = aggregate(&mined, &reviewed);
        // grep tool_error spans 2 tasks → should outrank the single-task glob retry loop.
        assert!(cands.len() >= 3);
        assert_eq!(cands[0]["kind"], "tool_error");
        assert_eq!(cands[0]["tool"], "grep");
        // a review finding is present
        assert!(cands.iter().any(|c| c["kind"] == "review"));
    }

    #[test]
    fn candidates_advance_pops_first() {
        let cands = json!([{"id":"a"},{"id":"b"},{"id":"c"}]);
        let mut arr = extract_array(&cands);
        arr.remove(0);
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "b");
    }
}

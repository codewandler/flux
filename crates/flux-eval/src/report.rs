//! Render an eval report (+ mined pain-points) into a categorized human-readable Markdown digest —
//! the legible counterpart to the machine JSON the improvement loop consumes. Used by the
//! `eval_report_md` op and `flux eval --report`.

use serde_json::Value;

use flux_events::EventStore;

use crate::painpoint::{self, PainKind, PainPoint};

/// Render a `run_eval` report as a categorized Markdown document: headline score, a per-task table,
/// and mined pain-points grouped by kind. Pain-points are mined here from the report's session
/// references, so the caller passes only the report.
pub fn render_markdown(report: &Value) -> String {
    let f = |k: &str| report.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let u = |k: &str| report.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let adapter = report
        .get("adapter")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let empty = Vec::new();
    let cases = report
        .get("cases")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let mut out = String::new();
    out.push_str(&format!("# flux eval report — `{adapter}`\n\n"));
    out.push_str(&format!(
        "- **Tasks pass-all:** {}/{} · **pass-rate:** {:.0}% · **score:** {}\n",
        u("passed"),
        u("total"),
        f("pass_rate") * 100.0,
        u("scalar"),
    ));
    out.push_str(&format!(
        "- **Mean iterations:** {:.1} · **mean tool-errors:** {:.1} · **trials:** {}\n\n",
        f("mean_iterations"),
        f("mean_tool_errors"),
        u("trials"),
    ));

    out.push_str(
        "## Per-task\n\n| task | pass | iters | tool-errs | notes |\n|---|---|---|---|---|\n",
    );
    for c in cases {
        let id = c.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
        let pr = c.get("pass_rate").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let mark = if pr >= 1.0 {
            "✅"
        } else if pr > 0.0 {
            "⚠️"
        } else {
            "❌"
        };
        let iters = c
            .get("mean_iterations")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let errs = c
            .get("mean_tool_errors")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let note = c.get("note").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!(
            "| `{id}` | {mark} {:.0}% | {iters:.1} | {errs:.1} | {note} |\n",
            pr * 100.0
        ));
    }
    out.push('\n');

    let pains = mine_report(cases);
    out.push_str("## Pain-points\n\n");
    if pains.is_empty() {
        out.push_str("_None mined — the runs were clean._\n");
        return out;
    }
    for kind in [
        PainKind::ToolNotFound,
        PainKind::ToolError,
        PainKind::RetryLoop,
    ] {
        let group: Vec<&PainPoint> = pains.iter().filter(|p| p.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("### {}\n", kind_label(kind)));
        for p in group {
            let tool = p
                .tool
                .as_deref()
                .map(|t| format!("`{t}` "))
                .unwrap_or_default();
            out.push_str(&format!(
                "- {}{} — _{}× in `{}`_ (sev {})\n",
                tool, p.evidence, p.occurrences, p.task_id, p.severity,
            ));
        }
        out.push('\n');
    }
    out
}

fn kind_label(k: PainKind) -> &'static str {
    match k {
        PainKind::ToolError => "Tool errors",
        PainKind::ToolNotFound => "Missing tools",
        PainKind::RetryLoop => "Retry loops",
    }
}

/// Mine pain-points from a report's per-case session references (`{id, flow_db, task_id}`), opening
/// each session's event store and folding its run trace — the same path as `painpoints_collect`.
fn mine_report(cases: &[Value]) -> Vec<PainPoint> {
    let mut all = Vec::new();
    for c in cases {
        let task_id = c.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
        let Some(sessions) = c.get("sessions").and_then(|v| v.as_array()) else {
            continue;
        };
        for s in sessions {
            let (Some(id), Some(db)) = (
                s.get("id").and_then(|v| v.as_str()),
                s.get("flow_db").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let Ok(store) = EventStore::open(db) else {
                continue;
            };
            let events = store.run_trace(id).unwrap_or_default();
            all.extend(painpoint::mine(task_id, &events));
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_headline_and_per_task_without_sessions() {
        let report = json!({
            "adapter": "synthetic",
            "pass_rate": 1.0,
            "scalar": 1000,
            "total": 2,
            "passed": 2,
            "trials": 1,
            "mean_iterations": 1.5,
            "mean_tool_errors": 0.0,
            "cases": [
                {"task_id": "synthetic/two-sum", "pass_rate": 1.0, "mean_iterations": 1.0, "mean_tool_errors": 0.0, "sessions": []},
                {"task_id": "synthetic/gcd", "pass_rate": 0.0, "mean_iterations": 2.0, "mean_tool_errors": 3.0, "sessions": []}
            ]
        });
        let md = render_markdown(&report);
        assert!(md.contains("# flux eval report — `synthetic`"));
        assert!(md.contains("2/2"));
        assert!(md.contains("`synthetic/two-sum`"));
        assert!(md.contains("✅"));
        assert!(md.contains("❌"));
        assert!(md.contains("_None mined"));
    }
}

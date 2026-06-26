//! Render a session's RunEvent trace into a compact transcript for the LLM reviewer.
//!
//! The reviewer reasons better over *what the agent actually did* than over pass/fail alone. The
//! pure-DAG engine records each op as a `StepStarted`/`StepSucceeded`/`StepFailed` event in flow.db;
//! this turns that trace into a short `→ op` / `✓` / `✗ <error>` listing (capped).

use flux_flow::ast::RunEvent;

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let cut: String = line.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Render a RunEvent trace as a compact transcript. `max_steps` caps the listing (0 = no cap).
pub fn render_run_trace(events: &[RunEvent], max_steps: usize) -> String {
    let mut out = String::new();
    let mut steps = 0usize;
    for ev in events {
        match ev {
            RunEvent::StepStarted { op, .. } => {
                if max_steps > 0 && steps >= max_steps {
                    out.push_str("  … (trace truncated)\n");
                    break;
                }
                steps += 1;
                out.push_str(&format!("→ {op}\n"));
            }
            RunEvent::StepSucceeded { .. } => out.push_str("  ✓\n"),
            RunEvent::StepFailed { error, .. } => {
                out.push_str(&format!("  ✗ {}\n", first_line(error, 160)))
            }
            _ => {}
        }
    }
    if out.is_empty() {
        out.push_str("(no operations)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_flow::ast::StepId;

    #[test]
    fn renders_ops_and_errors() {
        let events = vec![
            RunEvent::StepStarted {
                step: StepId("a".into()),
                op: "read".into(),
                input_hash: "h".into(),
            },
            RunEvent::StepSucceeded {
                step: StepId("a".into()),
                output: flux_flow::ast::ValueId("v".into()),
            },
            RunEvent::StepStarted {
                step: StepId("b".into()),
                op: "grep".into(),
                input_hash: "h2".into(),
            },
            RunEvent::StepFailed {
                step: StepId("b".into()),
                error: "regex error: bad\nsecond line".into(),
            },
        ];
        let t = render_run_trace(&events, 0);
        assert!(t.contains("→ read"));
        assert!(t.contains("✓"));
        assert!(t.contains("✗ regex error: bad"));
        assert!(!t.contains("second line")); // only the first line of the error
    }

    #[test]
    fn caps_steps() {
        let many: Vec<RunEvent> = (0..10)
            .map(|i| RunEvent::StepStarted {
                step: StepId(format!("s{i}")),
                op: "bash".into(),
                input_hash: "h".into(),
            })
            .collect();
        let t = render_run_trace(&many, 3);
        assert!(t.contains("truncated"));
        assert_eq!(t.matches("→ bash").count(), 3);
    }
}

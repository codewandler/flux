//! Semantic, **color-free** formatting of a tool/op call for display.
//!
//! Both surfaces share the *content* — what to show for a `bash`/`read`/`grep`/… call — while each
//! applies its own styling (the CLI via `style`, the TUI via ratatui). So this module returns plain
//! strings and never emits ANSI. The agent's only model-facing tool is the planner's `emit_plan`; the
//! ops formatted here are the plan nodes the runtime dispatches (their input is the tool's normal
//! schema-shaped JSON).

use serde_json::Value;

/// A call rendered as a `verb` (the op name) and a human `arg` line (e.g. `$ cargo test`). The `arg`
/// is empty when there is nothing useful to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub verb: String,
    pub arg: String,
}

/// Format an op call into a `{verb, arg}` pair: `bash → $ cargo test`, `read → foo.rs:100-180`,
/// `grep → "needle" in src/`, etc. Unknown ops fall back to a compact `k=v, k=v` of the input object.
pub fn format_call(name: &str, input: &Value) -> Call {
    let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let n = |k: &str| input.get(k).and_then(Value::as_u64);
    let arg = match name {
        "bash" => s("command").map(|c| format!("$ {c}")).unwrap_or_default(),
        "read" => match (s("path"), n("offset"), n("limit")) {
            (Some(p), Some(off), Some(lim)) => format!("{p}:{off}-{}", off + lim),
            (Some(p), Some(off), None) => format!("{p}:{off}-"),
            (Some(p), None, Some(lim)) => format!("{p} (first {lim})"),
            (Some(p), None, None) => p,
            _ => String::new(),
        },
        "write" => match (s("path"), input.get("content").and_then(Value::as_str)) {
            (Some(p), Some(c)) => format!("{p} ({} bytes)", c.len()),
            (Some(p), None) => p,
            _ => String::new(),
        },
        "edit" => s("path").unwrap_or_default(),
        "glob" => {
            let pat = s("pattern").unwrap_or_default();
            match s("path").filter(|p| !p.is_empty() && p != ".") {
                Some(p) => format!("{pat} in {p}"),
                None => pat,
            }
        }
        "grep" => {
            let pat = s("pattern").unwrap_or_default();
            let scope = s("glob")
                .filter(|g| !g.is_empty())
                .or_else(|| s("path").filter(|p| !p.is_empty() && p != "."));
            match scope {
                Some(sc) => format!("{pat:?} in {sc}"),
                None => format!("{pat:?}"),
            }
        }
        "web_fetch" => s("url").unwrap_or_default(),
        "search" => format!("{:?}", s("query").unwrap_or_default()),
        "task" => match (s("role"), s("task")) {
            (Some(r), Some(t)) => format!("{r}: {t}"),
            (Some(r), None) => r,
            (None, Some(t)) => t,
            _ => String::new(),
        },
        _ => fallback_arg(input),
    };
    Call {
        verb: name.to_string(),
        arg,
    }
}

/// A compact `k=v, k=v` rendering of an input object for ops without a bespoke formatter (values
/// shortened so the line stays a glance). Non-objects render as their compact JSON.
fn fallback_arg(input: &Value) -> String {
    match input {
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| format!("{k}={}", short_value(v)))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => short_value(other),
    }
}

fn short_value(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let one_line = s.replace('\n', " ");
            if one_line.chars().count() > 60 {
                let head: String = one_line.chars().take(60).collect();
                format!("{head:?}…")
            } else {
                format!("{one_line:?}")
            }
        }
        other => other.to_string(),
    }
}

/// A semantic one-line summary of a result for ops where the raw content is noisy — `grep`/`glob`/
/// `search` collapse to a match count. Returns `None` when the caller's generic preview is better
/// (so existing result rendering is preserved for everything else).
pub fn format_result(name: &str, content: &str, is_error: bool) -> Option<String> {
    if is_error {
        return None;
    }
    let content = content.trim();
    match name {
        "grep" | "search" if content == "no matches" => Some("no matches".to_string()),
        "glob" if content == "no files match" => Some("no files match".to_string()),
        "grep" | "glob" | "search" => {
            let n = content.lines().filter(|l| !l.trim().is_empty()).count();
            Some(format!("{n} match{}", if n == 1 { "" } else { "es" }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bash_shows_the_command() {
        let c = format_call("bash", &json!({"command": "cargo test --workspace"}));
        assert_eq!(c.verb, "bash");
        assert_eq!(c.arg, "$ cargo test --workspace");
    }

    #[test]
    fn read_shows_path_and_line_range() {
        assert_eq!(
            format_call(
                "read",
                &json!({"path": "foo.rs", "offset": 100, "limit": 80})
            )
            .arg,
            "foo.rs:100-180"
        );
        assert_eq!(
            format_call("read", &json!({"path": "foo.rs"})).arg,
            "foo.rs"
        );
    }

    #[test]
    fn grep_quotes_pattern_and_scopes_it() {
        assert_eq!(
            format_call("grep", &json!({"pattern": "tool_call", "path": "crates/"})).arg,
            "\"tool_call\" in crates/"
        );
        assert_eq!(
            format_call("grep", &json!({"pattern": "x", "path": "."})).arg,
            "\"x\""
        );
    }

    #[test]
    fn write_reports_byte_count() {
        assert_eq!(
            format_call("write", &json!({"path": "a.txt", "content": "hello"})).arg,
            "a.txt (5 bytes)"
        );
    }

    #[test]
    fn task_shows_role_and_task() {
        assert_eq!(
            format_call("task", &json!({"role": "planner", "task": "design X"})).arg,
            "planner: design X"
        );
    }

    #[test]
    fn unknown_op_falls_back_to_compact_kv() {
        let c = format_call("echo", &json!({"value": "hi", "n": 3}));
        assert_eq!(c.verb, "echo");
        // object order is preserved by serde_json's default (BTreeMap-free Map keeps insertion order
        // only with the preserve_order feature; assert the pieces are present instead).
        assert!(c.arg.contains("value=\"hi\""));
        assert!(c.arg.contains("n=3"));
    }

    #[test]
    fn result_counts_matches_for_search_ops() {
        assert_eq!(
            format_result("grep", "a.rs:1: x\nb.rs:2: y", false).as_deref(),
            Some("2 matches")
        );
        assert_eq!(
            format_result("grep", "no matches", false).as_deref(),
            Some("no matches")
        );
        assert_eq!(format_result("bash", "anything", false), None);
        assert_eq!(format_result("grep", "x", true), None); // errors keep the generic preview
    }
}

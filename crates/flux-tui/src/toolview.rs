//! Semantic, **color-free** formatting of a tool/op call for display.
//!
//! Both surfaces share the *content* — what to show for a `bash`/`read`/`grep`/… call — while each
//! applies its own styling (the CLI via `style`, the TUI via ratatui). So this module returns plain
//! strings and never emits ANSI. Model stages call operations through their native schemas; authored
//! Flux flows call the same operations as graph nodes. This formatter handles both paths.

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
        "web.fetch" => s("url").unwrap_or_default(),
        "search" => format!("{:?}", s("query").unwrap_or_default()),
        "append" => match (s("path"), input.get("content").and_then(Value::as_str)) {
            (Some(p), Some(c)) => format!("{p} (+{} bytes)", c.len()),
            (Some(p), None) => p,
            _ => String::new(),
        },
        "patch" => match (s("path"), input.get("edits").and_then(|e| e.as_array())) {
            (Some(p), Some(edits)) => format!(
                "{p} ({} edit{})",
                edits.len(),
                if edits.len() == 1 { "" } else { "s" }
            ),
            (Some(p), None) => p,
            _ => String::new(),
        },
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
        "read" | "read_many" => {
            // Suppress the raw file dump; show a compact line count instead.
            let n = content.lines().count();
            Some(format!("{n} line{}", if n == 1 { "" } else { "s" }))
        }
        "bash" => {
            // For successful bash calls collapse output to a line count so the card stays compact.
            let n = content.lines().filter(|l| !l.trim().is_empty()).count();
            if n == 0 {
                Some("ok".to_string())
            } else {
                Some(format!(
                    "exit 0 · {n} line{}",
                    if n == 1 { "" } else { "s" }
                ))
            }
        }
        _ => None,
    }
}

/// The kind of an expanded-detail line, so the surface can color it (diff add/del, metadata, plain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    Plain,
    Add,
    Del,
    Meta,
    /// A `@@ -a,b +c,d @@` hunk header in a [`format_diff`] hunk view (C-115).
    Hunk,
}

/// One row of a hunk-view diff (C-115): a kind for coloring, optional old/new gutter line numbers
/// (1-based, relative to the diffed snippet), and the text as `(emphasized, s)` spans — `true`
/// marks the word-level intraline change within a modified line pair. Color-free like the rest of
/// this module; the surface maps kind → style and emphasis → modifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DetailKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub spans: Vec<(bool, String)>,
}

impl DiffLine {
    fn plain_row(kind: DetailKind, old_no: Option<u32>, new_no: Option<u32>, text: String) -> Self {
        DiffLine {
            kind,
            old_no,
            new_no,
            spans: vec![(false, text)],
        }
    }
}

/// A real hunk-view diff for `edit`/`write` calls (C-115): `@@` headers, per-side line numbers,
/// and word-level intraline emphasis on changed spans. Built from the call *args*
/// (`old_string`/`new_string`/`content`) — exact, and available before the result, so the
/// approval sheet can preview a pending call. Returns `None` for every other op (callers fall
/// back to [`format_detail`]). Line numbers are relative to the diffed snippet: an `edit`'s
/// `old_string` is a fragment, so its offset within the file is unknown here.
pub fn format_diff(name: &str, input: &Value) -> Option<Vec<DiffLine>> {
    use similar::{ChangeTag, TextDiff};
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    let (old, new) = match name {
        "edit" => (s("old_string")?.to_string(), s("new_string")?.to_string()),
        "write" => (String::new(), s("content")?.to_string()),
        _ => return None,
    };
    let mut out = Vec::new();
    if let Some(p) = s("path") {
        out.push(DiffLine::plain_row(
            DetailKind::Meta,
            None,
            None,
            format!("@ {p}"),
        ));
    }
    let diff = TextDiff::from_lines(&old, &new);
    for group in diff.grouped_ops(2) {
        let (first, last) = match (group.first(), group.last()) {
            (Some(f), Some(l)) => (f, l),
            _ => continue,
        };
        let (os, oe) = (first.old_range().start, last.old_range().end);
        let (ns, ne) = (first.new_range().start, last.new_range().end);
        out.push(DiffLine::plain_row(
            DetailKind::Hunk,
            None,
            None,
            format!("@@ -{},{} +{},{} @@", os + 1, oe - os, ns + 1, ne - ns),
        ));
        for op in &group {
            for change in diff.iter_inline_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => DetailKind::Plain,
                    ChangeTag::Delete => DetailKind::Del,
                    ChangeTag::Insert => DetailKind::Add,
                };
                let mut spans: Vec<(bool, String)> = change
                    .iter_strings_lossy()
                    .map(|(emph, s)| (emph, s.into_owned()))
                    .collect();
                if let Some(last) = spans.last_mut() {
                    while last.1.ends_with('\n') || last.1.ends_with('\r') {
                        last.1.pop();
                    }
                }
                out.push(DiffLine {
                    kind,
                    old_no: change.old_index().map(|i| i as u32 + 1),
                    new_no: change.new_index().map(|i| i as u32 + 1),
                    spans,
                });
            }
        }
    }
    Some(out)
}

/// Expanded detail for a tool call, as color-free `(kind, text)` lines. `edit` becomes a unified
/// `-old`/`+new` diff and `write` a `+`-prefixed new-file preview — both read from the *input*, which
/// is exact and available before the result — while everything else shows the raw result `content`.
/// The caller caps the line count and applies color per [`DetailKind`].
pub fn format_detail(
    name: &str,
    input: &Value,
    content: &str,
    is_error: bool,
) -> Vec<(DetailKind, String)> {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    if is_error {
        return plain(content);
    }
    match name {
        "edit" => {
            let mut out = Vec::new();
            if let Some(p) = s("path") {
                out.push((DetailKind::Meta, format!("@ {p}")));
            }
            for l in s("old_string").unwrap_or_default().lines() {
                out.push((DetailKind::Del, format!("- {l}")));
            }
            for l in s("new_string").unwrap_or_default().lines() {
                out.push((DetailKind::Add, format!("+ {l}")));
            }
            out
        }
        "write" => {
            let mut out = Vec::new();
            if let Some(p) = s("path") {
                out.push((DetailKind::Meta, format!("@ {p}")));
            }
            for l in s("content").unwrap_or_default().lines() {
                out.push((DetailKind::Add, format!("+ {l}")));
            }
            out
        }
        _ => plain(content),
    }
}

fn plain(content: &str) -> Vec<(DetailKind, String)> {
    content
        .trim_end()
        .lines()
        .map(|l| (DetailKind::Plain, l.to_string()))
        .collect()
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
        // successful bash output collapses to a compact summary line
        assert_eq!(
            format_result("bash", "anything", false).as_deref(),
            Some("exit 0 · 1 line")
        );
        assert_eq!(format_result("bash", "boom", true), None); // errors keep the generic preview
        assert_eq!(format_result("grep", "x", true), None); // errors keep the generic preview
    }

    #[test]
    fn edit_detail_is_a_unified_diff() {
        let d = format_detail(
            "edit",
            &json!({"path": "a.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"}),
            "ok",
            false,
        );
        assert_eq!(d[0], (DetailKind::Meta, "@ a.rs".to_string()));
        assert_eq!(d[1], (DetailKind::Del, "- let x = 1;".to_string()));
        assert_eq!(d[2], (DetailKind::Add, "+ let x = 2;".to_string()));
    }

    #[test]
    fn diff_builds_hunks_with_line_numbers() {
        // Two distant changes in a 12-line snippet → two hunks with correct headers.
        let old: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 2", "line two")
            .replace("line 11", "line eleven");
        let d = format_diff(
            "edit",
            &json!({"path": "a.rs", "old_string": old, "new_string": new}),
        )
        .unwrap();
        assert_eq!(d[0].kind, DetailKind::Meta);
        assert_eq!(d[0].spans[0].1, "@ a.rs");
        let hunks: Vec<&DiffLine> = d.iter().filter(|l| l.kind == DetailKind::Hunk).collect();
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].spans[0].1, "@@ -1,4 +1,4 @@");
        assert_eq!(hunks[1].spans[0].1, "@@ -9,4 +9,4 @@");
        // Context rows carry both numbers; del/add rows carry one side only.
        let ctx = d.iter().find(|l| l.kind == DetailKind::Plain).unwrap();
        assert!(ctx.old_no.is_some() && ctx.new_no.is_some());
        let del = d.iter().find(|l| l.kind == DetailKind::Del).unwrap();
        assert_eq!((del.old_no, del.new_no), (Some(2), None));
        let add = d.iter().find(|l| l.kind == DetailKind::Add).unwrap();
        assert_eq!((add.old_no, add.new_no), (None, Some(2)));
    }

    #[test]
    fn diff_emphasizes_intraline_changes() {
        let d = format_diff(
            "edit",
            &json!({"old_string": "let x = 1;\n", "new_string": "let x = 2;\n"}),
        )
        .unwrap();
        let del = d.iter().find(|l| l.kind == DetailKind::Del).unwrap();
        let add = d.iter().find(|l| l.kind == DetailKind::Add).unwrap();
        // The changed word is emphasized; the shared prefix is not.
        assert!(del.spans.iter().any(|(e, s)| *e && s.contains('1')));
        assert!(add.spans.iter().any(|(e, s)| *e && s.contains('2')));
        assert!(del.spans.iter().any(|(e, s)| !*e && s.contains("let x")));
    }

    #[test]
    fn write_diff_is_all_additions_with_new_numbers() {
        let d = format_diff("write", &json!({"path": "n.txt", "content": "a\nb\n"})).unwrap();
        let adds: Vec<&DiffLine> = d.iter().filter(|l| l.kind == DetailKind::Add).collect();
        assert_eq!(adds.len(), 2);
        assert_eq!((adds[0].old_no, adds[0].new_no), (None, Some(1)));
        assert_eq!((adds[1].old_no, adds[1].new_no), (None, Some(2)));
        assert!(d.iter().all(|l| l.kind != DetailKind::Del));
    }

    #[test]
    fn diff_is_none_for_other_ops() {
        assert_eq!(format_diff("bash", &json!({"command": "ls"})), None);
        assert_eq!(format_diff("read", &json!({"path": "a.rs"})), None);
    }

    #[test]
    fn bash_detail_is_plain_content() {
        let d = format_detail("bash", &json!({"command": "ls"}), "a.rs\nb.rs", false);
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|(k, _)| *k == DetailKind::Plain));
    }
}

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
        // C-535: argv-only — show the argv, and no `$`, which is the bash spelling and implies a
        // shell this op deliberately does not have.
        "proc.run" => {
            let mut argv = s("program").unwrap_or_default();
            if let Some(args) = input.get("args").and_then(Value::as_array) {
                for a in args.iter().filter_map(Value::as_str) {
                    if !argv.is_empty() {
                        argv.push(' ');
                    }
                    argv.push_str(a);
                }
            }
            argv
        }
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
        // C-535: a size digest — the raw first body line is a poor summary, and the full body is
        // one expand away.
        "web.fetch" if !content.is_empty() => {
            let lines = content.lines().count();
            let bytes = content.len();
            let size = if bytes < 1024 {
                format!("{bytes} B")
            } else {
                format!("{:.1} KB", bytes as f64 / 1024.0)
            };
            Some(format!(
                "{lines} line{} · {size}",
                if lines == 1 { "" } else { "s" }
            ))
        }
        // Native Board/Fleet operations answer with a bounded envelope whose first ~100 characters
        // are `{"data":{"bounded":true,"byte_limit":262144,...` — the same prefix for every call, so
        // the raw head is the least informative possible summary while also being the widest. Report
        // the shape instead; the full envelope stays one expand away.
        name if name.starts_with("board.") || name.starts_with("fleet.") => {
            summarize_control_plane(name, content)
        }
        _ => None,
    }
}

/// Count-and-identity summary for a `board.*` / `fleet.*` envelope.
///
/// Deliberately reports only counts and short identifiers already present in the payload — never a
/// free-text field — so a summary line cannot become a channel for unbounded or attacker-shaped text
/// in the transcript.
fn summarize_control_plane(name: &str, content: &str) -> Option<String> {
    let envelope: Value = serde_json::from_str(content).ok()?;
    // Both `flux.fleet-inspect/v1` and the board views nest the useful body under `data.data`.
    let body = envelope
        .get("data")
        .map(|data| data.get("data").unwrap_or(data))?;

    let count = |key: &str| {
        body.get(key)
            .and_then(Value::as_array)
            .map(|items| items.len())
    };
    let number = |key: &str| body.get(key).and_then(Value::as_u64);

    let mut parts: Vec<String> = Vec::new();
    if let Some(items) = count("items") {
        parts.push(format!("{items} item{}", if items == 1 { "" } else { "s" }));
    }
    if let Some(agents) = count("agents").or_else(|| number("agents").map(|n| n as usize)) {
        parts.push(format!(
            "{agents} worker{}",
            if agents == 1 { "" } else { "s" }
        ));
    }
    if let Some(waves) = number("waves") {
        parts.push(format!("{waves} wave{}", if waves == 1 { "" } else { "s" }));
    }
    if let Some(program) = count("program") {
        parts.push(format!(
            "{program} program row{}",
            if program == 1 { "" } else { "s" }
        ));
    }
    if let Some(revision) = envelope.get("revision").and_then(Value::as_u64) {
        parts.push(format!("r{revision}"));
    }
    if parts.is_empty() {
        // Still better than the envelope head: say it answered, and how big the answer was.
        let bytes = content.len();
        parts.push(if bytes < 1024 {
            format!("{bytes} B")
        } else {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        });
    }
    let _ = name;
    Some(parts.join(" · "))
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
///
/// **C-195: this renders the input verbatim, and that is deliberate — do not add a `Redactor`
/// here.** It was asked and settled; the argument is in `docs/designs/security-assurance.md`
/// ("The approval sheet does not redact"). In short: redaction is a *boundary* control, applied
/// where content crosses into persistence, a provider, or a machine consumer — the dispatch result
/// (`flux_runtime`), the evidence flush, stream-json, the export, otel. This function feeds none of
/// them. Its callers are the approval sheet's preview and the transcript tool card — live renders to
/// the operator's own TTY of bytes the operator is being asked to authorize. Scrubbing a credential
/// out of that preview would not stop the write, only hide it from the one person able to deny it —
/// and since the redactor is a lossy heuristic, it would also make the sheet an unfaithful account
/// of the pending effect. Reopen only if the TUI grows a persistence/sharing path (redact at the
/// point of persistence, not here) or if the broader C-132/C-185 "redact conversation text at write
/// time" question lands.
pub fn format_diff(name: &str, input: &Value) -> Option<Vec<DiffLine>> {
    use similar::{ChangeTag, TextDiff};
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    let (old, new) = match name {
        "edit" => (s("old_string")?.to_string(), s("new_string")?.to_string()),
        "write" => (String::new(), s("content")?.to_string()),
        "patch" => return patch_diff(input),
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

/// C-534: a `patch` call's input-anchored hunk view — one `@@` header per edit naming its op and
/// original-line range, `+` rows for inserted/replacement text. The original file is not in the
/// args, only line anchors, so there are no `-` rows: the header states what each edit displaces,
/// which is exactly what the input pledges and all that is knowable before execution (the tool's
/// *result* carries the true unified diff, classified by [`classify_unified_diff`] once it lands).
/// **C-195 applies here unchanged** (see [`format_diff`]): the input is rendered verbatim — this
/// feeds the approval sheet, and scrubbing a preview would hide the pending write from the one
/// person able to deny it.
fn patch_diff(input: &Value) -> Option<Vec<DiffLine>> {
    let edits = input.get("edits")?.as_array()?;
    if edits.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    if let Some(p) = input.get("path").and_then(Value::as_str) {
        out.push(DiffLine::plain_row(
            DetailKind::Meta,
            None,
            None,
            format!("@ {p}"),
        ));
    }
    let total = edits.len();
    for (i, edit) in edits.iter().enumerate() {
        let op = edit.get("op").and_then(Value::as_str).unwrap_or("?");
        let line = edit.get("line").and_then(Value::as_u64).unwrap_or(0);
        let end = edit.get("end_line").and_then(Value::as_u64).unwrap_or(line);
        let (verb, ranged) = match op {
            "insert_before" => ("insert before", false),
            "insert_after" => ("insert after", false),
            "replace_range" => ("replace", true),
            "delete_range" => ("delete", true),
            other => (other, false),
        };
        let anchor = if ranged && end > line {
            format!("lines {line}-{end}")
        } else {
            format!("line {line}")
        };
        out.push(DiffLine::plain_row(
            DetailKind::Hunk,
            None,
            None,
            format!("@@ edit {}/{total} · {verb} {anchor} @@", i + 1),
        ));
        for l in edit
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .lines()
        {
            out.push(DiffLine::plain_row(
                DetailKind::Add,
                None,
                None,
                format!("+ {l}"),
            ));
        }
    }
    Some(out)
}

/// Expanded detail for a tool call, as color-free `(kind, text)` lines. `edit` becomes a unified
/// `-old`/`+new` diff and `write` a `+`-prefixed new-file preview — both read from the *input*, which
/// is exact and available before the result — while everything else shows the raw result `content`,
/// classified as a unified diff when it is one (C-534, [`classify_unified_diff`]).
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
        _ => classify_unified_diff(content).unwrap_or_else(|| plain(content)),
    }
}

/// C-534: classify result content that is itself a unified diff — `git_diff` output, a `bash git
/// diff`, a `patch` result view — into diff row kinds, keyed on content *shape* rather than op
/// name. Requires diff structure (a `@@ -a[,b] +c[,d] @@` hunk header somewhere), not merely
/// `+`/`-` line prefixes, so prose bullets are never misclassified. Returns `None` for anything
/// that is not a unified diff. Content is classified, never altered.
fn classify_unified_diff(content: &str) -> Option<Vec<(DetailKind, String)>> {
    fn is_hunk_header(line: &str) -> bool {
        let Some(rest) = line.strip_prefix("@@ -") else {
            return false;
        };
        rest.contains(" +") && rest.contains(" @@")
    }
    if !content.lines().any(is_hunk_header) {
        return None;
    }
    let meta_prefixes = [
        "diff --git ",
        "index ",
        "--- ",
        "+++ ",
        "new file mode",
        "deleted file mode",
        "old mode",
        "new mode",
        "rename from ",
        "rename to ",
        "similarity index",
        "Binary files ",
        "\\ No newline",
    ];
    Some(
        content
            .trim_end()
            .lines()
            .map(|l| {
                let kind = if is_hunk_header(l) {
                    DetailKind::Hunk
                } else if meta_prefixes.iter().any(|p| l.starts_with(p)) {
                    DetailKind::Meta
                } else if l.starts_with('+') {
                    DetailKind::Add
                } else if l.starts_with('-') {
                    DetailKind::Del
                } else {
                    DetailKind::Plain
                };
                (kind, l.to_string())
            })
            .collect(),
    )
}

fn plain(content: &str) -> Vec<(DetailKind, String)> {
    content
        .trim_end()
        .lines()
        .map(|l| (DetailKind::Plain, l.to_string()))
        .collect()
}

/// C-539: the one place both surfaces' tool-output elision budgets are declared. The numbers may
/// differ per surface **on purpose** — the CLI cannot expand a finished card, so it shows more up
/// front, while the TUI's expanded detail is one keypress (and `-v`) away from a full view — but
/// they are declared side by side so a change to one is made in sight of the other, instead of
/// drifting as private literals.
pub mod budget {
    /// TUI: expanded-detail row cap per card (lifted by `-v`/`FLUX_VERBOSE`).
    pub const TUI_DETAIL_LINES: usize = 30;
    /// CLI: preview line cap in `tool_preview` (lifted by `-v`).
    pub const CLI_PREVIEW_LINES: usize = 40;
    /// CLI: per-line character cap in previews (lifted by `-v`).
    pub const CLI_PREVIEW_LINE_CHARS: usize = 500;
    /// CLI: head lines shown for a `read`/`read_many` digest.
    pub const CLI_READ_HEAD_LINES: usize = 3;
    /// CLI: head matches shown for a `grep` digest.
    pub const CLI_GREP_HEAD_LINES: usize = 3;
    /// CLI: head paths shown for a `glob` digest.
    pub const CLI_GLOB_HEAD_LINES: usize = 5;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `fleet.status` card used to summarize as the first ~100 chars of its envelope —
    /// `{"data":{"bounded":true,"byte_limit":262144,…` — identical for every control-plane call and
    /// the widest possible line. Report the shape instead.
    #[test]
    fn control_plane_results_summarize_shape_not_envelope_head() {
        let envelope = json!({
            "revision": 276,
            "data": {"data": {"agents": 31, "waves": 16, "goals": 0}}
        })
        .to_string();

        let summary = format_result("fleet.status", &envelope, false)
            .expect("a control-plane envelope summarizes");

        assert!(summary.contains("31 workers"), "{summary}");
        assert!(summary.contains("16 waves"), "{summary}");
        assert!(summary.contains("r276"), "{summary}");
        assert!(
            !summary.contains("byte_limit"),
            "the envelope head must not leak: {summary}"
        );
    }

    /// Board reads report how many items came back, not the first bytes of the payload.
    #[test]
    fn board_results_report_item_counts() {
        let envelope =
            json!({"data": {"items": [{"id": "flux/C-1"}, {"id": "flux/C-2"}]}}).to_string();

        let summary =
            format_result("board.next", &envelope, false).expect("a board envelope summarizes");

        assert!(summary.contains("2 items"), "{summary}");
    }

    /// A payload that carries none of the known shapes still beats the raw head, and an error result
    /// keeps the existing behaviour of showing the real message.
    #[test]
    fn control_plane_falls_back_to_a_size_and_never_summarizes_errors() {
        let opaque = json!({"data": {"something_else": true}}).to_string();
        let summary = format_result("board.show", &opaque, false).expect("falls back");
        assert!(
            summary.ends_with(" B") || summary.ends_with("KB"),
            "{summary}"
        );

        assert_eq!(format_result("fleet.status", "boom", true), None);
    }

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

    /// C-195: the approval sheet's preview renders a credential on an added line **verbatim**, by
    /// decision — see `format_diff`'s doc comment and `docs/designs/security-assurance.md`. This is
    /// not a failing-first test (it pins behavior that already held); it exists so that adding a
    /// `Redactor` to this path is a deliberate act that breaks a test pointing at the design doc,
    /// rather than a silent "hardening" that blinds the operator at the approval gate.
    #[test]
    fn diff_does_not_redact_credentials_by_decision() {
        // Deliberately the exact literal `flux_secret`'s own `redacts_credential_shapes` test
        // asserts is scrubbed (`flux-secret/src/lib.rs:314-316`), and the marker-glued shape C-185
        // taught `redact_patterns` to catch — so this test provably fails the moment a `Redactor`
        // is threaded through here, rather than merely asserting today's output.
        const CRED: &str = "sk-ant-abc123def456";
        let text = |rows: &[DiffLine]| -> String {
            rows.iter()
                .flat_map(|r| r.spans.iter().map(|(_, s)| s.as_str()))
                .collect()
        };

        let d = format_diff(
            "write",
            &json!({"path": ".env", "content": format!("KEY={CRED}\n")}),
        )
        .unwrap();
        assert!(text(&d).contains(CRED), "write preview redacted: {:?}", d);

        let d = format_diff(
            "edit",
            &json!({"path": ".env", "old_string": "KEY=\n", "new_string": format!("KEY={CRED}\n")}),
        )
        .unwrap();
        assert!(text(&d).contains(CRED), "edit preview redacted: {:?}", d);
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

    /// C-534: a `patch` call gets an input-anchored hunk view — one `@@` header per edit naming
    /// its op and original-line range, `+` rows for the new text. The original file is not in the
    /// args, so there are no `-` rows; the header states what the edit displaces.
    #[test]
    fn patch_input_renders_an_input_anchored_hunk_view() {
        let d = format_diff(
            "patch",
            &json!({"path": "src/a.rs", "edits": [
                {"op": "replace_range", "line": 10, "end_line": 12, "text": "new a\nnew b"},
                {"op": "delete_range", "line": 30, "end_line": 31},
                {"op": "insert_after", "line": 40, "text": "tail"},
            ]}),
        )
        .expect("patch gets a diff view");
        let text = |rows: &[DiffLine]| -> Vec<String> {
            rows.iter()
                .map(|r| r.spans.iter().map(|(_, s)| s.as_str()).collect())
                .collect()
        };
        let rows = text(&d);
        assert_eq!(d[0].kind, DetailKind::Meta);
        assert_eq!(rows[0], "@ src/a.rs");
        assert!(rows[1].contains("edit 1/3") && rows[1].contains("replace lines 10-12"));
        assert_eq!(d[1].kind, DetailKind::Hunk);
        assert_eq!(rows[2], "+ new a");
        assert_eq!(d[2].kind, DetailKind::Add);
        assert_eq!(rows[3], "+ new b");
        assert!(rows[4].contains("edit 2/3") && rows[4].contains("delete lines 30-31"));
        assert!(rows[5].contains("edit 3/3") && rows[5].contains("insert after line 40"));
        assert_eq!(rows[6], "+ tail");
    }

    /// C-534: result content that is itself a unified diff — `git_diff` output, a `bash git diff`
    /// — is classified into diff row kinds instead of rendering flat.
    #[test]
    fn unified_diff_content_is_classified() {
        let content = "diff --git a/foo.rs b/foo.rs\nindex 1111111..2222222 100644\n\
                       --- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,3 @@ fn head()\n context\n\
                       -old line\n+new line";
        let d = format_detail("git_diff", &json!({}), content, false);
        let kinds: Vec<DetailKind> = d.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![
                DetailKind::Meta,
                DetailKind::Meta,
                DetailKind::Meta,
                DetailKind::Meta,
                DetailKind::Hunk,
                DetailKind::Plain,
                DetailKind::Del,
                DetailKind::Add,
            ],
            "rows: {d:?}"
        );
    }

    /// C-535: `proc.run` is argv-only — the header shows the argv, not a `k=v` dump (and no `$`,
    /// which is the bash spelling and implies a shell this op deliberately does not have).
    #[test]
    fn proc_run_shows_the_argv() {
        assert_eq!(
            format_call(
                "proc.run",
                &json!({"program": "rg", "args": ["--files", "-g", "*.rs"]})
            )
            .arg,
            "rg --files -g *.rs"
        );
        assert_eq!(format_call("proc.run", &json!({"program": "ls"})).arg, "ls");
    }

    /// C-535: `web.fetch` collapses to a size digest — the raw first body line is a poor summary
    /// and the full body is one expand away.
    #[test]
    fn web_fetch_summarizes_size_not_first_body_line() {
        let body = "# Title\n\nSome readable text.\nMore.";
        assert_eq!(
            format_result("web.fetch", body, false).as_deref(),
            Some(format!("4 lines · {} B", body.len()).as_str())
        );
        assert_eq!(format_result("web.fetch", "   ", false), None);
    }

    /// C-534: the classifier requires diff *structure* (a hunk header), not merely `+`/`-`
    /// prefixes — prose bullets and option listings stay plain.
    #[test]
    fn prose_with_dash_bullets_is_not_a_diff() {
        let d = format_detail(
            "bash",
            &json!({}),
            "- bullet one\n- bullet two\n+ a plus-prefixed line",
            false,
        );
        assert!(
            d.iter().all(|(k, _)| *k == DetailKind::Plain),
            "rows: {d:?}"
        );
    }
}

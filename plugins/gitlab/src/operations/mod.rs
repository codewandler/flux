//! Operation families plus their shared input, contribution, and diff helpers.

use super::*;

mod ci;
mod core;
mod releases;
mod repository;

pub(super) use ci::*;
pub(super) use core::*;
pub(super) use releases::*;
pub(super) use repository::*;

// ---------------------------------------------------------------------------
// Input helpers.
// ---------------------------------------------------------------------------

/// Percent-encode an id/path/value so `group/app` → `group%2Fapp` for a URL segment or query value.
pub(super) fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A trimmed string for `key`, accepting a JSON string or number; `None` when absent/empty.
pub(super) fn flex_str(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// The first present integer across `keys`, accepting a JSON integer or numeric string.
pub(super) fn flex_i64(input: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        match input.get(*key) {
            Some(Value::Number(n)) => {
                if let Some(i) = n.as_i64() {
                    return Some(i);
                }
            }
            Some(Value::String(s)) => {
                if let Ok(i) = s.trim().parse::<i64>() {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The project id/path from `project`/`project_id`/`path` aliases.
pub(super) fn req_project(input: &Value) -> Result<String, String> {
    for key in ["project", "project_id", "path"] {
        if let Some(s) = flex_str(input, key) {
            return Ok(s);
        }
    }
    Err("`project` (string) required".into())
}

/// Resolve `project` (already numeric, or a `namespace/path`) to its numeric project id.
///
/// GitLab's `job_token_scope/allowlist` and `groups_allowlist` POST/DELETE handlers reject the
/// URL-encoded `namespace%2Fproject` path form with `400 {"error":"id is invalid"}`, even though the
/// matching GET accepts it — they want the numeric id. Resolve it via `/projects/:id` (which does
/// accept the encoded path) rather than encoding a path into these endpoints.
pub(super) fn resolve_project_id(host: &mut Host, project: &str) -> Result<i64, String> {
    if let Ok(id) = project.parse::<i64>() {
        return Ok(id);
    }
    let obj = gl_get(host, &format!("/projects/{}", enc(project)))?;
    obj.get("id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("could not resolve project `{project}` to a numeric id"))
}

/// Resolve a merge request to (project, iid) from a `ref`/`id` (PROJECT!IID) or project + iid.
pub(super) fn mr_address(input: &Value) -> Result<(String, i64), String> {
    let reference = flex_str(input, "ref").or_else(|| flex_str(input, "id"));
    let project = ["project", "project_id", "path"]
        .into_iter()
        .find_map(|key| flex_str(input, key));
    resolve_mr_address(
        reference.as_deref(),
        project.as_deref(),
        flex_i64(input, &["iid", "merge_request_iid"]),
    )
}

/// Resolve the canonical MR address shared by flexible preflight and C-74 typed handlers.
pub(super) fn resolve_mr_address(
    reference: Option<&str>,
    project: Option<&str>,
    iid: Option<i64>,
) -> Result<(String, i64), String> {
    if let Some(reference) = reference.map(str::trim).filter(|value| !value.is_empty()) {
        let (project, iid) = reference
            .split_once('!')
            .ok_or("merge request ref must be PROJECT!IID")?;
        let iid = iid
            .trim()
            .parse::<i64>()
            .map_err(|_| "merge request ref must be PROJECT!IID".to_string())?;
        if project.trim().is_empty() || iid <= 0 {
            return Err("merge request ref must be PROJECT!IID".into());
        }
        return Ok((project.trim().to_string(), iid));
    }
    let project = project
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("`project` (string) required")?;
    let iid = iid
        .filter(|iid| *iid > 0)
        .ok_or("`iid` (integer) required")?;
    Ok((project.to_string(), iid))
}

/// Resolve an issue to (project, iid) from a `ref`/`id` (PROJECT#IID) or project + iid.
pub(super) fn issue_address(input: &Value) -> Result<(String, i64), String> {
    let reference = flex_str(input, "ref").or_else(|| flex_str(input, "id"));
    let project = ["project", "project_id", "path"]
        .into_iter()
        .find_map(|key| flex_str(input, key));
    resolve_issue_address(
        reference.as_deref(),
        project.as_deref(),
        flex_i64(input, &["iid", "issue_iid"]),
    )
}

/// Resolve the canonical issue address shared by flexible preflight and C-74 typed handlers.
pub(super) fn resolve_issue_address(
    reference: Option<&str>,
    project: Option<&str>,
    iid: Option<i64>,
) -> Result<(String, i64), String> {
    if let Some(reference) = reference.map(str::trim).filter(|value| !value.is_empty()) {
        let (project, iid) = reference
            .split_once('#')
            .ok_or("issue ref must be PROJECT#IID")?;
        let iid = iid
            .trim()
            .parse::<i64>()
            .map_err(|_| "issue ref must be PROJECT#IID".to_string())?;
        if project.trim().is_empty() || iid <= 0 {
            return Err("issue ref must be PROJECT#IID".into());
        }
        return Ok((project.trim().to_string(), iid));
    }
    let project = project
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("`project` (string) required")?;
    let iid = iid
        .filter(|iid| *iid > 0)
        .ok_or("`iid` (integer) required")?;
    Ok((project.to_string(), iid))
}

// ─── custom preflight rules (D-88) ──────────────────────────────────────────
// Constraints the generated schemas cannot express, attached via `PluginBuilder::preflight` so
// host-kit runs them in BOTH `--dry-run` (`plugin.validate`) and runtime dispatch. Each rule
// reuses the SAME resolution helper its handler calls, so the two verdicts cannot drift.

/// GL-004: a merge-request target — `ref`/`id` (PROJECT!IID) or `project` + `iid`.
pub(super) fn pf_mr_address(input: &Value) -> Vec<String> {
    mr_address(input).err().into_iter().collect()
}

/// GL-004: an issue target — `ref`/`id` (PROJECT#IID) or `project` + `iid`.
pub(super) fn pf_issue_address(input: &Value) -> Vec<String> {
    issue_address(input).err().into_iter().collect()
}

/// GL-021: an update op must carry at least one updatable field.
pub(super) fn pf_any_update(input: &Value, keys: &[&str]) -> Vec<String> {
    if body_from(input, keys).is_empty() {
        vec![format!("nothing to update: pass {}", keys.join(", "))]
    } else {
        Vec::new()
    }
}

/// GL-027: `mr.diff.lines` — the MR target, plus `search` must be a compilable regex.
pub(super) fn pf_mr_diff_lines(input: &Value) -> Vec<String> {
    let mut problems = pf_mr_address(input);
    if let Some(s) = flex_str(input, "search") {
        if let Err(e) = Regex::new(&s) {
            problems.push(format!("search: {e}"));
        }
    }
    problems
}

/// GL-036: `mr.discussion.create` — the MR target, plus the line-anchor conditionals
/// (`path` + `new_line`/`old_line` travel together).
pub(super) fn pf_mr_discussion_create(input: &Value) -> Vec<String> {
    let mut problems = pf_mr_address(input);
    let path = flex_str(input, "path");
    let new_line = flex_i64(input, &["new_line"]);
    let old_line = flex_i64(input, &["old_line"]);
    if path.is_some() || new_line.is_some() || old_line.is_some() {
        if path.is_none() {
            problems.push("`path` is required for a line-level comment".into());
        }
        if new_line.is_none() && old_line.is_none() {
            problems.push("`new_line` or `old_line` is required for a line-level comment".into());
        }
    }
    problems
}

/// GL-029: `snippet.delete` — `snippet_id` (or its `id` alias) is required.
pub(super) fn pf_snippet_delete(input: &Value) -> Vec<String> {
    if flex_i64(input, &["snippet_id", "id"]).is_none() {
        vec!["`snippet_id` (integer) required".into()]
    } else {
        Vec::new()
    }
}

/// GL-028: `branch` (or its `name` alias) is required.
pub(super) fn pf_branch(input: &Value) -> Vec<String> {
    if flex_str(input, "branch")
        .or_else(|| flex_str(input, "name"))
        .is_none()
    {
        vec!["`branch` (string) required".into()]
    } else {
        Vec::new()
    }
}

/// GL-028: a tag op's `tag_name` (or a documented alias) is required.
pub(super) fn pf_tag_name(input: &Value) -> Vec<String> {
    tag_name(input).err().into_iter().collect()
}

/// GL-028: a release op's `tag_name`/`tag` is required (`name` is a display name, never the tag).
pub(super) fn pf_release_tag(input: &Value) -> Vec<String> {
    release_tag(input).err().into_iter().collect()
}

/// GL-032/GL-041: blob-search scope must be unambiguous — `project` OR `group`, never both —
/// and `ref` only exists project-scoped.
pub(super) fn pf_search_blobs(input: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let project = flex_str(input, "project");
    let group = flex_str(input, "group");
    if project.is_some() && group.is_some() {
        problems.push("pass `project` OR `group`, not both (ambiguous search scope)".into());
    }
    if group.is_some() && flex_str(input, "ref").is_some() {
        problems.push(
            "`ref` is not supported for group-scoped blob search (project scope only)".into(),
        );
    }
    problems
}

/// GL-034: index selectors must resolve to at least one known index.
pub(super) fn pf_index_build(input: &Value) -> Vec<String> {
    index_include(input).err().into_iter().collect()
}

/// `&page=N` when the caller asked for a specific 1-based results page (GL-019), else "".
pub(super) fn page_qs(input: &Value) -> String {
    flex_i64(input, &["page"])
        .map(|p| format!("&page={p}"))
        .unwrap_or_default()
}

/// Clamp a 1-based `limit` to `[1, max]`, falling back to `default` when unset/non-positive.
pub(super) fn clamp(value: i64, default: i64, max: i64) -> i64 {
    if value <= 0 {
        default
    } else if value > max {
        max
    } else {
        value
    }
}

/// Copy each present, non-null `key` from `input` into a fresh body map.
pub(super) fn body_from(input: &Value, keys: &[&str]) -> Map<String, Value> {
    let mut m = Map::new();
    for key in keys {
        if let Some(v) = input.get(*key) {
            if !v.is_null() {
                m.insert((*key).to_string(), v.clone());
            }
        }
    }
    m
}

/// Build `?k=v&...` (values percent-encoded); empty values are dropped, empty result is "".
pub(super) fn qs(pairs: &[(&str, String)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

// ---------------------------------------------------------------------------
// Datasource contribution.
// ---------------------------------------------------------------------------

/// Contribute `gitlab.project` records keyed by `path_with_namespace`; returns the count contributed.
pub(super) fn contribute_projects(host: &mut Host, projects: &Value) -> usize {
    let Some(arr) = projects.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|p| {
            let id = p.get("path_with_namespace").and_then(|v| v.as_str())?;
            Some(Record::new(
                Source::new("gitlab"),
                "gitlab.project",
                id,
                p.get("name_with_namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or(id),
                p.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    let n = records.len();
    if n > 0 {
        let _ = host.contribute(&records);
    }
    n
}

/// Contribute project-scoped MR/issue list items keyed by `<project>!<iid>` with title/description;
/// returns the count contributed.
pub(super) fn contribute_list(
    host: &mut Host,
    items: &Value,
    entity: &str,
    project: &str,
) -> usize {
    let Some(arr) = items.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let id = it.get("iid").map(|v| v.to_string())?;
            Some(Record::new(
                Source::new("gitlab"),
                entity,
                format!("{project}!{}", id.trim_matches('"')),
                it.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    let n = records.len();
    if n > 0 {
        let _ = host.contribute(&records);
    }
    n
}

/// Contribute global MR/issue list items, deriving the `project!iid` / `project#iid` id from each
/// item's `references.full` (falling back to the numeric id); returns the count contributed.
pub(super) fn contribute_refs(host: &mut Host, items: &Value, entity: &str) -> usize {
    let Some(arr) = items.as_array() else {
        return 0;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let id = it
                .get("references")
                .and_then(|r| r.get("full"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .or_else(|| {
                    it.get("id")
                        .map(|v| v.to_string().trim_matches('"').to_string())
                })?;
            Some(Record::new(
                Source::new("gitlab"),
                entity,
                id,
                it.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                it.get("description").and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    let n = records.len();
    if n > 0 {
        let _ = host.contribute(&records);
    }
    n
}

// ---------------------------------------------------------------------------
// Unified-diff parsing (for mr.diff.lines and mr.discussion.create anchoring).
// ---------------------------------------------------------------------------

/// One parsed diff line: `kind` is `added` | `deleted` | `context`; line numbers are 1-based (0 = N/A).
pub(super) struct DiffLine {
    kind: &'static str,
    old_line: i64,
    new_line: i64,
    content: String,
}

pub(super) fn diff_line_json(l: &DiffLine) -> Value {
    json!({ "type": l.kind, "old_line": l.old_line, "new_line": l.new_line, "content": l.content })
}

/// Parse a unified diff body (hunks; no `diff --git`/`---`/`+++` file headers expected from GitLab).
pub(super) fn parse_unified_diff(diff: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut old_no = 0i64;
    let mut new_no = 0i64;
    for line in diff.split('\n') {
        if line.starts_with("@@") {
            if let Some(header) = line.strip_prefix("@@").and_then(|r| r.split_once("@@")) {
                for tok in header.0.split_whitespace() {
                    if let Some(t) = tok.strip_prefix('-') {
                        old_no = t.split(',').next().unwrap_or("0").parse().unwrap_or(0);
                    } else if let Some(t) = tok.strip_prefix('+') {
                        new_no = t.split(',').next().unwrap_or("0").parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("diff ")
            || line.starts_with('\\')
        {
            continue;
        }
        match line.chars().next() {
            Some('+') => {
                out.push(DiffLine {
                    kind: "added",
                    old_line: 0,
                    new_line: new_no,
                    content: line[1..].to_string(),
                });
                new_no += 1;
            }
            Some('-') => {
                out.push(DiffLine {
                    kind: "deleted",
                    old_line: old_no,
                    new_line: 0,
                    content: line[1..].to_string(),
                });
                old_no += 1;
            }
            Some(' ') => {
                out.push(DiffLine {
                    kind: "context",
                    old_line: old_no,
                    new_line: new_no,
                    content: line[1..].to_string(),
                });
                old_no += 1;
                new_no += 1;
            }
            _ => {}
        }
    }
    out
}

/// Find one file's diff object within an MR/compare change set by `new_path` or `old_path`.
/// The diff entry for `file`, paginating the MR diff list past a single page (GL-043) — a file
/// beyond the first page of changed files is still addressable. `None` when the file is not part
/// of the merge request.
pub(super) fn fetch_file_diff(
    host: &mut Host,
    project: &str,
    iid: i64,
    file: &str,
) -> Result<Option<Value>, String> {
    let mut page = 1;
    loop {
        let diffs = gl_get(
            host,
            &format!(
                "/projects/{}/merge_requests/{iid}/diffs?per_page=100&page={page}",
                enc(project)
            ),
        )?;
        if let Some(fd) = find_file_diff(&diffs, file) {
            return Ok(Some(fd.clone()));
        }
        if diffs.as_array().map(|a| a.len()).unwrap_or(0) < 100 {
            return Ok(None);
        }
        page += 1;
    }
}

pub(super) fn find_file_diff<'a>(diffs: &'a Value, file: &str) -> Option<&'a Value> {
    diffs.as_array()?.iter().find(|f| {
        f.get("new_path").and_then(|v| v.as_str()) == Some(file)
            || f.get("old_path").and_then(|v| v.as_str()) == Some(file)
    })
}

/// Truncate `s` so the RESULT — marker included — is at most `max` bytes on a char boundary;
/// `None` if it fits. The cap is a promise about the returned string (GL-035); when `max` is too
/// small to fit the marker, the bare capped prefix is returned (the caller's `*_truncated` flag
/// still signals the cut).
pub(super) fn cap_bytes(s: &str, max: usize) -> Option<String> {
    const MARKER: &str = "\n[diff truncated]";
    if max == 0 || s.len() <= max {
        return None;
    }
    let budget = max.saturating_sub(MARKER.len());
    let mut end = budget;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        let mut bare = max.min(s.len());
        while bare > 0 && !s.is_char_boundary(bare) {
            bare -= 1;
        }
        return Some(s[..bare].to_string());
    }
    Some(format!("{}{MARKER}", &s[..end]))
}

// ---------------------------------------------------------------------------
// CI/CD job-token scope, protected tags, deploy tokens (CI governance).
// ---------------------------------------------------------------------------

/// Fat-finger guard for a destructive op: when a `confirm_*` integer field is supplied it must equal
/// the target, else the op is refused; an absent confirm is allowed (so automation stays ergonomic).
pub(super) fn confirm_i64(input: &Value, field: &str, expected: i64) -> Result<(), String> {
    match flex_i64(input, &[field]) {
        Some(c) if c == expected => Ok(()),
        Some(_) => Err(format!(
            "`{field}` does not match the target — refusing to proceed"
        )),
        None => Ok(()),
    }
}

/// String counterpart of [`confirm_i64`].
pub(super) fn confirm_str(input: &Value, field: &str, expected: &str) -> Result<(), String> {
    match flex_str(input, field) {
        Some(c) if c == expected => Ok(()),
        Some(_) => Err(format!(
            "`{field}` does not match the target — refusing to proceed"
        )),
        None => Ok(()),
    }
}

//! Repository, branch, commit, tag, snippet, search, and review operations.

use super::*;

// ---------------------------------------------------------------------------
// Branches.
// ---------------------------------------------------------------------------

pub(crate) fn branch_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let branch = flex_str(&input, "branch")
        .or_else(|| flex_str(&input, "name"))
        .ok_or("`branch` (string) required")?;
    let git_ref = flex_str(&input, "ref").ok_or("`ref` (string) required")?;
    gl_post(
        host,
        &format!("/projects/{}/repository/branches", enc(&project)),
        &json!({ "branch": branch, "ref": git_ref }),
    )
}

pub(crate) fn branch_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let branch = flex_str(&input, "branch")
        .or_else(|| flex_str(&input, "name"))
        .ok_or("`branch` (string) required")?;
    confirm_str(&input, "confirm_branch", &branch)?;
    gl_delete(
        host,
        &format!(
            "/projects/{}/repository/branches/{}",
            enc(&project),
            enc(&branch)
        ),
    )?;
    Ok(json!({ "project": project, "branch": branch, "message": "branch deleted" }))
}

pub(crate) fn branch_delete_merged(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    confirm_str(&input, "confirm_project", &project)?;
    gl_delete(
        host,
        &format!("/projects/{}/repository/merged_branches", enc(&project)),
    )?;
    Ok(json!({ "project": project, "message": "merged branches deletion requested" }))
}

// ---------------------------------------------------------------------------
// Repository files + tree.
// ---------------------------------------------------------------------------

pub(crate) fn repo_file_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, file_path) = repo_file_target(&input)?;
    require_keys(&input, &["branch", "content", "commit_message"])?;
    let body = body_from(
        &input,
        &[
            "branch",
            "content",
            "commit_message",
            "encoding",
            "start_branch",
            "author_email",
            "author_name",
            "execute_filemode",
        ],
    );
    gl_post(
        host,
        &format!(
            "/projects/{}/repository/files/{}",
            enc(&project),
            enc(&file_path)
        ),
        &Value::Object(body),
    )
}

pub(crate) fn repo_file_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, file_path) = repo_file_target(&input)?;
    require_keys(&input, &["branch", "content", "commit_message"])?;
    let body = body_from(
        &input,
        &[
            "branch",
            "content",
            "commit_message",
            "encoding",
            "start_branch",
            "author_email",
            "author_name",
            "last_commit_id",
            "execute_filemode",
        ],
    );
    gl_put(
        host,
        &format!(
            "/projects/{}/repository/files/{}",
            enc(&project),
            enc(&file_path)
        ),
        &Value::Object(body),
    )
}

pub(crate) fn repo_file_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, file_path) = repo_file_target(&input)?;
    confirm_str(&input, "confirm_file_path", &file_path)?;
    require_keys(&input, &["branch", "commit_message"])?;
    let body = body_from(
        &input,
        &[
            "branch",
            "commit_message",
            "start_branch",
            "author_email",
            "author_name",
            "last_commit_id",
        ],
    );
    // The delete-file endpoint takes the commit params in the body.
    gl_request(
        host,
        "DELETE",
        &format!(
            "/projects/{}/repository/files/{}",
            enc(&project),
            enc(&file_path)
        ),
        Some(&Value::Object(body)),
    )?;
    Ok(json!({
        "project": project,
        "file_path": file_path,
        "branch": flex_str(&input, "branch"),
        "message": "repository file deleted"
    }))
}

pub(crate) fn repo_file_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let path = flex_str(&input, "path").ok_or("`path` (string) required")?;
    let git_ref = match flex_str(&input, "ref") {
        Some(r) => r,
        None => {
            // The files API needs an explicit ref — fall back to the project default branch.
            let project_obj = gl_get(host, &format!("/projects/{}", enc(&project)))?;
            project_obj
                .get("default_branch")
                .and_then(|v| v.as_str())
                .map(String::from)
                .ok_or("project has no default branch — pass ref explicitly")?
        }
    };
    let mut file = gl_get(
        host,
        &format!(
            "/projects/{}/repository/files/{}?ref={}",
            enc(&project),
            enc(&path),
            enc(&git_ref)
        ),
    )?;
    if let Some(max_bytes) = flex_i64(&input, &["max_bytes"]) {
        if max_bytes > 0 {
            let max = max_bytes as usize;
            let is_b64 = file.get("encoding").and_then(|v| v.as_str()) == Some("base64");
            let mut truncated = false;
            if let Some(Value::String(content)) = file.get_mut("content") {
                if is_b64 {
                    // GL-013: the cap applies to DECODED bytes and the prefix is re-encoded, so
                    // `content` stays valid base64 — truncating the base64 string itself would
                    // hand back an undecodable fragment.
                    use base64::Engine as _;
                    let engine = base64::engine::general_purpose::STANDARD;
                    let compact: String = content.split_whitespace().collect();
                    if let Ok(decoded) = engine.decode(compact) {
                        if decoded.len() > max {
                            *content = engine.encode(&decoded[..max]);
                            truncated = true;
                        }
                    }
                } else if content.len() > max {
                    let mut end = max;
                    while end > 0 && !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    *content = content[..end].to_string();
                    truncated = true;
                }
            }
            if truncated {
                file["truncated"] = json!(true);
            }
        }
    }
    // GL-006: convenience decoded text for UTF-8 files. GitLab returns file content base64-encoded;
    // agents and CLI users almost always want the text. Decode the (post-`max_bytes`) base64 into
    // `decoded_content` when it is valid UTF-8, leaving the raw `content`/`encoding` untouched for
    // existing consumers. Binary files (and a truncation that split a multi-byte char) simply omit
    // the field, so nothing breaks.
    let is_b64 = file.get("encoding").and_then(|v| v.as_str()) == Some("base64");
    if is_b64 {
        if let Some(content) = file.get("content").and_then(|v| v.as_str()) {
            use base64::Engine as _;
            let engine = base64::engine::general_purpose::STANDARD;
            let compact: String = content.split_whitespace().collect();
            if let Ok(decoded) = engine.decode(compact) {
                if let Ok(text) = String::from_utf8(decoded) {
                    file["decoded_content"] = json!(text);
                }
            }
        }
    }
    Ok(file)
}

pub(crate) fn repo_tree(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        200,
        2000,
    );
    let recursive = input
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("path", flex_str(&input, "path").unwrap_or_default()),
        ("ref", flex_str(&input, "ref").unwrap_or_default()),
        (
            "recursive",
            if recursive {
                "true".into()
            } else {
                String::new()
            },
        ),
    ];
    gl_get(
        host,
        &format!("/projects/{}/repository/tree{}", enc(&project), qs(&pairs)),
    )
}

/// (project, file_path) for the repository-file write ops.
pub(crate) fn repo_file_target(input: &Value) -> Result<(String, String), String> {
    let project = req_project(input)?;
    let file_path = flex_str(input, "file_path").ok_or("`file_path` (string) required")?;
    Ok((project, file_path))
}

pub(crate) fn require_keys(input: &Value, keys: &[&str]) -> Result<(), String> {
    for key in keys {
        if input.get(*key).map(|v| v.is_null()).unwrap_or(true) {
            return Err(format!("`{key}` required"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commits.
// ---------------------------------------------------------------------------

pub(crate) fn commit_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    require_keys(&input, &["branch", "commit_message"])?;
    let actions = input
        .get("actions")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`actions` (non-empty array) required")?;
    let mut body = body_from(
        &input,
        &[
            "branch",
            "commit_message",
            "start_branch",
            "start_sha",
            "start_project",
            "author_email",
            "author_name",
            "force",
        ],
    );
    body.insert("actions".into(), json!(actions));
    gl_post(
        host,
        &format!("/projects/{}/repository/commits", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn commit_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("ref_name", flex_str(&input, "ref").unwrap_or_default()),
        ("path", flex_str(&input, "file_path").unwrap_or_default()),
        ("author", flex_str(&input, "author").unwrap_or_default()),
        ("since", flex_str(&input, "since").unwrap_or_default()),
        ("until", flex_str(&input, "until").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!(
            "/projects/{}/repository/commits{}",
            enc(&project),
            qs(&pairs)
        ),
    )
}

// ---------------------------------------------------------------------------
// Tags.
// ---------------------------------------------------------------------------

pub(crate) fn tag_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag_name = flex_str(&input, "tag_name")
        .or_else(|| flex_str(&input, "name"))
        .ok_or("`tag_name` (string) required")?;
    let git_ref = flex_str(&input, "ref").ok_or("`ref` (string) required")?;
    let mut body = json!({ "tag_name": tag_name, "ref": git_ref });
    if let Some(msg) = flex_str(&input, "message") {
        body["message"] = json!(msg);
    }
    gl_post(
        host,
        &format!("/projects/{}/repository/tags", enc(&project)),
        &body,
    )
}

pub(crate) fn tag_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("search", flex_str(&input, "search").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!("/projects/{}/repository/tags{}", enc(&project), qs(&pairs)),
    )
}

pub(crate) fn tag_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = tag_name(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/repository/tags/{}", enc(&project), enc(&tag)),
    )
}

pub(crate) fn tag_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = tag_name(&input)?;
    confirm_str(&input, "confirm_tag_name", &tag)?;
    gl_delete(
        host,
        &format!("/projects/{}/repository/tags/{}", enc(&project), enc(&tag)),
    )?;
    Ok(json!({ "project": project, "tag_name": tag, "message": "tag deleted" }))
}

/// A tag name from `tag_name`/`tag`/`name` aliases (tag ops only — see [`release_tag`]).
pub(crate) fn tag_name(input: &Value) -> Result<String, String> {
    flex_str(input, "tag_name")
        .or_else(|| flex_str(input, "tag"))
        .or_else(|| flex_str(input, "name"))
        .ok_or_else(|| "`tag_name` (string) required".into())
}

/// The release tag from `tag_name`/`tag` — deliberately NOT `name`, which is the release/link
/// display-name field on the release ops (GL-028: the old `name` fallback could silently treat
/// a display name as the tag).
pub(crate) fn release_tag(input: &Value) -> Result<String, String> {
    flex_str(input, "tag_name")
        .or_else(|| flex_str(input, "tag"))
        .ok_or_else(|| "`tag_name` (string) required".into())
}

// ---------------------------------------------------------------------------
// Snippets.
// ---------------------------------------------------------------------------

pub(crate) fn snippet_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let title = flex_str(&input, "title").ok_or("`title` (string) required")?;
    let files = input
        .get("files")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`files` (non-empty array) required")?;
    let visibility = flex_str(&input, "visibility").unwrap_or_else(|| "private".into());
    let mut body = json!({ "title": title, "visibility": visibility, "files": files });
    if let Some(desc) = flex_str(&input, "description") {
        body["description"] = json!(desc);
    }
    gl_post(host, "/snippets", &body)
}

pub(crate) fn snippet_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = flex_i64(&input, &["snippet_id", "id"]).ok_or("`snippet_id` (integer) required")?;
    confirm_i64(&input, "confirm_snippet_id", id)?;
    gl_delete(host, &format!("/snippets/{id}"))?;
    Ok(json!({ "snippet_id": id, "message": "snippet deleted" }))
}

// ---------------------------------------------------------------------------
// Search.
// ---------------------------------------------------------------------------

pub(crate) fn search_blobs(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = flex_str(&input, "query").ok_or("`query` (string) required")?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let project = flex_str(&input, "project");
    let group = flex_str(&input, "group");
    let git_ref = flex_str(&input, "ref").unwrap_or_default();
    let page = page_qs(&input);
    let scope = format!("?scope=blobs&search={}&per_page={limit}{page}", enc(&query));
    let path = if let Some(p) = project {
        let r = if git_ref.is_empty() {
            String::new()
        } else {
            format!("&ref={}", enc(&git_ref))
        };
        format!("/projects/{}/search{scope}{r}", enc(&p))
    } else if let Some(g) = group {
        format!("/groups/{}/search{scope}", enc(&g))
    } else {
        format!("/search{scope}")
    };
    let mut matches = gl_get(host, &path)?;
    if let Some(max_data_bytes) = flex_i64(&input, &["max_data_bytes"]) {
        if max_data_bytes > 0 {
            if let Some(arr) = matches.as_array_mut() {
                let max = max_data_bytes as usize;
                for m in arr {
                    if let Some(Value::String(data)) = m.get_mut("data") {
                        if data.len() > max {
                            // The cap includes the marker (GL-035): the returned string never
                            // exceeds the requested max_data_bytes.
                            const MARKER: &str = "\n[snippet truncated]";
                            let budget = max.saturating_sub(MARKER.len());
                            let mut end = budget;
                            while end > 0 && !data.is_char_boundary(end) {
                                end -= 1;
                            }
                            *data = if end == 0 {
                                let mut bare = max.min(data.len());
                                while bare > 0 && !data.is_char_boundary(bare) {
                                    bare -= 1;
                                }
                                data[..bare].to_string()
                            } else {
                                format!("{}{MARKER}", &data[..end])
                            };
                            m["data_truncated"] = json!(true);
                        }
                    }
                }
            }
        }
    }
    Ok(matches)
}

// ---------------------------------------------------------------------------
// Review: changes / diff lines / compare / discussions.
// ---------------------------------------------------------------------------

pub(crate) fn mr_changes(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let max_files = clamp(flex_i64(&input, &["max_files"]).unwrap_or(0), 50, 200) as usize;
    let max_diff_bytes = clamp(
        flex_i64(&input, &["max_diff_bytes"]).unwrap_or(0),
        16384,
        262144,
    ) as usize;
    let file_filter = flex_str(&input, "file");
    let mut files = Vec::new();
    let mut files_truncated = false;
    // Paginate the diff list (unique `/diffs` substring, fetched before the MR detail) and apply
    // the `file` filter BEFORE the file cap (GL-042) — asking for a specific file can never
    // return empty just because it sits beyond the first page (GL-043).
    let mut page = 1;
    loop {
        let diffs = gl_get(
            host,
            &format!(
                "/projects/{}/merge_requests/{iid}/diffs?per_page=100&page={page}",
                enc(&project)
            ),
        )?;
        let arr = diffs.as_array().cloned().unwrap_or_default();
        let page_len = arr.len();
        for f in &arr {
            if let Some(ff) = &file_filter {
                let np = f.get("new_path").and_then(|v| v.as_str()).unwrap_or("");
                let op = f.get("old_path").and_then(|v| v.as_str()).unwrap_or("");
                if np != ff && op != ff {
                    continue;
                }
            }
            if files.len() >= max_files {
                // GL-044: the file-count cut has its own top-level flag, distinct from the
                // per-file `diff_truncated`.
                files_truncated = true;
                break;
            }
            let mut fc = f.clone();
            if let Some(d) = f.get("diff").and_then(|v| v.as_str()) {
                if let Some(capped) = cap_bytes(d, max_diff_bytes) {
                    fc["diff"] = json!(capped);
                    fc["diff_truncated"] = json!(true);
                }
            }
            files.push(fc);
        }
        let filter_satisfied = file_filter.is_some() && !files.is_empty();
        if files_truncated || filter_satisfied || page_len < 100 {
            break;
        }
        page += 1;
    }
    let detail = gl_get(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
    )?;
    let diff_refs = detail.get("diff_refs").cloned().unwrap_or(Value::Null);
    let count = files.len();
    Ok(json!({
        "project": project, "iid": iid, "diff_refs": diff_refs, "files": files,
        "count": count, "files_truncated": files_truncated
    }))
}

pub(crate) fn mr_diff_lines(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let file = flex_str(&input, "file").ok_or("`file` (string) required")?;
    let fd = fetch_file_diff(host, &project, iid, &file)?
        .ok_or_else(|| format!("file {file:?} is not part of this merge request"))?;
    let parsed = parse_unified_diff(fd.get("diff").and_then(|v| v.as_str()).unwrap_or(""));
    let limit = clamp(flex_i64(&input, &["limit"]).unwrap_or(0), 200, 2000) as usize;
    let mut lines = Vec::new();
    let mut truncated = false;
    // Anchor on a new-file `line`, or an old-file `old_line` (GL-047 — deleted/context lines);
    // `line` wins when both are set.
    let anchor = flex_i64(&input, &["line"])
        .map(|t| (t, false))
        .or_else(|| flex_i64(&input, &["old_line"]).map(|t| (t, true)));
    if let Some((target, on_old)) = anchor {
        let ctx = flex_i64(&input, &["context"]).unwrap_or(3).max(0) as usize;
        let pos = if on_old {
            parsed
                .iter()
                .position(|l| l.old_line == target && l.kind != "added")
        } else {
            parsed
                .iter()
                .position(|l| l.new_line == target && l.kind != "deleted")
        };
        match pos {
            Some(idx) => {
                let start = idx.saturating_sub(ctx);
                let end = (idx + ctx + 1).min(parsed.len());
                for (i, l) in parsed[start..end].iter().enumerate() {
                    let mut o = diff_line_json(l);
                    if start + i == idx {
                        o["target"] = json!(true);
                    }
                    lines.push(o);
                }
            }
            None => {
                let side = if on_old { "old-file" } else { "new-file" };
                return Ok(json!({
                    "project": project, "iid": iid, "file": file, "lines": [], "count": 0,
                    "hint": format!("{side} line {target} is not part of this file's diff")
                }));
            }
        }
    } else if let Some(search) = flex_str(&input, "search") {
        // Regex search over line content (matching the reference's `SearchLines`),
        // not a plain substring scan.
        let re = Regex::new(&search).map_err(|e| format!("search: {e}"))?;
        for l in &parsed {
            if re.is_match(&l.content) {
                if lines.len() >= limit {
                    truncated = true;
                    break;
                }
                lines.push(diff_line_json(l));
            }
        }
    } else {
        for l in &parsed {
            if lines.len() >= limit {
                truncated = true;
                break;
            }
            lines.push(diff_line_json(l));
        }
    }
    let count = lines.len();
    Ok(json!({
        "project": project, "iid": iid, "file": file,
        "old_path": fd.get("old_path"), "new_path": fd.get("new_path"),
        "lines": lines, "count": count, "truncated": truncated
    }))
}

pub(crate) fn compare(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let from = flex_str(&input, "from").ok_or("`from` (string) required")?;
    let to = flex_str(&input, "to").ok_or("`to` (string) required")?;
    let straight = input
        .get("straight")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_files = clamp(flex_i64(&input, &["max_files"]).unwrap_or(0), 50, 200) as usize;
    let max_diff_bytes = clamp(
        flex_i64(&input, &["max_diff_bytes"]).unwrap_or(0),
        16384,
        262144,
    ) as usize;
    let result = gl_get(
        host,
        &format!(
            "/projects/{}/repository/compare?from={}&to={}{}",
            enc(&project),
            enc(&from),
            enc(&to),
            if straight { "&straight=true" } else { "" }
        ),
    )?;
    let max_commits = clamp(flex_i64(&input, &["max_commits"]).unwrap_or(0), 50, 500) as usize;
    let commit_arr = result
        .get("commits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // GL-045: commits are capped with their own marker; `commit_count` stays the full total.
    let commit_count = commit_arr.len();
    let commits_truncated = commit_count > max_commits;
    let commits: Vec<Value> = commit_arr.into_iter().take(max_commits).collect();
    let mut files = Vec::new();
    let mut files_truncated = false;
    let mut any_diff_truncated = false;
    if let Some(arr) = result.get("diffs").and_then(|v| v.as_array()) {
        for f in arr {
            if files.len() >= max_files {
                files_truncated = true;
                break;
            }
            let mut fc = f.clone();
            if let Some(d) = f.get("diff").and_then(|v| v.as_str()) {
                if let Some(capped) = cap_bytes(d, max_diff_bytes) {
                    fc["diff"] = json!(capped);
                    fc["diff_truncated"] = json!(true);
                    any_diff_truncated = true;
                }
            }
            files.push(fc);
        }
    }
    let file_count = files.len();
    // GL-014: the top-level flag is true when ANYTHING was cut — dropped files, a capped
    // per-file diff, or capped commits — with per-cause flags alongside.
    Ok(json!({
        "project": project, "from": from, "to": to,
        "web_url": result.get("web_url"),
        "commits": commits, "commit_count": commit_count,
        "commits_truncated": commits_truncated,
        "files": files, "file_count": file_count,
        "files_truncated": files_truncated,
        "truncated": files_truncated || any_diff_truncated || commits_truncated
    }))
}

pub(crate) fn mr_discussion_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        50,
        200,
    );
    let page = page_qs(&input);
    gl_get(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions?per_page={limit}{page}",
            enc(&project)
        ),
    )
}

pub(crate) fn mr_note_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let body = flex_str(&input, "body").ok_or("`body` (string) required")?;
    gl_post(
        host,
        &format!("/projects/{}/merge_requests/{iid}/notes", enc(&project)),
        &json!({ "body": body }),
    )
}

pub(crate) fn mr_discussion_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let comment = flex_str(&input, "body").ok_or("`body` (string) required")?;
    let dry_run = input
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = flex_str(&input, "path");
    let new_line = flex_i64(&input, &["new_line"]);
    let old_line = flex_i64(&input, &["old_line"]);
    let positioned = path.is_some() || new_line.is_some() || old_line.is_some();

    let mut position = Value::Null;
    if positioned {
        let path = path.ok_or("`path` is required for a line-level comment")?;
        if new_line.is_none() && old_line.is_none() {
            return Err("`new_line` or `old_line` is required for a line-level comment".into());
        }
        let detail = gl_get(
            host,
            &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
        )?;
        let refs = detail.get("diff_refs").cloned().unwrap_or(Value::Null);
        let fd = fetch_file_diff(host, &project, iid, &path)?
            .ok_or_else(|| format!("file {path:?} is not part of this merge request"))?;
        let old_path = fd
            .get("old_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&path)
            .to_string();
        let new_path = fd
            .get("new_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&path)
            .to_string();
        // Derive the missing side for context lines so GitLab accepts the anchor.
        let parsed = parse_unified_diff(fd.get("diff").and_then(|v| v.as_str()).unwrap_or(""));
        let (mut nl, mut ol) = (new_line, old_line);
        if let (Some(n), None) = (new_line, old_line) {
            if let Some(l) = parsed
                .iter()
                .find(|l| l.new_line == n && l.kind == "context")
            {
                ol = Some(l.old_line);
            }
        } else if let (None, Some(o)) = (new_line, old_line) {
            if let Some(l) = parsed
                .iter()
                .find(|l| l.old_line == o && l.kind == "context")
            {
                nl = Some(l.new_line);
            }
        }
        let mut pos = Map::new();
        pos.insert("position_type".into(), json!("text"));
        pos.insert(
            "base_sha".into(),
            refs.get("base_sha").cloned().unwrap_or(Value::Null),
        );
        pos.insert(
            "start_sha".into(),
            refs.get("start_sha").cloned().unwrap_or(Value::Null),
        );
        pos.insert(
            "head_sha".into(),
            refs.get("head_sha").cloned().unwrap_or(Value::Null),
        );
        pos.insert("old_path".into(), json!(old_path));
        pos.insert("new_path".into(), json!(new_path));
        if let Some(n) = nl {
            pos.insert("new_line".into(), json!(n));
        }
        if let Some(o) = ol {
            pos.insert("old_line".into(), json!(o));
        }
        position = Value::Object(pos);
    }

    if dry_run {
        return Ok(json!({
            "project": project, "iid": iid, "posted": false, "dry_run": true, "position": position
        }));
    }

    let mut body = json!({ "body": comment });
    if !position.is_null() {
        body["position"] = position;
    }
    let discussion = gl_post(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions",
            enc(&project)
        ),
        &body,
    )?;
    Ok(json!({ "project": project, "iid": iid, "posted": true, "discussion": discussion }))
}

pub(crate) fn mr_discussion_reply(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let discussion_id =
        flex_str(&input, "discussion_id").ok_or("`discussion_id` (string) required")?;
    let body = flex_str(&input, "body").ok_or("`body` (string) required")?;
    gl_post(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions/{}/notes",
            enc(&project),
            enc(&discussion_id)
        ),
        &json!({ "body": body }),
    )
}

pub(crate) fn mr_discussion_resolve(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let discussion_id =
        flex_str(&input, "discussion_id").ok_or("`discussion_id` (string) required")?;
    let resolved = input
        .get("resolved")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    gl_put(
        host,
        &format!(
            "/projects/{}/merge_requests/{iid}/discussions/{}",
            enc(&project),
            enc(&discussion_id)
        ),
        &json!({ "resolved": resolved }),
    )
}

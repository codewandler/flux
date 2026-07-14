//! Projects, merge requests, issues, authentication, and indexing operations.

use super::*;

// ---------------------------------------------------------------------------
// Reads: projects / merge requests / issues / pipelines (the original surface).
// ---------------------------------------------------------------------------

pub(crate) fn project_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let membership = input
        .get("membership")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let search = flex_str(&input, "search")
        .or_else(|| flex_str(&input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(&input, "order_by").unwrap_or_else(|| "last_activity_at".into());
    let sort = flex_str(&input, "sort").unwrap_or_else(|| "desc".into());
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let pairs = [
        (
            "membership",
            if membership {
                "true".into()
            } else {
                "false".into()
            },
        ),
        ("search", search),
        ("order_by", order_by),
        ("sort", sort),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
    ];
    let projects = gl_get(host, &format!("/projects{}", qs(&pairs)))?;
    if wants_contribution(&input) {
        contribute_projects(host, &projects);
    }
    Ok(projects)
}

pub(crate) fn project_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(host, &format!("/projects/{}", enc(&project)))
}

pub(crate) fn mr_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let state = flex_str(&input, "state").unwrap_or_else(|| "opened".into());
    let search = flex_str(&input, "search")
        .or_else(|| flex_str(&input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(&input, "order_by").unwrap_or_else(|| "updated_at".into());
    let sort = flex_str(&input, "sort").unwrap_or_else(|| "desc".into());
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let source_branch = flex_str(&input, "source_branch").unwrap_or_default();
    let target_branch = flex_str(&input, "target_branch").unwrap_or_default();
    let pairs = [
        ("state", state),
        ("search", search),
        ("order_by", order_by),
        ("sort", sort),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("source_branch", source_branch),
        ("target_branch", target_branch),
    ];
    let mrs = gl_get(
        host,
        &format!("/projects/{}/merge_requests{}", enc(&project), qs(&pairs)),
    )?;
    if wants_contribution(&input) {
        contribute_list(host, &mrs, "gitlab.merge_request", &project);
    }
    Ok(mrs)
}

pub(crate) fn mr_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
    )
}

pub(crate) fn issue_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let state = flex_str(&input, "state").unwrap_or_else(|| "opened".into());
    let search = flex_str(&input, "search")
        .or_else(|| flex_str(&input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(&input, "order_by").unwrap_or_default();
    let sort = flex_str(&input, "sort").unwrap_or_default();
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let pairs = [
        ("state", state),
        ("search", search),
        ("order_by", order_by),
        ("sort", sort),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
    ];
    let issues = gl_get(
        host,
        &format!("/projects/{}/issues{}", enc(&project), qs(&pairs)),
    )?;
    if wants_contribution(&input) {
        contribute_list(host, &issues, "gitlab.issue", &project);
    }
    Ok(issues)
}

pub(crate) fn pipeline_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let status = flex_str(&input, "status").unwrap_or_default();
    let git_ref = flex_str(&input, "ref").unwrap_or_default();
    let source = flex_str(&input, "source").unwrap_or_default();
    let username = flex_str(&input, "username").unwrap_or_default();
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let pairs = [
        ("status", status),
        ("ref", git_ref),
        ("source", source),
        ("username", username),
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
    ];
    gl_get(
        host,
        &format!("/projects/{}/pipelines{}", enc(&project), qs(&pairs)),
    )
}

// ---------------------------------------------------------------------------
// Auth test + index build.
// ---------------------------------------------------------------------------

pub(crate) fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let user = gl_get(host, "/user")?;
    // GL-016: an auth smoke check needs only enough identity to confirm *which* account the token
    // authenticates as — id/username/name. The full `GET /user` (~50 keys: email, public/commit
    // email, two-factor status, last-sign-in timestamps, …) is sensitive and must never be echoed
    // for a health check, so pin the result to a minimal, documented identity subset.
    let pick = |key: &str| user.get(key).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "status": "ok",
        "text": "GitLab auth OK",
        "user": {
            "id": pick("id"),
            "username": pick("username"),
            "name": pick("name"),
        },
    }))
}

/// Which datasource categories the current `index.build` call should populate.
#[derive(Default)]
pub(crate) struct IndexInclude {
    projects: bool,
    merge_requests: bool,
    issues: bool,
}

pub(crate) fn index_include(input: &Value) -> Result<IndexInclude, String> {
    let mut raw = Vec::new();
    for key in ["index", "indexes", "entity", "entities"] {
        match input.get(key) {
            Some(Value::String(s)) => {
                for part in s.split(',') {
                    raw.push(part.trim().to_lowercase());
                }
            }
            Some(Value::Array(arr)) => {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        raw.push(s.trim().to_lowercase());
                    }
                }
            }
            _ => {}
        }
    }
    let raw: Vec<String> = raw.into_iter().filter(|s| !s.is_empty()).collect();
    if raw.is_empty() {
        return Ok(IndexInclude {
            projects: true,
            merge_requests: true,
            issues: true,
        });
    }
    let mut inc = IndexInclude::default();
    let mut unknown = Vec::new();
    for v in raw {
        match v.as_str() {
            "projects" | "project" | "gitlab.projects" | "gitlab.project" => inc.projects = true,
            "merge_requests"
            | "merge_request"
            | "mr"
            | "mrs"
            | "gitlab.merge_requests"
            | "gitlab.merge_request" => inc.merge_requests = true,
            "issues" | "issue" | "gitlab.issues" | "gitlab.issue" => inc.issues = true,
            other => unknown.push(other.to_string()),
        }
    }
    // GL-034: a selector typo must be an error, not an empty `indexed: 0` success.
    if !unknown.is_empty() {
        return Err(format!(
            "unknown index selector(s): {} (known: projects, merge_requests/mrs, issues)",
            unknown.join(", ")
        ));
    }
    Ok(inc)
}

/// Resolve a 1-based `limit` into `(all_pages, per_page)` for index paging.
/// A positive limit yields a single page of up to `max_per_page` items; otherwise all pages are fetched with `per_page`.
pub(crate) fn page_plan(input: &Value, limit_key: &str, max_per_page: i64) -> (bool, i64) {
    match flex_i64(input, &[limit_key]) {
        Some(v) if v > 0 => (false, clamp(v, 1, max_per_page)),
        _ => (true, max_per_page),
    }
}

/// Drive datasource contribution over the requested selectors. Each category pages via
/// `per_page`/`page` unless a datasource-specific limit pins it to a single page.
pub(crate) fn index_build(input: Value, host: &mut Host) -> Result<Value, String> {
    let include = index_include(&input)?;
    // GL-017: a dry-run scope estimate — describe the breadth WITHOUT crawling or contributing,
    // so a no-argument `index.build` is never a silent instance-wide sweep.
    if input
        .get("estimate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(index_estimate(&input, &include));
    }
    let mut total = 0;
    if include.projects {
        total += index_projects(host, &input);
    }
    if include.merge_requests {
        total += index_merge_requests(host, &input);
    }
    if include.issues {
        total += index_issues(host, &input);
    }
    Ok(json!({ "indexed": total }))
}

/// GL-017: describe the crawl `index.build` is about to run — which datasources, and each one's
/// scope (a named project vs the whole instance) — without any HTTP or contribution. The operator
/// runs this first, sees the breadth, then reruns without `estimate` to actually index.
pub(crate) fn index_estimate(input: &Value, include: &IndexInclude) -> Value {
    let mut would_crawl = Vec::new();
    let mut scopes = Map::new();
    let mut instance_wide = false;
    if include.projects {
        would_crawl.push(json!("projects"));
        let membership = input
            .get("membership")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        // Projects are always instance-scoped (there is no project selector to narrow them).
        instance_wide = true;
        scopes.insert(
            "projects".into(),
            json!(if membership {
                "instance-wide (projects you are a member of)"
            } else {
                "instance-wide (every visible project)"
            }),
        );
    }
    if include.merge_requests {
        would_crawl.push(json!("merge_requests"));
        let project = flex_str(input, "mr_project")
            .or_else(|| flex_str(input, "project"))
            .or_else(|| flex_str(input, "project_id"))
            .or_else(|| flex_str(input, "path"));
        match project {
            Some(p) => {
                scopes.insert("merge_requests".into(), json!(format!("project {p}")));
            }
            None => {
                instance_wide = true;
                scopes.insert(
                    "merge_requests".into(),
                    json!("instance-wide (every visible merge request)"),
                );
            }
        }
    }
    if include.issues {
        would_crawl.push(json!("issues"));
        let project = flex_str(input, "issue_project")
            .or_else(|| flex_str(input, "project"))
            .or_else(|| flex_str(input, "project_id"))
            .or_else(|| flex_str(input, "path"));
        match project {
            Some(p) => {
                scopes.insert("issues".into(), json!(format!("project {p}")));
            }
            None => {
                instance_wide = true;
                scopes.insert(
                    "issues".into(),
                    json!("instance-wide (every visible issue)"),
                );
            }
        }
    }
    let note = if instance_wide {
        "This crawls instance-wide datasources — potentially every visible project/MR/issue. Scope it with a project (project/mr_project/issue_project) or narrow with index/entities, then rerun without estimate to index."
    } else {
        "Rerun without estimate to index the scoped datasources above."
    };
    json!({
        "estimate": true,
        "would_crawl": would_crawl,
        "scopes": Value::Object(scopes),
        "instance_wide": instance_wide,
        "note": note,
    })
}

pub(crate) fn index_projects(host: &mut Host, input: &Value) -> usize {
    let membership = input
        .get("membership")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let search = flex_str(input, "search")
        .or_else(|| flex_str(input, "query"))
        .unwrap_or_default();
    let order_by = flex_str(input, "order_by").unwrap_or_else(|| "last_activity_at".into());
    let sort = flex_str(input, "sort").unwrap_or_else(|| "desc".into());
    let (all_pages, per_page) = page_plan(input, "limit", 100);
    let mut pairs = vec![(
        "membership",
        if membership {
            "true".into()
        } else {
            "false".into()
        },
    )];
    if !search.is_empty() {
        pairs.push(("search", search));
    }
    pairs.push(("order_by", order_by));
    pairs.push(("sort", sort));
    let base = format!("/projects{}", qs(&pairs));
    page_index(host, &base, per_page, all_pages, contribute_projects)
}

pub(crate) fn index_merge_requests(host: &mut Host, input: &Value) -> usize {
    let project = flex_str(input, "mr_project")
        .or_else(|| flex_str(input, "project"))
        .or_else(|| flex_str(input, "project_id"))
        .or_else(|| flex_str(input, "path"));
    let state = flex_str(input, "mr_state").unwrap_or_else(|| "all".into());
    let search = flex_str(input, "mr_search").unwrap_or_default();
    let order_by = flex_str(input, "mr_order_by").unwrap_or_else(|| "updated_at".into());
    let sort = flex_str(input, "mr_sort").unwrap_or_else(|| "desc".into());
    let (all_pages, per_page) = page_plan(input, "mr_limit", 100);
    let mut pairs = vec![("scope", "all".into())];
    if !state.is_empty() {
        pairs.push(("state", state));
    }
    if !search.is_empty() {
        pairs.push(("search", search));
    }
    pairs.push(("order_by", order_by));
    pairs.push(("sort", sort));
    let base = if let Some(project) = project {
        format!("/projects/{}/merge_requests{}", enc(&project), qs(&pairs))
    } else {
        format!("/merge_requests{}", qs(&pairs))
    };
    page_index(host, &base, per_page, all_pages, |h, page| {
        contribute_refs(h, page, "gitlab.merge_request")
    })
}

pub(crate) fn index_issues(host: &mut Host, input: &Value) -> usize {
    // GL-040: honor a project scope for issues, matching MR indexing — `issue_project` (or the
    // shared `project`). Without one, issues are crawled instance-wide.
    let project = flex_str(input, "issue_project")
        .or_else(|| flex_str(input, "project"))
        .or_else(|| flex_str(input, "project_id"))
        .or_else(|| flex_str(input, "path"));
    let state = flex_str(input, "issue_state").unwrap_or_else(|| "all".into());
    let search = flex_str(input, "issue_search").unwrap_or_default();
    let order_by = flex_str(input, "issue_order_by").unwrap_or_else(|| "updated_at".into());
    let sort = flex_str(input, "issue_sort").unwrap_or_else(|| "desc".into());
    let (all_pages, per_page) = page_plan(input, "issue_limit", 100);
    let mut pairs = vec![("scope", "all".into())];
    if !state.is_empty() {
        pairs.push(("state", state));
    }
    if !search.is_empty() {
        pairs.push(("search", search));
    }
    pairs.push(("order_by", order_by));
    pairs.push(("sort", sort));
    let base = if let Some(project) = project {
        format!("/projects/{}/issues{}", enc(&project), qs(&pairs))
    } else {
        format!("/issues{}", qs(&pairs))
    };
    page_index(host, &base, per_page, all_pages, |h, page| {
        contribute_refs(h, page, "gitlab.issue")
    })
}

/// Page `base_path` until exhausted (or a single page when `all_pages` is false),
/// contributing each page and returning the number of records indexed.
pub(crate) fn page_index(
    host: &mut Host,
    base_path: &str,
    per_page: i64,
    all_pages: bool,
    contribute: impl Fn(&mut Host, &Value) -> usize,
) -> usize {
    let mut total = 0;
    let mut page = 1;
    loop {
        let sep = if base_path.contains('?') { "&" } else { "?" };
        let path = format!("{base_path}{sep}per_page={per_page}&page={page}");
        let items = match gl_get(host, &path) {
            Ok(v) => v,
            Err(_) => break,
        };
        let len = items.as_array().map(|a| a.len()).unwrap_or(0);
        if len == 0 {
            break;
        }
        total += contribute(host, &items);
        if !all_pages || len < per_page as usize {
            break;
        }
        page += 1;
    }
    total
}

// ---------------------------------------------------------------------------
// Project / merge request / issue writes.
// ---------------------------------------------------------------------------

pub(crate) fn project_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    let mut body = body_from(
        &input,
        &[
            "path",
            "description",
            "visibility",
            "initialize_with_readme",
        ],
    );
    body.insert("name".into(), json!(name));
    // Resolve a group namespace path → namespace_id (GL-026/GL-046).
    if let Some(namespace) = flex_str(&input, "namespace") {
        let id = resolve_namespace_id(host, &namespace)?;
        body.insert("namespace_id".into(), id);
    }
    gl_post(host, "/projects", &Value::Object(body))
}

/// Resolve a group `namespace` to its numeric id for `project.create` (GL-026/GL-046).
///
/// Robust against the two beta findings: it **paginates** the `/groups` search beyond the first
/// page (the old code capped at `per_page=20`, so a group past the first 20 hits was invisible),
/// and it resolves **unambiguously**. An exact `full_path` match wins deterministically; otherwise
/// a bare basename (`path`) match is used only when it is unique — a basename shared by several
/// nested groups is an error asking for the full path, never a silent first-wins pick.
pub(crate) fn resolve_namespace_id(host: &mut Host, namespace: &str) -> Result<Value, String> {
    let mut exact_full: Option<Value> = None;
    let mut basename: Vec<(String, Value)> = Vec::new();
    let mut page = 1;
    loop {
        let groups = gl_get(
            host,
            &format!("/groups?search={}&per_page=100&page={page}", enc(namespace)),
        )?;
        let arr = match groups.as_array() {
            Some(a) if !a.is_empty() => a.clone(),
            _ => break,
        };
        let len = arr.len();
        for g in &arr {
            let full = g.get("full_path").and_then(|v| v.as_str()).unwrap_or("");
            let path = g.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let Some(id) = g.get("id").filter(|v| !v.is_null()).cloned() else {
                continue;
            };
            if full.eq_ignore_ascii_case(namespace) {
                // First exact full_path match wins deterministically.
                exact_full.get_or_insert(id);
            } else if path.eq_ignore_ascii_case(namespace)
                && !basename.iter().any(|(f, _)| f.eq_ignore_ascii_case(full))
            {
                basename.push((full.to_string(), id));
            }
        }
        if len < 100 {
            break;
        }
        page += 1;
        if page > 50 {
            break; // safety cap: never loop unboundedly on a pathological search
        }
    }
    if let Some(id) = exact_full {
        return Ok(id);
    }
    match basename.len() {
        0 => Err(format!("group {namespace:?} not found")),
        1 => Ok(basename.into_iter().next().unwrap().1),
        _ => {
            let names: Vec<String> = basename.into_iter().map(|(f, _)| f).collect();
            Err(format!(
                "namespace {namespace:?} is ambiguous — it matches multiple groups: {}. Pass the full group path.",
                names.join(", ")
            ))
        }
    }
}

pub(crate) fn project_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    // GL-005: fat-finger guards — a supplied confirm must match, an absent one stays ergonomic.
    confirm_str(&input, "confirm_path", &project)?;
    if flex_i64(&input, &["confirm_project_id"]).is_some() {
        let id = resolve_project_id(host, &project)?;
        confirm_i64(&input, "confirm_project_id", id)?;
    }
    gl_delete(host, &format!("/projects/{}", enc(&project)))?;
    Ok(json!({ "project": project, "message": "project deleted" }))
}

pub(crate) fn mr_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    for key in ["title", "source_branch", "target_branch"] {
        if flex_str(&input, key).is_none() {
            return Err(format!("`{key}` (string) required"));
        }
    }
    let body = body_from(
        &input,
        &[
            "title",
            "source_branch",
            "target_branch",
            "description",
            "labels",
            "assignee_id",
            "assignee_ids",
            "reviewer_ids",
            "target_project_id",
            "milestone_id",
            "remove_source_branch",
            "squash",
            "allow_collaboration",
        ],
    );
    gl_post(
        host,
        &format!("/projects/{}/merge_requests", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn mr_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let body = body_from(
        &input,
        &[
            "title",
            "description",
            "target_branch",
            "state_event",
            "labels",
        ],
    );
    if body.is_empty() {
        return Err(
            "nothing to update: pass title, description, target_branch, state_event, or labels"
                .into(),
        );
    }
    gl_put(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn mr_approve(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let body = body_from(&input, &["sha"]);
    gl_post(
        host,
        &format!("/projects/{}/merge_requests/{iid}/approve", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn mr_merge(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = mr_address(&input)?;
    let mut body = body_from(
        &input,
        &[
            "merge_commit_message",
            "squash_commit_message",
            "squash",
            "should_remove_source_branch",
            "sha",
        ],
    );
    if body.get("should_remove_source_branch").is_none() {
        if let Some(v) = input.get("remove_source_branch") {
            if !v.is_null() {
                body.insert("should_remove_source_branch".into(), v.clone());
            }
        }
    }
    // GitLab's modern accept-MR parameter is `auto_merge` (the older
    // `merge_when_pipeline_succeeds` is deprecated), matching the reference.
    if let Some(v) = input.get("auto_merge") {
        if !v.is_null() {
            body.insert("auto_merge".into(), v.clone());
        }
    }
    gl_put(
        host,
        &format!("/projects/{}/merge_requests/{iid}/merge", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn issue_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    gl_get(host, &format!("/projects/{}/issues/{iid}", enc(&project)))
}

pub(crate) fn issue_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    if flex_str(&input, "title").is_none() {
        return Err("`title` (string) required".into());
    }
    let body = body_from(
        &input,
        &[
            "title",
            "description",
            "labels",
            "assignee_ids",
            "milestone_id",
            "confidential",
        ],
    );
    gl_post(
        host,
        &format!("/projects/{}/issues", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn issue_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    let body = body_from(
        &input,
        &[
            "title",
            "description",
            "labels",
            "add_labels",
            "remove_labels",
            "state_event",
            "assignee_ids",
        ],
    );
    gl_put(
        host,
        &format!("/projects/{}/issues/{iid}", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn issue_note_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        100,
    );
    let pairs = [
        ("per_page", limit.to_string()),
        (
            "page",
            flex_i64(&input, &["page"])
                .map(|p| p.to_string())
                .unwrap_or_default(),
        ),
        ("sort", flex_str(&input, "sort").unwrap_or_default()),
        ("order_by", flex_str(&input, "order_by").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!(
            "/projects/{}/issues/{iid}/notes{}",
            enc(&project),
            qs(&pairs)
        ),
    )
}

pub(crate) fn issue_note_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let (project, iid) = issue_address(&input)?;
    let body = flex_str(&input, "body").ok_or("`body` (string) required")?;
    gl_post(
        host,
        &format!("/projects/{}/issues/{iid}/notes", enc(&project)),
        &json!({ "body": body }),
    )
}

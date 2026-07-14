//! CI/CD, job-token governance, protected-tag, and deploy-token operations.

use super::*;

// ---------------------------------------------------------------------------
// CI/CD: variables / pipelines / jobs / environments / deployments.
// ---------------------------------------------------------------------------

pub(crate) fn ci_variable_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    require_keys(&input, &["key", "value"])?;
    let body = body_from(
        &input,
        &[
            "key",
            "value",
            "description",
            "environment_scope",
            "masked",
            "masked_and_hidden",
            "protected",
            "raw",
            "variable_type",
        ],
    );
    gl_post(
        host,
        &format!("/projects/{}/variables", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn ci_variable_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let key = flex_str(&input, "key").ok_or("`key` (string) required")?;
    require_keys(&input, &["value"])?;
    let body = body_from(
        &input,
        &[
            "value",
            "description",
            "environment_scope",
            "masked",
            "protected",
            "raw",
            "variable_type",
        ],
    );
    gl_put(
        host,
        &format!(
            "/projects/{}/variables/{}{}",
            enc(&project),
            enc(&key),
            env_scope_filter(&input)
        ),
        &Value::Object(body),
    )
}

pub(crate) fn ci_variable_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let key = flex_str(&input, "key").ok_or("`key` (string) required")?;
    confirm_str(&input, "confirm_key", &key)?;
    gl_delete(
        host,
        &format!(
            "/projects/{}/variables/{}{}",
            enc(&project),
            enc(&key),
            env_scope_filter(&input)
        ),
    )?;
    Ok(json!({ "project": project, "key": key, "message": "ci variable deleted" }))
}

/// `?filter[environment_scope]=<scope>` when an environment_scope is supplied, else "".
pub(crate) fn env_scope_filter(input: &Value) -> String {
    match flex_str(input, "environment_scope") {
        Some(scope) => format!("?filter[environment_scope]={}", enc(&scope)),
        None => String::new(),
    }
}

pub(crate) fn pipeline_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let git_ref = flex_str(&input, "ref").ok_or("`ref` (string) required")?;
    let mut body = json!({ "ref": git_ref });
    if let Some(vars) = input.get("variables").and_then(|v| v.as_array()) {
        let variables = validate_pipeline_variables(vars)?;
        body["variables"] = json!(variables);
    }
    gl_post(
        host,
        &format!("/projects/{}/pipeline", enc(&project)),
        &body,
    )
}

/// Validate and normalize pipeline `variables` (matching the reference): each entry needs a
/// non-empty `key`, and `variable_type` must be one of `env_var`/`file` when given; the forwarded
/// object carries `key`/`value`/`variable_type`.
pub(crate) fn validate_pipeline_variables(vars: &[Value]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(vars.len());
    for (i, v) in vars.iter().enumerate() {
        let key = flex_str(v, "key").ok_or_else(|| format!("variables[{i}]: key is required"))?;
        let variable_type = match flex_str(v, "variable_type") {
            Some(t) if t == "env_var" || t == "file" => Some(t),
            Some(t) => return Err(format!("variables[{i}]: invalid variable_type {t:?}")),
            None => None,
        };
        let mut entry = Map::new();
        entry.insert("key".into(), json!(key));
        entry.insert(
            "value".into(),
            v.get("value").cloned().unwrap_or(Value::Null),
        );
        if let Some(t) = variable_type {
            entry.insert("variable_type".into(), json!(t));
        }
        out.push(Value::Object(entry));
    }
    Ok(out)
}

pub(crate) fn pipeline_retry(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let id = flex_i64(&input, &["pipeline_id"]).ok_or("`pipeline_id` (integer) required")?;
    gl_post(
        host,
        &format!("/projects/{}/pipelines/{id}/retry", enc(&project)),
        &json!({}),
    )
}

pub(crate) fn pipeline_cancel(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let id = flex_i64(&input, &["pipeline_id"]).ok_or("`pipeline_id` (integer) required")?;
    gl_post(
        host,
        &format!("/projects/{}/pipelines/{id}/cancel", enc(&project)),
        &json!({}),
    )
}

pub(crate) fn job_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let id = flex_i64(&input, &["pipeline_id"]).ok_or("`pipeline_id` (integer) required")?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        50,
        200,
    );
    let page = page_qs(&input);
    let mut path = format!(
        "/projects/{}/pipelines/{id}/jobs?per_page={limit}{page}",
        enc(&project)
    );
    if let Some(scopes) = input.get("scope").and_then(|v| v.as_array()) {
        for s in scopes {
            if let Some(st) = s.as_str() {
                path.push_str(&format!("&scope[]={}", enc(st)));
            }
        }
    }
    gl_get(host, &path)
}

pub(crate) fn environment_list(input: Value, host: &mut Host) -> Result<Value, String> {
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
        ("states", flex_str(&input, "states").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!("/projects/{}/environments{}", enc(&project), qs(&pairs)),
    )
}

pub(crate) fn deployment_list(input: Value, host: &mut Host) -> Result<Value, String> {
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
        ("order_by", "created_at".to_string()),
        ("sort", "desc".to_string()),
        (
            "environment",
            flex_str(&input, "environment").unwrap_or_default(),
        ),
        ("status", flex_str(&input, "status").unwrap_or_default()),
    ];
    gl_get(
        host,
        &format!("/projects/{}/deployments{}", enc(&project), qs(&pairs)),
    )
}

pub(crate) fn ci_job_token_scope_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/job_token_scope", enc(&project)),
    )
}

pub(crate) fn ci_job_token_scope_set(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let enabled = input
        .get("enabled")
        .and_then(|v| v.as_bool())
        .ok_or("`enabled` (boolean) required")?;
    // GitLab replies 204 No Content to this PATCH, so synthesize the confirmation.
    gl_request(
        host,
        "PATCH",
        &format!("/projects/{}/job_token_scope", enc(&project)),
        Some(&json!({ "enabled": enabled })),
    )?;
    Ok(json!({ "project": project, "enabled": enabled, "message": "job token scope updated" }))
}

pub(crate) fn ci_job_token_allowlist_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/job_token_scope/allowlist", enc(&project)),
    )
}

pub(crate) fn ci_job_token_allowlist_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_project_id"]).ok_or("`target_project_id` (integer) required")?;
    let project_id = resolve_project_id(host, &project)?;
    gl_post(
        host,
        &format!("/projects/{project_id}/job_token_scope/allowlist"),
        &json!({ "target_project_id": target }),
    )
}

pub(crate) fn ci_job_token_allowlist_remove(
    input: Value,
    host: &mut Host,
) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_project_id"]).ok_or("`target_project_id` (integer) required")?;
    confirm_i64(&input, "confirm_target_project_id", target)?;
    let project_id = resolve_project_id(host, &project)?;
    gl_delete(
        host,
        &format!("/projects/{project_id}/job_token_scope/allowlist/{target}"),
    )?;
    Ok(json!({
        "project": project,
        "target_project_id": target,
        "message": "removed from job token allowlist"
    }))
}

pub(crate) fn ci_job_token_groups_allowlist_list(
    input: Value,
    host: &mut Host,
) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(
        host,
        &format!(
            "/projects/{}/job_token_scope/groups_allowlist",
            enc(&project)
        ),
    )
}

pub(crate) fn ci_job_token_groups_allowlist_add(
    input: Value,
    host: &mut Host,
) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_group_id"]).ok_or("`target_group_id` (integer) required")?;
    let project_id = resolve_project_id(host, &project)?;
    gl_post(
        host,
        &format!("/projects/{project_id}/job_token_scope/groups_allowlist"),
        &json!({ "target_group_id": target }),
    )
}

pub(crate) fn ci_job_token_groups_allowlist_remove(
    input: Value,
    host: &mut Host,
) -> Result<Value, String> {
    let project = req_project(&input)?;
    let target =
        flex_i64(&input, &["target_group_id"]).ok_or("`target_group_id` (integer) required")?;
    confirm_i64(&input, "confirm_target_group_id", target)?;
    let project_id = resolve_project_id(host, &project)?;
    gl_delete(
        host,
        &format!("/projects/{project_id}/job_token_scope/groups_allowlist/{target}"),
    )?;
    Ok(json!({
        "project": project,
        "target_group_id": target,
        "message": "removed from job token groups allowlist"
    }))
}

pub(crate) fn protected_tag_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(host, &format!("/projects/{}/protected_tags", enc(&project)))
}

pub(crate) fn protected_tag_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    gl_get(
        host,
        &format!("/projects/{}/protected_tags/{}", enc(&project), enc(&name)),
    )
}

pub(crate) fn protected_tag_protect(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    let create_access_level = flex_i64(&input, &["create_access_level"]).unwrap_or(40);
    gl_post(
        host,
        &format!("/projects/{}/protected_tags", enc(&project)),
        &json!({ "name": name, "create_access_level": create_access_level }),
    )
}

pub(crate) fn protected_tag_unprotect(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    confirm_str(&input, "confirm_name", &name)?;
    gl_delete(
        host,
        &format!("/projects/{}/protected_tags/{}", enc(&project), enc(&name)),
    )?;
    Ok(json!({ "project": project, "name": name, "message": "tag unprotected" }))
}

pub(crate) fn deploy_token_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    gl_get(host, &format!("/projects/{}/deploy_tokens", enc(&project)))
}

pub(crate) fn deploy_token_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let name = flex_str(&input, "name").ok_or("`name` (string) required")?;
    let scopes = input
        .get("scopes")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`scopes` (non-empty array, e.g. [\"read_repository\"]) required")?;
    let mut body = body_from(&input, &["expires_at", "username"]);
    body.insert("name".into(), json!(name));
    body.insert("scopes".into(), json!(scopes));
    gl_post(
        host,
        &format!("/projects/{}/deploy_tokens", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn deploy_token_revoke(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let token_id = flex_i64(&input, &["token_id", "id"]).ok_or("`token_id` (integer) required")?;
    confirm_i64(&input, "confirm_token_id", token_id)?;
    gl_delete(
        host,
        &format!("/projects/{}/deploy_tokens/{token_id}", enc(&project)),
    )?;
    Ok(json!({ "project": project, "token_id": token_id, "message": "deploy token revoked" }))
}

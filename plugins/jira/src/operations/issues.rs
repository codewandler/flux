//! Authentication, indexing, issue lifecycle, search, and metadata operations.

use super::*;

// ---------------------------------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------------------------------

pub(crate) fn auth_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let user = jget(host, "/myself")?;
    Ok(json!({"text": "Jira auth OK", "status": "ok", "user": user}))
}

pub(crate) fn index_build(input: Value, host: &mut Host) -> Result<Value, String> {
    let issue_selector = json!({
        "jql": opt_str(&input, "issue_jql"),
        "project": opt_str(&input, "project"),
        "status": opt_str(&input, "status"),
        "query": opt_str(&input, "issue_query"),
    });
    let jql = build_jql(&issue_selector);
    let issue_limit = clamp_limit(&input, &["issue_limit"], 100, 100);
    let issues = jget(
        host,
        &format!(
            "/search/jql?jql={}&maxResults={issue_limit}&fields={}",
            urlencode(&jql),
            urlencode(FIELDS)
        ),
    )?;
    let n_issues = contribute_issues(host, &issues);

    let user_query = opt_str(&input, "user_query").trim();
    let user_limit = clamp_limit(&input, &["user_limit"], 100, 100);
    let users = jget(
        host,
        &format!(
            "/user/search?query={}&maxResults={user_limit}&startAt=0",
            urlencode(user_query)
        ),
    )?;
    let n_users = contribute_users(host, &users);

    Ok(json!({"indexed": n_issues + n_users}))
}

pub(crate) fn issue_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = {
        let p = opt_str(&input, "project_key").trim();
        if p.is_empty() {
            opt_str(&input, "project").trim()
        } else {
            p
        }
    };
    let issue_type = opt_str(&input, "issue_type").trim();
    let summary = opt_str(&input, "summary").trim();
    if project.is_empty() || issue_type.is_empty() || summary.is_empty() {
        return Err("project_key (or project), issue_type, and summary are required".into());
    }
    let mut fields = raw_obj(&input, "fields").unwrap_or_default();
    fields.insert("project".into(), json!({"key": project}));
    fields.insert("issuetype".into(), json!({"name": issue_type}));
    fields.insert("summary".into(), json!(summary));
    apply_common(&mut fields, &input);
    let reporter = opt_str(&input, "reporter_account_id").trim();
    if !reporter.is_empty() {
        fields.insert("reporter".into(), json!({"accountId": reporter}));
    }
    let parent = opt_str(&input, "parent_key").trim();
    if !parent.is_empty() {
        fields.insert("parent".into(), json!({"key": parent}));
    }
    let update = raw_obj(&input, "update").unwrap_or_default();
    let mut body = Map::new();
    body.insert("fields".into(), Value::Object(fields));
    if !update.is_empty() {
        body.insert("update".into(), Value::Object(update));
    }
    let resp = jsend(host, "POST", "/issue", &Value::Object(body))?;
    Ok(json!({
        "ok": true,
        "id": resp.get("id").cloned().unwrap_or(Value::Null),
        "key": resp.get("key").cloned().unwrap_or(Value::Null),
        "self": resp.get("self").cloned().unwrap_or(Value::Null),
    }))
}

pub(crate) fn issue_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let mut fields = raw_obj(&input, "fields").unwrap_or_default();
    let summary = opt_str(&input, "summary").trim();
    if !summary.is_empty() {
        fields.insert("summary".into(), json!(summary));
    }
    apply_common(&mut fields, &input);
    let parent = opt_str(&input, "parent_key").trim();
    if !parent.is_empty() {
        fields.insert("parent".into(), json!({"key": parent}));
    }
    let update = raw_obj(&input, "update").unwrap_or_default();
    if fields.is_empty() && update.is_empty() {
        return Err("at least one field or update instruction is required".into());
    }
    let mut body = Map::new();
    body.insert("fields".into(), Value::Object(fields));
    if !update.is_empty() {
        body.insert("update".into(), Value::Object(update));
    }
    jsend_noresp(
        host,
        "PUT",
        &format!("/issue/{}", urlencode(&key)),
        Some(&Value::Object(body)),
    )?;
    let mut issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    render_issue_body_format(&mut issue, body_format_from_input(&input));
    Ok(json!({"ok": true, "key": key, "issue": issue}))
}

pub(crate) fn issue_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let mut path = format!("/issue/{}", urlencode(&key));
    if input
        .get("delete_subtasks")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        path.push_str("?deleteSubtasks=true");
    }
    jsend_noresp(host, "DELETE", &path, None)?;
    Ok(json!({"ok": true, "key": key}))
}

pub(crate) fn issue_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let jql = build_jql(&input);
    let max = clamp_limit(&input, &["max", "limit"], 25, 100);
    let fields_param: Option<String> = input
        .get("fields")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|s| !s.is_empty());
    let fields_str = fields_param.as_deref().unwrap_or(FIELDS);
    let mut result = jget(
        host,
        &format!(
            "/search/jql?jql={}&maxResults={max}&fields={}",
            urlencode(&jql),
            urlencode(fields_str)
        ),
    )?;
    let format = body_format_from_input(&input);
    if let Some(issues) = result.get_mut("issues").and_then(|v| v.as_array_mut()) {
        for issue in issues {
            render_issue_body_format(issue, format);
        }
    }
    contribute_issues(host, &result);
    Ok(result)
}

pub(crate) fn issue_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let mut issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    render_issue_body_format(&mut issue, body_format_from_input(&input));
    Ok(issue)
}

pub(crate) fn create_meta(input: Value, host: &mut Host) -> Result<Value, String> {
    let mut path = String::from("/issue/createmeta?expand=projects.issuetypes.fields");
    let project = opt_str(&input, "project_key").trim();
    if !project.is_empty() {
        path.push_str(&format!("&projectKeys={}", urlencode(project)));
    }
    let issue_type = opt_str(&input, "issue_type").trim();
    if !issue_type.is_empty() {
        path.push_str(&format!("&issuetypeNames={}", urlencode(issue_type)));
    }
    let metadata = jget(host, &path)?;
    Ok(json!({"metadata": metadata}))
}

pub(crate) fn edit_meta(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let metadata = jget(host, &format!("/issue/{}/editmeta", urlencode(&key)))?;
    Ok(json!({"metadata": metadata}))
}

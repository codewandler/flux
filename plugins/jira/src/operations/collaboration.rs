//! Issue-link and user-search operations.

use super::*;

pub(crate) fn issue_link_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let to_key = opt_str(&input, "to_key").trim();
    let link_type = opt_str(&input, "type").trim();
    if to_key.is_empty() || link_type.is_empty() {
        return Err("key, to_key, and type are required".into());
    }
    // Reference `LinkIssues`: "key <verb> to_key" posts the type's name with inwardIssue=key,
    // outwardIssue=to_key.
    jsend_noresp(
        host,
        "POST",
        "/issueLink",
        Some(&json!({
            "type": {"name": link_type},
            "inwardIssue": {"key": key},
            "outwardIssue": {"key": to_key},
        })),
    )?;
    let issue = jget(
        host,
        &format!("/issue/{}?fields={}", urlencode(&key), urlencode(FIELDS)),
    )?;
    let links = issue
        .get("fields")
        .and_then(|f| f.get("issuelinks"))
        .cloned()
        .unwrap_or(json!([]));
    Ok(json!({"ok": true, "key": key, "to_key": to_key, "type": link_type, "links": links}))
}

pub(crate) fn user_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = opt_str(&input, "query").trim();
    let limit = clamp_limit(&input, &["limit"], 20, 100);
    let users = jget(
        host,
        &format!(
            "/user/search?query={}&maxResults={limit}&startAt=0",
            urlencode(query)
        ),
    )?;
    let count = users.as_array().map(|a| a.len()).unwrap_or(0);
    contribute_users(host, &users);
    Ok(json!({"users": users, "count": count}))
}

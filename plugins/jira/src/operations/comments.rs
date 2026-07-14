//! Comment creation, editing, deletion, listing, and body rendering.

use super::*;

pub(crate) fn comment_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let body = opt_str(&input, "body_markdown").trim();
    if body.is_empty() {
        return Err("`body_markdown` (string) required".into());
    }
    let resp = jsend(
        host,
        "POST",
        &format!("/issue/{}/comment", urlencode(&key)),
        &json!({"body": markdown_to_adf(body)}),
    )?;
    let comment_id = resp
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(json!({"ok": true, "issue_key": key, "comment_id": comment_id, "comment": resp}))
}

pub(crate) fn comment_edit(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let comment_id = opt_str(&input, "comment_id").trim();
    if comment_id.is_empty() {
        return Err("`comment_id` (string) required".into());
    }
    let body = opt_str(&input, "body_markdown").trim();
    if body.is_empty() {
        return Err("`body_markdown` (string) required".into());
    }
    let resp = jsend(
        host,
        "PUT",
        &format!(
            "/issue/{}/comment/{}",
            urlencode(&key),
            urlencode(comment_id)
        ),
        &json!({"body": markdown_to_adf(body)}),
    )?;
    let resolved = {
        let from_resp = resp.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if from_resp.is_empty() {
            comment_id.to_string()
        } else {
            from_resp.to_string()
        }
    };
    Ok(json!({"ok": true, "issue_key": key, "comment_id": resolved, "comment": resp}))
}

pub(crate) fn comment_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let comment_id = opt_str(&input, "comment_id").trim();
    if comment_id.is_empty() {
        return Err("`comment_id` (string) required".into());
    }
    jsend_noresp(
        host,
        "DELETE",
        &format!(
            "/issue/{}/comment/{}",
            urlencode(&key),
            urlencode(comment_id)
        ),
        None,
    )?;
    Ok(json!({"ok": true, "issue_key": key, "comment_id": comment_id}))
}

pub(crate) fn comment_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = issue_key(&input)?;
    let mut path = format!(
        "/issue/{}/comment?maxResults={}",
        urlencode(&key),
        clamp_limit(&input, &["limit"], 20, 100)
    );
    let start_at = input.get("start_at").and_then(|v| v.as_i64()).unwrap_or(0);
    if start_at > 0 {
        path.push_str(&format!("&startAt={start_at}"));
    }
    let order = opt_str(&input, "order").trim();
    if !order.is_empty() {
        path.push_str(&format!("&orderBy={}", urlencode(order)));
    }
    let format = body_format_from_input(&input);
    let mut page = jget(host, &path)?;
    if let Some(comments) = page.get_mut("comments").and_then(|v| v.as_array_mut()) {
        for comment in comments {
            render_comment_body_format(comment, format);
        }
    }
    let comments = page.get("comments").cloned().unwrap_or(json!([]));
    let count = comments.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(json!({
        "issue_key": key,
        "count": count,
        "total": page.get("total").cloned().unwrap_or(json!(count)),
        "start_at": page.get("startAt").cloned().unwrap_or(json!(0)),
        "comments": comments,
    }))
}

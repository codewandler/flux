//! Release, changelog, asset-link, and archive operations.

use super::*;

// ---------------------------------------------------------------------------
// Releases + asset links + changelog.
// ---------------------------------------------------------------------------

pub(crate) fn release_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let page = page_qs(&input);
    gl_get(
        host,
        &format!(
            "/projects/{}/releases?per_page={limit}{page}",
            enc(&project)
        ),
    )
}

pub(crate) fn release_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = flex_str(&input, "tag_name").ok_or("`tag_name` (string) required")?;
    let mut body = body_from(
        &input,
        &[
            "ref",
            "name",
            "description",
            "tag_message",
            "milestones",
            "released_at",
        ],
    );
    body.insert("tag_name".into(), json!(tag));
    if let Some(links) = input.get("assets_links").and_then(|v| v.as_array()) {
        body.insert("assets".into(), json!({ "links": links }));
    }
    gl_post(
        host,
        &format!("/projects/{}/releases", enc(&project)),
        &Value::Object(body),
    )
}

pub(crate) fn release_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    gl_get(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
    )
}

pub(crate) fn release_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let body = body_from(
        &input,
        &["name", "description", "milestones", "released_at"],
    );
    gl_put(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
        &Value::Object(body),
    )
}

pub(crate) fn release_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    confirm_str(&input, "confirm_tag_name", &tag)?;
    gl_delete(
        host,
        &format!("/projects/{}/releases/{}", enc(&project), enc(&tag)),
    )?;
    Ok(json!({ "project": project, "tag_name": tag, "message": "release deleted" }))
}

pub(crate) fn release_link_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let limit = clamp(
        flex_i64(&input, &["limit", "per_page"]).unwrap_or(0),
        20,
        200,
    );
    let page = page_qs(&input);
    gl_get(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links?per_page={limit}{page}",
            enc(&project),
            enc(&tag)
        ),
    )
}

pub(crate) fn release_link_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    require_keys(&input, &["name", "url"])?;
    let body = body_from(&input, &["name", "url", "direct_asset_path", "link_type"]);
    gl_post(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links",
            enc(&project),
            enc(&tag)
        ),
        &Value::Object(body),
    )
}

pub(crate) fn release_link_update(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let link_id = flex_i64(&input, &["link_id"]).ok_or("`link_id` (integer) required")?;
    let body = body_from(&input, &["name", "url", "direct_asset_path", "link_type"]);
    gl_put(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links/{link_id}",
            enc(&project),
            enc(&tag)
        ),
        &Value::Object(body),
    )
}

pub(crate) fn release_link_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let tag = release_tag(&input)?;
    let link_id = flex_i64(&input, &["link_id"]).ok_or("`link_id` (integer) required")?;
    gl_delete(
        host,
        &format!(
            "/projects/{}/releases/{}/assets/links/{link_id}",
            enc(&project),
            enc(&tag)
        ),
    )?;
    Ok(
        json!({ "project": project, "tag_name": tag, "link_id": link_id, "message": "release link deleted" }),
    )
}

pub(crate) fn changelog_generate(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let version = flex_str(&input, "version").ok_or("`version` (string) required")?;
    let pairs = [
        ("version", version),
        ("from", flex_str(&input, "from").unwrap_or_default()),
        ("to", flex_str(&input, "to").unwrap_or_default()),
        ("date", flex_str(&input, "date").unwrap_or_default()),
        ("trailer", flex_str(&input, "trailer").unwrap_or_default()),
        (
            "config_file",
            flex_str(&input, "config_file").unwrap_or_default(),
        ),
    ];
    gl_get(
        host,
        &format!(
            "/projects/{}/repository/changelog{}",
            enc(&project),
            qs(&pairs)
        ),
    )
}

pub(crate) fn changelog_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let version = flex_str(&input, "version").ok_or("`version` (string) required")?;
    // GL-037: require an explicit target branch — never let GitLab default this write to the
    // repo's default branch. (The schema also marks `branch` required, so the shared preflight
    // rejects a missing/blank branch in both --dry-run and runtime.)
    let branch = flex_str(&input, "branch").ok_or(
        "`branch` (string) required — name the branch to commit the changelog onto, rather than silently writing the default branch",
    )?;
    let mut body = body_from(
        &input,
        &[
            "branch",
            "file",
            "from",
            "to",
            "date",
            "message",
            "trailer",
            "config_file",
        ],
    );
    body.insert("branch".into(), json!(branch));
    body.insert("version".into(), json!(version.clone()));
    // The add-changelog endpoint returns no body.
    gl_request(
        host,
        "POST",
        &format!("/projects/{}/repository/changelog", enc(&project)),
        Some(&Value::Object(body)),
    )?;
    let file = flex_str(&input, "file").unwrap_or_else(|| "CHANGELOG.md".into());
    Ok(json!({
        "project": project, "version": version,
        "branch": flex_str(&input, "branch"), "file": file, "message": "changelog committed"
    }))
}

// ---------------------------------------------------------------------------
// Archive (blob): download then stage through the host blob store.
// ---------------------------------------------------------------------------

pub(crate) fn repository_archive(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_project(&input)?;
    let format = flex_str(&input, "format").unwrap_or_else(|| "tar.gz".into());
    let git_ref = flex_str(&input, "ref");
    let sub = flex_str(&input, "path");
    let pairs = [
        ("sha", git_ref.clone().unwrap_or_default()),
        ("path", sub.unwrap_or_default()),
    ];
    let path = format!(
        "/projects/{}/repository/archive.{format}{}",
        enc(&project),
        qs(&pairs)
    );
    let bytes = gl_get_bytes(host, &path)?;
    // GL-023: an "archive read" must not stage an unbounded blob — refuse oversized results
    // (the caller raises max_bytes explicitly to accept a bigger archive).
    let max_bytes = flex_i64(&input, &["max_bytes"])
        .filter(|v| *v > 0)
        .unwrap_or(52_428_800) as usize;
    if bytes.len() > max_bytes {
        return Err(format!(
            "archive is {} bytes, exceeding max_bytes {max_bytes} — pass a larger max_bytes to accept it",
            bytes.len()
        ));
    }
    let mut name = project.replace(['/', ' '], "-");
    if let Some(r) = &git_ref {
        name.push('-');
        name.push_str(&r.replace(['/', ' '], "-"));
    }
    let filename = format!("{name}.{format}");
    let blob_ref = host.blob_put(&filename, &bytes)?;
    Ok(json!({
        "project": project, "ref": git_ref, "format": format,
        "blob_ref": blob_ref, "filename": filename, "bytes": bytes.len()
    }))
}

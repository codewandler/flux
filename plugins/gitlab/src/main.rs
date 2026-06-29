//! `gitlab` — a flux integration plugin for the GitLab REST API (v4): projects, merge requests, issues,
//! and pipelines. Authenticates with a personal access token via the `PRIVATE-TOKEN` header; the base
//! URL is the `gitlab.endpoint` (defaults to gitlab.com). List ops contribute datasource records
//! (`gitlab.project` / `gitlab.merge_request` / `gitlab.issue`) so the agent can search them.
//!
//! This is the reference template for the HTTP-API integration plugins.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    let project_arg = json!({ "project": {"type": "string", "description": "project id or path (e.g. \"group/app\")"} });
    PluginBuilder::new("gitlab", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec![
                "GITLAB_PERSONAL_TOKEN".into(),
                "GITLAB_PERSONAL_ACCESS_TOKEN".into(),
            ],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "personal_token".into(),
            env: vec![
                "GITLAB_PERSONAL_TOKEN".into(),
                "GITLAB_PERSONAL_ACCESS_TOKEN".into(),
            ],
            description: "GitLab personal access token".into(),
        })
        .endpoint(EndpointSpec {
            name: "gitlab.endpoint".into(),
            env: vec!["GITLAB_URL".into(), "GITLAB_BASE_URL".into()],
            description: "GitLab base URL (default https://gitlab.com)".into(),
        })
        .datasource(ds("gitlab.projects", "gitlab.project", "GitLab projects."))
        .datasource(ds(
            "gitlab.merge_requests",
            "gitlab.merge_request",
            "GitLab merge requests.",
        ))
        .datasource(ds("gitlab.issues", "gitlab.issue", "GitLab issues."))
        .operation(
            read_op(
                "gitlab.project.list",
                "List/search projects the token can see.",
                json!({"type": "object", "properties": {"search": {"type": "string"}}}),
            ),
            project_list,
        )
        .operation(
            read_op(
                "gitlab.project.show",
                "Show one project by id or path.",
                json!({"type": "object", "properties": project_arg, "required": ["project"]}),
            ),
            project_show,
        )
        .operation(
            read_op(
                "gitlab.mr.list",
                "List a project's merge requests (state: opened|closed|merged|all).",
                json!({"type": "object", "properties": {
                    "project": {"type": "string"}, "state": {"type": "string"}
                }, "required": ["project"]}),
            ),
            mr_list,
        )
        .operation(
            read_op(
                "gitlab.mr.show",
                "Show one merge request by project + iid.",
                json!({"type": "object", "properties": {
                    "project": {"type": "string"}, "iid": {"type": "integer"}
                }, "required": ["project", "iid"]}),
            ),
            mr_show,
        )
        .operation(
            read_op(
                "gitlab.issue.list",
                "List a project's issues (state: opened|closed|all).",
                json!({"type": "object", "properties": {
                    "project": {"type": "string"}, "state": {"type": "string"}
                }, "required": ["project"]}),
            ),
            issue_list,
        )
        .operation(
            read_op(
                "gitlab.pipeline.list",
                "List a project's recent CI pipelines.",
                json!({"type": "object", "properties": {"project": {"type": "string"}}, "required": ["project"]}),
            ),
            pipeline_list,
        )
}

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into()],
        entity_schema: None,
    }
}

/// GET `{base}/api/v4{path}` with the PRIVATE-TOKEN header; returns the parsed JSON.
fn gl_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let base = host
        .endpoint("gitlab.endpoint")
        .unwrap_or_else(|_| "https://gitlab.com".into());
    let token = host.secret("personal_token")?;
    let url = format!("{}/api/v4{}", base.trim_end_matches('/'), path);
    let resp = host.http(
        "GET",
        &url,
        None,
        &[("PRIVATE-TOKEN", token.as_str())],
        None,
    )?;
    if !resp.is_success() {
        return Err(format!("gitlab GET {path} → {} {}", resp.status, resp.body));
    }
    resp.json()
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

/// Percent-encode a project id/path so `group/app` → `group%2Fapp` for the path segment.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn project_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let search = input.get("search").and_then(|v| v.as_str()).unwrap_or("");
    let path = if search.is_empty() {
        "/projects?membership=true&per_page=20&order_by=last_activity_at".to_string()
    } else {
        format!("/projects?search={}&per_page=20", enc(search))
    };
    let projects = gl_get(host, &path)?;
    contribute_projects(host, &projects);
    Ok(projects)
}

fn project_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_str(&input, "project")?;
    gl_get(host, &format!("/projects/{}", enc(project)))
}

fn mr_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_str(&input, "project")?;
    let state = input
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("opened");
    let mrs = gl_get(
        host,
        &format!(
            "/projects/{}/merge_requests?state={state}&per_page=20",
            enc(project)
        ),
    )?;
    contribute_list(
        host,
        &mrs,
        "gitlab.merge_request",
        project,
        "iid",
        "title",
        "description",
    );
    Ok(mrs)
}

fn mr_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_str(&input, "project")?;
    let iid = input
        .get("iid")
        .and_then(|v| v.as_i64())
        .ok_or("`iid` (integer) required")?;
    gl_get(
        host,
        &format!("/projects/{}/merge_requests/{iid}", enc(project)),
    )
}

fn issue_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_str(&input, "project")?;
    let state = input
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("opened");
    let issues = gl_get(
        host,
        &format!(
            "/projects/{}/issues?state={state}&per_page=20",
            enc(project)
        ),
    )?;
    contribute_list(
        host,
        &issues,
        "gitlab.issue",
        project,
        "iid",
        "title",
        "description",
    );
    Ok(issues)
}

fn pipeline_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let project = req_str(&input, "project")?;
    gl_get(
        host,
        &format!("/projects/{}/pipelines?per_page=20", enc(project)),
    )
}

fn contribute_projects(host: &mut Host, projects: &Value) {
    let Some(arr) = projects.as_array() else {
        return;
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
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

/// Contribute a list of entities keyed by `<project>!<id_field>`, with title/body from the named fields.
fn contribute_list(
    host: &mut Host,
    items: &Value,
    entity: &str,
    project: &str,
    id_field: &str,
    title_field: &str,
    body_field: &str,
) {
    let Some(arr) = items.as_array() else { return };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let id = it.get(id_field).map(|v| v.to_string())?;
            Some(Record::new(
                Source::new("gitlab"),
                entity,
                format!("{project}!{}", id.trim_matches('"')),
                it.get(title_field).and_then(|v| v.as_str()).unwrap_or(""),
                it.get(body_field).and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mr_list_calls_the_api_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("gitlab.endpoint", "https://gl.example.com")
            .with_secret("personal_token", "tok")
            .with_http(
                "/projects/group%2Fapp/merge_requests",
                json!([
                    { "iid": 7, "title": "Fix warm transfer", "description": "MR body" }
                ]),
            );
        let out = plugin
            .call(
                "gitlab.mr.list",
                json!({ "project": "group/app", "state": "opened" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out[0]["iid"], 7);
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "gitlab.merge_request");
        assert_eq!(recs[0].id, "group/app!7");
        assert_eq!(recs[0].title, "Fix warm transfer");
    }

    #[test]
    fn project_show_encodes_the_path() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_secret("personal_token", "tok")
            // default endpoint (gitlab.com) since none configured; matched by the encoded path
            .with_http(
                "gitlab.com/api/v4/projects/group%2Fapp",
                json!({ "id": 1, "name": "app" }),
            );
        let out = plugin
            .call(
                "gitlab.project.show",
                json!({ "project": "group/app" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["name"], "app");
    }

    #[test]
    fn manifest_declares_ops_auth_and_datasources() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 6);
        assert_eq!(m.auth[0].purpose, "personal_token");
        assert!(m
            .datasources
            .iter()
            .any(|d| d.entity == "gitlab.merge_request"));
    }
}

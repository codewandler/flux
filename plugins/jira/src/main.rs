//! `jira` — a flux integration plugin for the Atlassian Jira Cloud REST API (v3): issue search, single
//! issue lookup, and project listing. Authenticates with HTTP Basic (`email:api_token`); the base URL is
//! the `jira.endpoint` (e.g. `https://site.atlassian.net`, no default). `jira.issue.search` contributes
//! `jira.issue` datasource records so the agent can search them.
//!
//! host-kit only injects `Authorization: Bearer`, while Jira needs Basic auth, so we resolve both secrets
//! by purpose and build the header ourselves (with a tiny inline base64 encoder).

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("jira", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec![
                "JIRA_API_TOKEN".into(),
                "ATLASSIAN_API_TOKEN".into(),
                "JIRA_EMAIL".into(),
                "ATLASSIAN_EMAIL".into(),
            ],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "api_token".into(),
            env: vec!["JIRA_API_TOKEN".into(), "ATLASSIAN_API_TOKEN".into()],
            description: "Atlassian API token (the password half of Basic auth)".into(),
        })
        .auth(AuthMethod {
            purpose: "email".into(),
            env: vec!["JIRA_EMAIL".into(), "ATLASSIAN_EMAIL".into()],
            description: "Atlassian account email (the username half of Basic auth)".into(),
        })
        .endpoint(EndpointSpec {
            name: "jira.endpoint".into(),
            env: vec!["JIRA_URL".into(), "ATLASSIAN_URL".into()],
            description: "Jira Cloud base URL (e.g. https://site.atlassian.net)".into(),
        })
        .datasource(ds("jira.issues", "jira.issue", "Jira issues."))
        .operation(
            read_op(
                "jira.issue.search",
                "Search issues with a JQL query (e.g. `project = PROJ AND status = Open`).",
                json!({"type": "object", "properties": {
                    "jql": {"type": "string", "description": "JQL query"},
                    "max": {"type": "integer", "description": "max results (default 25)"}
                }, "required": ["jql"]}),
            ),
            issue_search,
        )
        .operation(
            read_op(
                "jira.issue.show",
                "Show one issue by key (e.g. PROJ-123).",
                json!({"type": "object", "properties": {
                    "key": {"type": "string", "description": "issue key (e.g. PROJ-123)"}
                }, "required": ["key"]}),
            ),
            issue_show,
        )
        .operation(
            read_op(
                "jira.project.list",
                "List/search the projects the account can see.",
                json!({"type": "object", "properties": {}}),
            ),
            project_list,
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

/// GET `{base}/rest/api/3{path}` with a self-built `Authorization: Basic <base64(email:token)>` header;
/// returns the parsed JSON. The base URL is required (no default).
fn jira_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let base = host.endpoint("jira.endpoint")?;
    let token = host.secret("api_token")?;
    let email = host.secret("email")?;
    let auth = format!(
        "Basic {}",
        base64_encode(format!("{email}:{token}").as_bytes())
    );
    let url = format!("{}/rest/api/3{}", base.trim_end_matches('/'), path);
    let resp = host.http("GET", &url, None, &[("Authorization", auth.as_str())], None)?;
    if !resp.is_success() {
        return Err(format!("jira GET {path} → {} {}", resp.status, resp.body));
    }
    resp.json()
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

fn issue_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let jql = req_str(&input, "jql")?;
    let max = input.get("max").and_then(|v| v.as_i64()).unwrap_or(25);
    let result = jira_get(
        host,
        &format!("/search?jql={}&maxResults={max}", urlencode(jql)),
    )?;
    contribute_issues(host, &result);
    Ok(result)
}

fn issue_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let key = req_str(&input, "key")?;
    jira_get(host, &format!("/issue/{}", urlencode(key)))
}

fn project_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    jira_get(host, "/project/search")
}

/// Contribute one `jira.issue` record per issue in `result.issues[]` (id = key, title = summary,
/// body = summary plus the status name when present).
fn contribute_issues(host: &mut Host, result: &Value) {
    let Some(arr) = result.get("issues").and_then(|v| v.as_array()) else {
        return;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|it| {
            let key = it.get("key").and_then(|v| v.as_str())?;
            let fields = it.get("fields");
            let summary = fields
                .and_then(|f| f.get("summary"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let status = fields
                .and_then(|f| f.get("status"))
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str());
            let body = match status {
                Some(s) => format!("{summary} [{s}]"),
                None => summary.to_string(),
            };
            Some(Record::new(
                Source::new("jira"),
                "jira.issue",
                key,
                summary,
                body,
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

/// Percent-encode a query/path component: unreserved chars (`alnum -_.~`) pass through, all else `%XX`.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Standard base64 (RFC 4648 alphabet, with `=` padding) — Jira Basic auth needs the credentials encoded.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_search_calls_the_api_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("jira.endpoint", "https://x.atlassian.net")
            .with_secret("api_token", "t")
            .with_secret("email", "a@b.c")
            .with_http(
                "/rest/api/3/search",
                json!({"issues": [
                    {"key": "PROJ-1", "fields": {"summary": "Warm transfer bug", "status": {"name": "Open"}}}
                ]}),
            );
        let out = plugin
            .call(
                "jira.issue.search",
                json!({ "jql": "project = PROJ", "max": 10 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["issues"][0]["key"], "PROJ-1");

        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "jira.issue");
        assert_eq!(recs[0].id, "PROJ-1");
        assert_eq!(recs[0].title, "Warm transfer bug");
        assert!(recs[0].body.contains("Open"));
    }

    #[test]
    fn issue_show_fetches_by_key() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("jira.endpoint", "https://x.atlassian.net")
            .with_secret("api_token", "t")
            .with_secret("email", "a@b.c")
            .with_http(
                "/rest/api/3/issue/PROJ-1",
                json!({"key": "PROJ-1", "fields": {"summary": "Warm transfer bug"}}),
            );
        let out = plugin
            .call("jira.issue.show", json!({ "key": "PROJ-1" }), &mut host)
            .unwrap();
        assert_eq!(out["key"], "PROJ-1");
    }

    #[test]
    fn manifest_declares_ops_auth_and_datasource() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 3);
        let purposes: Vec<&str> = m.auth.iter().map(|a| a.purpose.as_str()).collect();
        assert!(purposes.contains(&"api_token"));
        assert!(purposes.contains(&"email"));
        assert!(m.datasources.iter().any(|d| d.entity == "jira.issue"));
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 examples plus a credential-shaped input.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"a@b.c:t"), "YUBiLmM6dA==");
    }
}

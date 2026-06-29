//! `confluence` — a flux integration plugin for the Atlassian Confluence Cloud REST API: content
//! search (CQL), page fetch, and space listing. Authenticates with HTTP Basic `email:api_token`
//! (the same scheme as Jira); the base URL is the `confluence.endpoint` (e.g.
//! `https://site.atlassian.net`). Search contributes `confluence.page` datasource records so the
//! agent can search them.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("confluence", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec![
                "CONFLUENCE_API_TOKEN".into(),
                "ATLASSIAN_API_TOKEN".into(),
                "JIRA_API_TOKEN".into(),
                "CONFLUENCE_EMAIL".into(),
                "ATLASSIAN_EMAIL".into(),
                "JIRA_EMAIL".into(),
            ],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "api_token".into(),
            env: vec![
                "CONFLUENCE_API_TOKEN".into(),
                "ATLASSIAN_API_TOKEN".into(),
                "JIRA_API_TOKEN".into(),
            ],
            description: "Atlassian API token (Basic auth password)".into(),
        })
        .auth(AuthMethod {
            purpose: "email".into(),
            env: vec![
                "CONFLUENCE_EMAIL".into(),
                "ATLASSIAN_EMAIL".into(),
                "JIRA_EMAIL".into(),
            ],
            description: "Atlassian account email (Basic auth username)".into(),
        })
        .endpoint(EndpointSpec {
            name: "confluence.endpoint".into(),
            env: vec!["CONFLUENCE_URL".into(), "ATLASSIAN_URL".into()],
            description: "Confluence Cloud base URL (e.g. https://site.atlassian.net)".into(),
        })
        .datasource(Declaration {
            name: "confluence.pages".into(),
            entity: "confluence.page".into(),
            description: Some("Confluence pages and content.".into()),
            capabilities: vec!["search".into(), "get".into()],
            entity_schema: None,
        })
        .operation(
            read_op(
                "confluence.search",
                "Search content with CQL (Confluence Query Language).",
                json!({"type": "object", "properties": {
                    "cql": {"type": "string", "description": "CQL query, e.g. text ~ \"runbook\""},
                    "limit": {"type": "integer", "description": "max results (default 25)"}
                }, "required": ["cql"]}),
            ),
            search,
        )
        .operation(
            read_op(
                "confluence.page.show",
                "Show one page/content by id (expands body + space).",
                json!({"type": "object", "properties": {
                    "id": {"type": "string", "description": "content id"}
                }, "required": ["id"]}),
            ),
            page_show,
        )
        .operation(
            read_op(
                "confluence.space.list",
                "List spaces the account can see.",
                json!({"type": "object", "properties": {}}),
            ),
            space_list,
        )
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

/// Percent-encode a string for a query value (RFC 3986 unreserved chars pass through).
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

/// Standard-alphabet base64 with padding (no external crate).
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Build the `Authorization: Basic <base64(email:token)>` header value from secrets-by-purpose.
fn auth_header(host: &mut Host) -> Result<String, String> {
    let email = host.secret("email")?;
    let token = host.secret("api_token")?;
    Ok(format!(
        "Basic {}",
        base64(format!("{email}:{token}").as_bytes())
    ))
}

/// GET `{base}{path}` with the Basic-auth header; returns the parsed JSON.
fn cf_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let base = host.endpoint("confluence.endpoint")?;
    let auth = auth_header(host)?;
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let resp = host.http(
        "GET",
        &url,
        None,
        &[
            ("Authorization", auth.as_str()),
            ("Accept", "application/json"),
        ],
        None,
    )?;
    if !resp.is_success() {
        return Err(format!(
            "confluence GET {path} → {} {}",
            resp.status, resp.body
        ));
    }
    resp.json()
}

fn search(input: Value, host: &mut Host) -> Result<Value, String> {
    let cql = req_str(&input, "cql")?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(25);
    let path = format!(
        "/wiki/rest/api/content/search?cql={}&limit={limit}",
        urlencode(cql)
    );
    let result = cf_get(host, &path)?;
    contribute_pages(host, &result);
    Ok(result)
}

fn page_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = req_str(&input, "id")?;
    cf_get(
        host,
        &format!(
            "/wiki/rest/api/content/{}?expand=body.storage,space",
            urlencode(id)
        ),
    )
}

fn space_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    cf_get(host, "/wiki/rest/api/space?limit=50")
}

/// Contribute a `confluence.page` record per `.results[]`: body is the title (+ space key if present).
fn contribute_pages(host: &mut Host, result: &Value) {
    let Some(arr) = result.get("results").and_then(|v| v.as_array()) else {
        return;
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|p| {
            let id = p.get("id").and_then(|v| v.as_str())?;
            let title = p.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let body = match p
                .get("space")
                .and_then(|s| s.get("key"))
                .and_then(|v| v.as_str())
            {
                Some(key) => format!("{title} (space {key})"),
                None => title.to_string(),
            };
            Some(Record::new(
                Source::new("confluence"),
                "confluence.page",
                id,
                title,
                body,
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
    fn base64_encodes_with_padding() {
        // standard-alphabet RFC 4648 vectors
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"a@b.c:tok"), "YUBiLmM6dG9r");
    }

    #[test]
    fn search_calls_the_api_and_contributes_a_page_record() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("confluence.endpoint", "https://x.atlassian.net")
            .with_secret("api_token", "t")
            .with_secret("email", "a@b.c")
            .with_http(
                "/wiki/rest/api/content/search",
                json!({"results": [
                    {"id": "123", "title": "Warm transfer runbook", "space": {"key": "OPS"}}
                ]}),
            );
        let out = plugin
            .call(
                "confluence.search",
                json!({ "cql": "text ~ \"runbook\"" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["results"][0]["id"], "123");

        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "confluence.page");
        assert_eq!(recs[0].id, "123");
        assert_eq!(recs[0].title, "Warm transfer runbook");
        assert_eq!(recs[0].body, "Warm transfer runbook (space OPS)");
    }

    #[test]
    fn page_show_fetches_by_id() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("confluence.endpoint", "https://x.atlassian.net")
            .with_secret("api_token", "t")
            .with_secret("email", "a@b.c")
            .with_http(
                "/wiki/rest/api/content/123",
                json!({"id": "123", "title": "Warm transfer runbook"}),
            );
        let out = plugin
            .call("confluence.page.show", json!({ "id": "123" }), &mut host)
            .unwrap();
        assert_eq!(out["title"], "Warm transfer runbook");
    }

    #[test]
    fn manifest_declares_ops_auth_endpoint_and_datasource() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 3);
        assert!(m.auth.iter().any(|a| a.purpose == "api_token"));
        assert!(m.auth.iter().any(|a| a.purpose == "email"));
        assert_eq!(m.endpoints[0].name, "confluence.endpoint");
        assert!(m.datasources.iter().any(|d| d.entity == "confluence.page"));
    }
}

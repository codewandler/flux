//! `loki` — a flux integration plugin for the Grafana Loki HTTP API: instant LogQL queries, range
//! queries, and label discovery. The base URL is the `loki.endpoint`; bearer auth is OPTIONAL (Loki is
//! often unauthenticated) and injected by the host when a `loki_token` secret is configured. All ops are
//! read-only and return Loki's JSON verbatim (logs are transient, so no datasource records are emitted).

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    let query_arg = json!({ "query": {"type": "string", "description": "a LogQL expression, e.g. {app=\"web\"} |= \"error\""} });
    PluginBuilder::new("loki", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec!["LOKI_TOKEN".into(), "LOKI_BEARER_TOKEN".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "loki_token".into(),
            env: vec!["LOKI_TOKEN".into(), "LOKI_BEARER_TOKEN".into()],
            description: "Optional bearer token for the Loki HTTP API".into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "loki.endpoint".into(),
            env: vec!["LOKI_URL".into(), "LOKI_ADDR".into()],
            description: "Loki base URL (e.g. https://loki.example.com)".into(),
        })
        .operation(
            read_op(
                "loki.query",
                "Run an instant LogQL query and return the matching streams/values.",
                json!({"type": "object", "properties": {
                    "query": query_arg["query"].clone(),
                    "limit": {"type": "integer", "description": "max entries to return (default 50)"}
                }, "required": ["query"]}),
            ),
            query,
        )
        .operation(
            read_op(
                "loki.query_range",
                "Run a LogQL query over a time range (start/end/step are optional RFC3339 or unix-ns).",
                json!({"type": "object", "properties": {
                    "query": query_arg["query"].clone(),
                    "start": {"type": "string", "description": "range start (RFC3339 or unix ns)"},
                    "end": {"type": "string", "description": "range end (RFC3339 or unix ns)"},
                    "limit": {"type": "integer", "description": "max entries to return"},
                    "step": {"type": "string", "description": "query resolution step, e.g. \"30s\""}
                }, "required": ["query"]}),
            ),
            query_range,
        )
        .operation(
            read_op(
                "loki.labels",
                "List the label names known to Loki.",
                json!({"type": "object", "properties": {}}),
            ),
            labels,
        )
}

/// Percent-encode a string so a LogQL expression is safe in a query string: alnum and `-_.~` pass
/// through unchanged, everything else becomes `%XX`.
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

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

/// Resolve the Loki base URL (no default) and decide the optional bearer purpose for this call.
fn base_and_bearer(host: &mut Host) -> Result<(String, Option<&'static str>), String> {
    let base = host.endpoint("loki.endpoint")?;
    let bearer = if host.secret("loki_token").is_ok() {
        Some("loki_token")
    } else {
        None
    };
    Ok((base.trim_end_matches('/').to_string(), bearer))
}

fn query(input: Value, host: &mut Host) -> Result<Value, String> {
    let expr = req_str(&input, "query")?;
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
    let (base, bearer) = base_and_bearer(host)?;
    let url = format!(
        "{base}/loki/api/v1/query?query={}&limit={limit}",
        urlencode(expr)
    );
    host.get_json(&url, bearer)
}

fn query_range(input: Value, host: &mut Host) -> Result<Value, String> {
    let expr = req_str(&input, "query")?;
    let (base, bearer) = base_and_bearer(host)?;
    let mut url = format!("{base}/loki/api/v1/query_range?query={}", urlencode(expr));
    if let Some(start) = input.get("start").and_then(|v| v.as_str()) {
        url.push_str(&format!("&start={}", urlencode(start)));
    }
    if let Some(end) = input.get("end").and_then(|v| v.as_str()) {
        url.push_str(&format!("&end={}", urlencode(end)));
    }
    if let Some(limit) = input.get("limit").and_then(|v| v.as_i64()) {
        url.push_str(&format!("&limit={limit}"));
    }
    if let Some(step) = input.get("step").and_then(|v| v.as_str()) {
        url.push_str(&format!("&step={}", urlencode(step)));
    }
    host.get_json(&url, bearer)
}

fn labels(_input: Value, host: &mut Host) -> Result<Value, String> {
    let (base, bearer) = base_and_bearer(host)?;
    host.get_json(&format!("{base}/loki/api/v1/labels"), bearer)
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_runs_without_a_token() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/query",
                json!({"status": "success", "data": {"resultType": "streams", "result": []}}),
            );
        let out = plugin
            .call(
                "loki.query",
                json!({ "query": "{app=\"web\"} |= \"error\"" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["status"], "success");
        assert_eq!(out["data"]["resultType"], "streams");
    }

    #[test]
    fn query_runs_with_a_token() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_secret("loki_token", "t")
            .with_http(
                "/loki/api/v1/query",
                json!({"status": "success", "data": {"resultType": "streams", "result": []}}),
            );
        let out = plugin
            .call(
                "loki.query",
                json!({ "query": "{app=\"web\"}", "limit": 10 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["status"], "success");
    }

    #[test]
    fn query_range_only_appends_provided_params() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "matrix", "result": []}}),
            );
        let out = plugin
            .call(
                "loki.query_range",
                json!({ "query": "rate({app=\"web\"}[5m])", "start": "1700000000", "step": "30s" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["data"]["resultType"], "matrix");
    }

    #[test]
    fn labels_lists_label_names() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/labels",
                json!({"status": "success", "data": ["app", "namespace"]}),
            );
        let out = plugin.call("loki.labels", json!({}), &mut host).unwrap();
        assert_eq!(out["data"][0], "app");
    }

    #[test]
    fn manifest_declares_ops_auth_and_endpoint() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 3);
        assert_eq!(m.auth[0].purpose, "loki_token");
        assert_eq!(m.endpoints[0].name, "loki.endpoint");
        assert!(m.operations.iter().all(|o| o.effects == vec![Effect::Read]));
    }
}

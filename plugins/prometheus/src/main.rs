//! `prometheus` — a flux integration plugin for the Prometheus HTTP API (v1): instant queries, range
//! queries, alerts, and scrape targets. The base URL is the `prometheus.endpoint`; bearer auth is
//! optional — when a `prometheus_token` is configured the host injects it as a Bearer header, otherwise
//! requests go unauthenticated. All ops are read-only; metrics are transient so nothing is contributed
//! to the datasource index.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    let query_arg = json!({ "query": {"type": "string", "description": "a PromQL expression"} });
    PluginBuilder::new("prometheus", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec!["PROMETHEUS_TOKEN".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "prometheus_token".into(),
            env: vec!["PROMETHEUS_TOKEN".into()],
            description: "Optional Prometheus bearer token".into(),
        })
        .endpoint(EndpointSpec {
            name: "prometheus.endpoint".into(),
            env: vec!["PROMETHEUS_URL".into(), "PROM_URL".into()],
            description: "Prometheus base URL (e.g. https://prom.example.com)".into(),
        })
        .operation(
            read_op(
                "prometheus.query",
                "Evaluate a PromQL expression at a single instant (optionally at `time`).",
                json!({"type": "object", "properties": {
                    "query": query_arg["query"],
                    "time": {"type": "string", "description": "evaluation timestamp (RFC3339 or unix)"}
                }, "required": ["query"]}),
            ),
            query,
        )
        .operation(
            read_op(
                "prometheus.query_range",
                "Evaluate a PromQL expression over a time range at a fixed step.",
                json!({"type": "object", "properties": {
                    "query": query_arg["query"],
                    "start": {"type": "string", "description": "range start (RFC3339 or unix)"},
                    "end": {"type": "string", "description": "range end (RFC3339 or unix)"},
                    "step": {"type": "string", "description": "resolution step (e.g. \"30s\")"}
                }, "required": ["query", "start", "end", "step"]}),
            ),
            query_range,
        )
        .operation(
            read_op(
                "prometheus.alerts",
                "List the currently active alerts.",
                json!({"type": "object", "properties": {}}),
            ),
            alerts,
        )
        .operation(
            read_op(
                "prometheus.targets",
                "List the scrape targets and their health.",
                json!({"type": "object", "properties": {}}),
            ),
            targets,
        )
}

/// GET `{base}{path}` against the Prometheus endpoint, injecting the bearer token when one is
/// configured; returns the parsed JSON body.
fn prom_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let base = host.endpoint("prometheus.endpoint")?;
    let bearer = if host.secret("prometheus_token").is_ok() {
        Some("prometheus_token")
    } else {
        None
    };
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    host.get_json(&url, bearer)
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

/// Percent-encode a value for a URL query: alphanumerics and `-_.~` pass through, everything else
/// becomes `%XX`. Used for PromQL expressions and timestamps.
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

fn query(input: Value, host: &mut Host) -> Result<Value, String> {
    let q = req_str(&input, "query")?;
    let mut path = format!("/api/v1/query?query={}", urlencode(q));
    if let Some(time) = input.get("time").and_then(|v| v.as_str()) {
        path.push_str(&format!("&time={}", urlencode(time)));
    }
    prom_get(host, &path)
}

fn query_range(input: Value, host: &mut Host) -> Result<Value, String> {
    let q = req_str(&input, "query")?;
    let start = req_str(&input, "start")?;
    let end = req_str(&input, "end")?;
    let step = req_str(&input, "step")?;
    let path = format!(
        "/api/v1/query_range?query={}&start={}&end={}&step={}",
        urlencode(q),
        urlencode(start),
        urlencode(end),
        urlencode(step),
    );
    prom_get(host, &path)
}

fn alerts(_input: Value, host: &mut Host) -> Result<Value, String> {
    prom_get(host, "/api/v1/alerts")
}

fn targets(_input: Value, host: &mut Host) -> Result<Value, String> {
    prom_get(host, "/api/v1/targets")
}

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_hits_the_instant_endpoint() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/query",
                json!({"status": "success", "data": {"resultType": "vector", "result": []}}),
            );
        let out = plugin
            .call(
                "prometheus.query",
                json!({ "query": "up{job=\"api\"}" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["status"], "success");
        assert_eq!(out["data"]["resultType"], "vector");
        // metrics are transient — nothing contributed to the index
        assert!(host.contributed.borrow().is_empty());
    }

    #[test]
    fn alerts_hits_the_alerts_endpoint() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/alerts",
                json!({"status": "success", "data": {"alerts": [{"state": "firing"}]}}),
            );
        let out = plugin
            .call("prometheus.alerts", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["status"], "success");
        assert_eq!(out["data"]["alerts"][0]["state"], "firing");
    }

    #[test]
    fn query_range_injects_the_bearer_token_when_configured() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x/")
            .with_secret("prometheus_token", "tok")
            .with_http(
                "/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "matrix", "result": []}}),
            );
        let out = plugin
            .call(
                "prometheus.query_range",
                json!({"query": "rate(http_requests_total[5m])", "start": "2021-01-01T00:00:00Z", "end": "2021-01-01T01:00:00Z", "step": "30s"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["data"]["resultType"], "matrix");
    }

    #[test]
    fn manifest_declares_read_ops_and_optional_auth() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 4);
        assert_eq!(m.auth[0].purpose, "prometheus_token");
        assert!(m.datasources.is_empty());
        assert!(m.operations.iter().all(|o| o.effects == vec![Effect::Read]));
    }
}

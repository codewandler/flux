//! `grafana` — a flux integration plugin for Grafana: datasource catalog, dashboard search,
//! annotations, and proxy ops for Loki, Prometheus, Alertmanager, and Tempo (20 ops).
//!
//! Auth: service-account Bearer token preferred (`GRAFANA_API_TOKEN`), HTTP Basic fallback
//! (`GRAFANA_USERNAME`/`GRAFANA_PASSWORD`). All IO is through the host; never direct HTTP.

use host_kit::*;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("grafana", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec!["GRAFANA_API_TOKEN".into(), "GRAFANA_PASSWORD".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "api_token".into(),
            scheme: AuthScheme::Bearer,
            env: vec!["GRAFANA_API_TOKEN".into()],
            description: "Grafana service-account token (preferred).".into(),
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "basic".into(),
            scheme: AuthScheme::Basic,
            user_env: vec!["GRAFANA_USERNAME".into()],
            env: vec!["GRAFANA_PASSWORD".into()],
            description: "Grafana HTTP Basic auth fallback.".into(),
        })
        .endpoint(EndpointSpec {
            name: "grafana.endpoint".into(),
            env: vec!["GRAFANA_URL".into()],
            description: "Grafana base URL (e.g. https://grafana.example.com).".into(),
        })
        .datasource(ds(
            "grafana.dashboards",
            "grafana.dashboard",
            "Grafana dashboards.",
        ))
        .datasource(ds(
            "grafana.annotations",
            "grafana.annotation",
            "Grafana annotations.",
        ))
        // ---- connectivity / auth ----
        .operation(
            read_op(
                "grafana.test",
                "Probe Grafana reachability (/api/health, no auth) then credential validity (/api/org).",
                so(json!({}), json!([])),
            ),
            op_test,
        )
        // ---- datasources ----
        .operation(
            read_op(
                "grafana.datasource.list",
                "List Grafana datasources with derived cluster aliases.",
                so(json!({}), json!([])),
            ),
            op_datasource_list,
        )
        .operation(
            read_op(
                "grafana.datasource.health",
                "Check health of one Grafana datasource by UID.",
                so(
                    json!({"uid": {"type": "string", "description": "Grafana datasource UID."}}),
                    json!(["uid"]),
                ),
            ),
            op_datasource_health,
        )
        // ---- folders / dashboards ----
        .operation(
            read_op(
                "grafana.folder.list",
                "List Grafana folders.",
                so(
                    json!({"limit": {"type": "integer", "description": "Maximum folders."}}),
                    json!([]),
                ),
            ),
            op_folder_list,
        )
        .operation(
            read_op(
                "grafana.dashboard.list",
                "Search Grafana dashboards; optionally filter by query, folder_uid, or tags.",
                so(
                    json!({
                        "query":      {"type": "string"},
                        "folder_uid": {"type": "string"},
                        "tags":       {"type": "array", "items": {"type": "string"}},
                        "limit":      {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_dashboard_list,
        )
        .operation(
            read_op(
                "grafana.dashboard.get",
                "Fetch a Grafana dashboard by UID and extract panel target queries.",
                so(
                    json!({"uid": {"type": "string", "description": "Dashboard UID."}}),
                    json!(["uid"]),
                ),
            ),
            op_dashboard_get,
        )
        // ---- annotations ----
        .operation(
            read_op(
                "grafana.annotation.list",
                "List Grafana annotations; filter by tags, dashboard_uid, and time window.",
                so(
                    json!({
                        "since":         {"type": "string"},
                        "until":         {"type": "string"},
                        "tags":          {"type": "array", "items": {"type": "string"}},
                        "dashboard_uid": {"type": "string"},
                        "limit":         {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_annotation_list,
        )
        .operation(
            write_op(
                "grafana.annotation.add",
                "Create a Grafana annotation.",
                so(
                    json!({
                        "text":          {"type": "string", "description": "Annotation text."},
                        "time":          {"type": "string"},
                        "time_end":      {"type": "string"},
                        "tags":          {"type": "array", "items": {"type": "string"}},
                        "dashboard_uid": {"type": "string"},
                        "panel_id":      {"type": "integer"}
                    }),
                    json!(["text"]),
                ),
            ),
            op_annotation_add,
        )
        // ---- Loki (proxied) ----
        .operation(
            read_op(
                "grafana.loki.labels",
                "List Loki labels (or values for one label) through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster": {"type": "string", "description": "Cluster alias from datasource.list or datasource UID suffix."},
                        "uid":     {"type": "string", "description": "Grafana datasource UID override."},
                        "label":   {"type": "string"},
                        "query":   {"type": "string"}
                    }),
                    json!([]),
                ),
            ),
            op_loki_labels,
        )
        .operation(
            read_op(
                "grafana.loki.query",
                "Run a Loki range query through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster": {"type": "string"},
                        "uid":     {"type": "string"},
                        "query":   {"type": "string", "description": "LogQL query."},
                        "since":   {"type": "string"},
                        "until":   {"type": "string"},
                        "limit":   {"type": "integer"}
                    }),
                    json!(["query"]),
                ),
            ),
            op_loki_query,
        )
        .operation(
            read_op(
                "grafana.loki.recent_logs",
                "Query recent Loki logs by cluster, app, namespace, and optional contains filter.",
                so(
                    json!({
                        "cluster":   {"type": "string"},
                        "uid":       {"type": "string"},
                        "app":       {"type": "string"},
                        "namespace": {"type": "string"},
                        "contains":  {"type": "string"},
                        "since":     {"type": "string"},
                        "until":     {"type": "string"},
                        "limit":     {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_loki_recent_logs,
        )
        // ---- Prometheus (proxied) ----
        .operation(
            read_op(
                "grafana.prometheus.query",
                "Run an instant Prometheus query through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster": {"type": "string"},
                        "uid":     {"type": "string"},
                        "query":   {"type": "string", "description": "PromQL query."},
                        "time":    {"type": "string"}
                    }),
                    json!(["query"]),
                ),
            ),
            op_prometheus_query,
        )
        .operation(
            read_op(
                "grafana.prometheus.range",
                "Run a Prometheus range query through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster": {"type": "string"},
                        "uid":     {"type": "string"},
                        "query":   {"type": "string"},
                        "since":   {"type": "string"},
                        "until":   {"type": "string"},
                        "step":    {"type": "string"}
                    }),
                    json!(["query"]),
                ),
            ),
            op_prometheus_range,
        )
        .operation(
            read_op(
                "grafana.prometheus.rules",
                "List Prometheus alerting and recording rules through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster": {"type": "string"},
                        "uid":     {"type": "string"},
                        "type":    {"type": "string", "description": "alert or record."}
                    }),
                    json!([]),
                ),
            ),
            op_prometheus_rules,
        )
        // ---- Alertmanager (proxied) ----
        .operation(
            read_op(
                "grafana.alerts.active",
                "List active Alertmanager alerts through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster":   {"type": "string"},
                        "uid":       {"type": "string"},
                        "severity":  {"type": "string"},
                        "namespace": {"type": "string"}
                    }),
                    json!([]),
                ),
            ),
            op_alerts_active,
        )
        .operation(
            read_op(
                "grafana.alerts.silences.list",
                "List Alertmanager silences through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster": {"type": "string"},
                        "uid":     {"type": "string"},
                        "filter":  {"type": "array", "items": {"type": "string"}}
                    }),
                    json!([]),
                ),
            ),
            op_alerts_silences_list,
        )
        .operation(
            write_op(
                "grafana.alerts.silences.create",
                "Create an Alertmanager silence through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster":    {"type": "string"},
                        "uid":        {"type": "string"},
                        "matchers":   {"type": "array",  "description": "Array of {name, value, is_regex?} matchers."},
                        "starts_at":  {"type": "string"},
                        "ends_at":    {"type": "string", "description": "End time as RFC3339, unix, or duration (e.g. 2h)."},
                        "created_by": {"type": "string"},
                        "comment":    {"type": "string"}
                    }),
                    json!(["matchers", "ends_at", "comment"]),
                ),
            ),
            op_alerts_silences_create,
        )
        .operation(
            write_op(
                "grafana.alerts.silences.delete",
                "Delete an Alertmanager silence through the Grafana datasource proxy.",
                so(
                    json!({
                        "cluster":    {"type": "string"},
                        "uid":        {"type": "string"},
                        "silence_id": {"type": "string"}
                    }),
                    json!(["silence_id"]),
                ),
            ),
            op_alerts_silences_delete,
        )
        // ---- Tempo (proxied) ----
        .operation(
            read_op(
                "grafana.tempo.search",
                "Search Tempo traces through the Grafana datasource proxy.",
                so(
                    json!({
                        "uid":   {"type": "string", "description": "Tempo datasource UID."},
                        "query": {"type": "string"},
                        "since": {"type": "string"},
                        "until": {"type": "string"},
                        "limit": {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_tempo_search,
        )
        .operation(
            read_op(
                "grafana.tempo.trace.get",
                "Fetch a Tempo trace by ID through the Grafana datasource proxy.",
                so(
                    json!({
                        "uid":      {"type": "string"},
                        "trace_id": {"type": "string"}
                    }),
                    json!(["trace_id"]),
                ),
            ),
            op_tempo_trace_get,
        )
}

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}

fn so(props: Value, required: Value) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// Pick the auth purpose: prefer "api_token" if set, then "basic", then None.
fn auth_purpose(host: &mut Host) -> Option<&'static str> {
    if host.secret("api_token").is_ok() {
        Some("api_token")
    } else if host.secret("basic").is_ok() {
        Some("basic")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// HTTP plumbing
// ---------------------------------------------------------------------------

fn gf_base(host: &mut Host) -> Result<String, String> {
    host.endpoint("grafana.endpoint")
        .map(|u| u.trim_end_matches('/').to_string())
}

fn gf_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let base = gf_base(host)?;
    let ap = auth_purpose(host);
    host.get_json(&format!("{base}{path}"), ap)
}

fn gf_post(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    let base = gf_base(host)?;
    let ap = auth_purpose(host);
    host.send_json("POST", &format!("{base}{path}"), ap, body)
}

fn gf_delete(host: &mut Host, path: &str) -> Result<(), String> {
    let base = gf_base(host)?;
    let ap = auth_purpose(host);
    let resp = host.http("DELETE", &format!("{base}{path}"), ap, &[], None)?;
    if !resp.is_success() {
        return Err(format!("DELETE {path} → {} {}", resp.status, resp.body));
    }
    Ok(())
}

/// `/api/datasources/proxy/uid/{uid}/{path}` — the proxy path for all backend ops.
fn proxy_path(uid: &str, backend_path: &str) -> String {
    let backend_path = backend_path.trim_start_matches('/');
    format!("/api/datasources/proxy/uid/{uid}/{backend_path}")
}

/// Unwrap `{status,data}` if present, else return the value as-is.
fn unwrap_data(v: Value) -> Value {
    if let Some(data) = v.get("data") {
        data.clone()
    } else {
        v
    }
}

// ---------------------------------------------------------------------------
// Datasource resolution helpers
// ---------------------------------------------------------------------------

fn fetch_datasources(host: &mut Host) -> Result<Vec<Value>, String> {
    let arr = gf_get(host, "/api/datasources")?;
    arr.as_array()
        .cloned()
        .ok_or_else(|| "datasources not an array".into())
}

fn normalize_type(t: &str) -> &str {
    match t.to_lowercase().trim() {
        "prom" | "prometheus" => "prometheus",
        "loki" => "loki",
        "alertmanager" | "alertmanagerng" => "alertmanager",
        "tempo" => "tempo",
        _ => t,
    }
}

fn ds_type(ds: &Value) -> String {
    normalize_type(ds.get("type").and_then(|v| v.as_str()).unwrap_or("")).to_string()
}

fn ds_uid(ds: &Value) -> &str {
    ds.get("uid").and_then(|v| v.as_str()).unwrap_or("")
}

/// Derive a cluster alias from `type-cluster` UID convention (e.g. "loki-prod" → "prod").
fn cluster_from_uid(typ: &str, uid: &str) -> String {
    let uid = uid.to_lowercase();
    if uid == typ {
        return "infra".into();
    }
    let prefix = format!("{typ}-");
    if uid.starts_with(&prefix) {
        return uid[prefix.len()..].to_string();
    }
    String::new()
}

/// Resolve a datasource UID for `typ` + optional `cluster`.
/// If `explicit_uid` is non-empty, that wins immediately.
fn resolve_uid(
    datasources: &[Value],
    typ: &str,
    cluster: &str,
    explicit_uid: &str,
) -> Result<String, String> {
    let explicit_uid = explicit_uid.trim();
    if !explicit_uid.is_empty() {
        return Ok(explicit_uid.to_string());
    }
    let typ = normalize_type(typ);
    let cluster = cluster.to_lowercase();
    let matches: Vec<&Value> = datasources
        .iter()
        .filter(|ds| ds_type(ds) == typ)
        .filter(|ds| {
            if cluster.is_empty() {
                return true;
            }
            let uid = ds_uid(ds).to_lowercase();
            let cl = cluster_from_uid(typ, &uid);
            cl == cluster
                || uid.contains(&cluster)
                || ds
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&cluster)
        })
        .collect();
    match matches.len() {
        0 => Err(format!(
            "no {typ} datasource found for cluster {cluster:?}; pass uid explicitly"
        )),
        1 => Ok(ds_uid(matches[0]).to_string()),
        _ if cluster.is_empty() => Ok(ds_uid(matches[0]).to_string()),
        _ => Err(format!(
            "multiple {typ} datasources match {cluster:?}; pass uid explicitly"
        )),
    }
}

// ---------------------------------------------------------------------------
// Input helpers
// ---------------------------------------------------------------------------

fn flex_str(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn flex_i64(input: &Value, key: &str) -> Option<i64> {
    match input.get(key) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

fn flex_arr(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Time parsing (RFC3339 | unix-seconds/millis | "1h"/"30m" ago | "0s" = now)
// ---------------------------------------------------------------------------

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse a time string to unix seconds. Duration strings are treated as "ago from now" (e.g. "1h" = now - 1h).
fn parse_time(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(now_unix());
    }
    let s_strip = s.strip_suffix("ago").map(|x| x.trim()).unwrap_or(s);
    if let Ok(d) = parse_duration(s_strip) {
        return Ok(now_unix() - d);
    }
    // RFC3339
    if let Ok(t) = parse_rfc3339(s) {
        return Ok(t);
    }
    // Unix number
    if let Ok(n) = s.parse::<i64>() {
        if n > 1_000_000_000_000 {
            return Ok(n / 1000); // millis
        }
        return Ok(n);
    }
    Err(format!("invalid time {s:?}"))
}

/// Parse a duration string like "30m", "2h", "1h30m" to seconds.
fn parse_duration(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s == "0" || s == "0s" {
        return Ok(0);
    }
    let mut total: i64 = 0;
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            let n: i64 = cur.parse().map_err(|_| format!("bad duration {s:?}"))?;
            cur.clear();
            let mult = match c {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                _ => return Err(format!("bad duration unit {c:?}")),
            };
            total += n * mult;
        }
    }
    if !cur.is_empty() {
        return Err(format!("bad duration {s:?}"));
    }
    Ok(total)
}

/// Parse RFC3339 to unix seconds (minimal impl).
fn parse_rfc3339(s: &str) -> Result<i64, String> {
    // Use time crate if available; we fall back to a regex-free approach using chrono-style.
    // Since this plugin only has serde_json as an explicit dep, we do a manual parse.
    // Format: 2006-01-02T15:04:05Z or 2006-01-02T15:04:05+07:00
    // We'll use std parsing through SystemTime via a simple numeric approach.
    // Simpler: delegate to the `time` crate re-exported via host_kit if present, else manual.

    // Manual parse of fixed-format RFC3339.
    let s = s.trim();
    if s.len() < 20 {
        return Err(format!("not RFC3339: {s:?}"));
    }
    let year: i64 = s[0..4].parse().map_err(|_| format!("bad year in {s:?}"))?;
    let month: i64 = s[5..7].parse().map_err(|_| format!("bad month in {s:?}"))?;
    let day: i64 = s[8..10].parse().map_err(|_| format!("bad day in {s:?}"))?;
    let hour: i64 = s[11..13]
        .parse()
        .map_err(|_| format!("bad hour in {s:?}"))?;
    let min: i64 = s[14..16].parse().map_err(|_| format!("bad min in {s:?}"))?;
    let sec: i64 = s[17..19].parse().map_err(|_| format!("bad sec in {s:?}"))?;
    // Offset: Z or +HH:MM or -HH:MM
    let offset_secs: i64 = if s.len() >= 20 {
        let tail = &s[19..];
        // skip fraction
        let tail = tail.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
        if tail == "Z" || tail.is_empty() {
            0
        } else if tail.starts_with('+') || tail.starts_with('-') {
            let sign: i64 = if tail.starts_with('+') { 1 } else { -1 };
            let t = &tail[1..];
            let oh: i64 = t.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0);
            let om: i64 = t.get(3..5).and_then(|x| x.parse().ok()).unwrap_or(0);
            sign * (oh * 3600 + om * 60)
        } else {
            0
        }
    } else {
        0
    };
    // Days since epoch, ignoring leap seconds (good enough for grafana time windows).
    let days = days_since_epoch(year, month, day);
    Ok(days * 86400 + hour * 3600 + min * 60 + sec - offset_secs)
}

fn days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    // Rata Die algorithm
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 12 } else { month };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn unix_ms_to_rfc3339(ms: i64) -> String {
    unix_to_rfc3339(ms / 1000)
}

fn unix_to_rfc3339(ts: i64) -> String {
    // Simple UTC ISO-8601 from unix seconds.
    let mut rem = ts;
    let secs = rem % 60;
    rem /= 60;
    let mins = rem % 60;
    rem /= 60;
    let hrs = rem % 24;
    let days = rem / 24;
    // Convert days since epoch (1970-01-01) to y/m/d.
    let (y, mo, d) = days_from_civil(days);
    format!("{y:04}-{mo:02}-{d:02}T{hrs:02}:{mins:02}:{secs:02}Z")
}

fn days_from_civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// URL query-string builder
// ---------------------------------------------------------------------------

fn qs(pairs: &[(&str, String)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect();
    parts.join("&")
}

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

// ---------------------------------------------------------------------------
// Log-entry ID (sha1 of labels+ts+line, first 8 bytes as hex, same as fluxplane)
// ---------------------------------------------------------------------------

fn log_entry_id(labels: &HashMap<String, String>, ts: &str, line: &str) -> String {
    let mut keys: Vec<&str> = labels.keys().map(|s| s.as_str()).collect();
    keys.sort_unstable();
    let label_str: String = keys
        .iter()
        .map(|k| format!("{k}={}", labels[*k]))
        .collect::<Vec<_>>()
        .join(",");
    let mut h = Sha1::new();
    h.update(format!("{label_str}\x00{ts}\x00{line}"));
    let sum = h.finalize();
    hex::encode(&sum[..8])
}

// ---------------------------------------------------------------------------
// Loki parsing
// ---------------------------------------------------------------------------

const MAX_SERIES: usize = 200;
const MAX_POINTS: usize = 500;

fn parse_loki_response(
    base_url: &str,
    uid: &str,
    cluster: &str,
    query: &str,
    limit: i64,
    raw: &Value,
) -> Value {
    let limit = if limit <= 0 { 100 } else { limit as usize };

    // Check if it's a wrapped {status,data} response (Loki returns this directly)
    let data = raw.get("data").unwrap_or(raw);
    let result_type = data
        .get("resultType")
        .or_else(|| data.get("result_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();

    if result_type != "streams" && !result_type.is_empty() {
        // Metric query — reuse Prometheus parser
        if let Some(result) = data.get("result") {
            let (samples, series, truncated) = parse_promql_data(&result_type, result);
            let count = samples.len() + series.len();
            return json!({
                "url": base_url, "uid": uid, "cluster": cluster,
                "normalized_query": query, "result_type": result_type,
                "entries": [], "samples": samples, "series": series,
                "count": count, "limit": limit, "truncated": truncated
            });
        }
    }

    let mut entries: Vec<Value> = Vec::new();
    if let Some(result) = data.get("result").and_then(|v| v.as_array()) {
        for stream in result {
            let labels: HashMap<String, String> = stream
                .get("stream")
                .and_then(|v| v.as_object())
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(values) = stream.get("values").and_then(|v| v.as_array()) {
                for pair in values {
                    if let Some(arr) = pair.as_array() {
                        if arr.len() >= 2 {
                            let ts_raw = arr[0].as_str().unwrap_or("");
                            let line = arr[1].as_str().unwrap_or("");
                            let ts_ns: i64 = ts_raw.parse().unwrap_or(0);
                            let ts_rfc = unix_to_rfc3339(ts_ns / 1_000_000_000);
                            let id = log_entry_id(&labels, ts_raw, line);
                            let labels_json: Value = Value::Object(
                                labels
                                    .iter()
                                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                                    .collect(),
                            );
                            entries.push(json!({
                                "id": id, "timestamp": ts_rfc,
                                "labels": labels_json, "line": line
                            }));
                        }
                    }
                }
            }
        }
    }
    // Sort newest first
    entries.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });
    let truncated = entries.len() >= limit;
    let count = entries.len();
    json!({
        "url": base_url, "uid": uid, "cluster": cluster,
        "normalized_query": query, "result_type": "streams",
        "entries": entries, "samples": [], "series": [],
        "count": count, "limit": limit, "truncated": truncated
    })
}

// ---------------------------------------------------------------------------
// Prometheus parsing
// ---------------------------------------------------------------------------

fn parse_promql_data(result_type: &str, result: &Value) -> (Vec<Value>, Vec<Value>, bool) {
    let mut truncated = false;
    match result_type {
        "vector" => {
            let mut samples = Vec::new();
            if let Some(arr) = result.as_array() {
                for item in arr {
                    if let (Some(metric), Some(value)) = (item.get("metric"), item.get("value")) {
                        if let Some(point) = sample_point_from_pair(value) {
                            samples.push(
                                json!({ "metric": metric, "timestamp": point.0, "value": point.1 }),
                            );
                        }
                    }
                }
            }
            if samples.len() > MAX_SERIES {
                samples.truncate(MAX_SERIES);
                truncated = true;
            }
            (samples, vec![], truncated)
        }
        "matrix" => {
            let mut series = Vec::new();
            if let Some(arr) = result.as_array() {
                for item in arr {
                    let metric = item.get("metric").cloned().unwrap_or(json!({}));
                    let values_raw: &[Value] = item
                        .get("values")
                        .and_then(|v| v.as_array())
                        .map(|a| a.as_slice())
                        .unwrap_or_default();
                    let point_count = values_raw.len();
                    let values = if values_raw.len() > MAX_POINTS {
                        truncated = true;
                        &values_raw[values_raw.len() - MAX_POINTS..]
                    } else {
                        values_raw
                    };
                    let points: Vec<Value> = values
                        .iter()
                        .filter_map(sample_point_from_pair)
                        .map(|(ts, v)| json!({ "timestamp": ts, "value": v }))
                        .collect();
                    let series_truncated = point_count > MAX_POINTS;
                    series.push(json!({
                        "metric": metric, "points": points,
                        "point_count": point_count, "truncated": series_truncated
                    }));
                }
            }
            if series.len() > MAX_SERIES {
                series.truncate(MAX_SERIES);
                truncated = true;
            }
            (vec![], series, truncated)
        }
        "scalar" | "string" => {
            if let Some(point) = sample_point_from_pair(result) {
                (
                    vec![json!({ "metric": {}, "timestamp": point.0, "value": point.1 })],
                    vec![],
                    false,
                )
            } else {
                (vec![], vec![], false)
            }
        }
        _ => (vec![], vec![], false),
    }
}

fn sample_point_from_pair(pair: &Value) -> Option<(String, String)> {
    let arr = pair.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    let ts = match &arr[0] {
        Value::Number(n) => {
            let f = n.as_f64()?;
            unix_to_rfc3339(f as i64)
        }
        Value::String(s) => {
            if let Ok(f) = s.parse::<f64>() {
                unix_to_rfc3339(f as i64)
            } else {
                s.clone()
            }
        }
        _ => return None,
    };
    let val = match &arr[1] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    Some((ts, val))
}

fn parse_prom_rules(data: &Value) -> Value {
    let mut groups = Vec::new();
    let mut rule_count = 0usize;
    if let Some(gs) = data.get("groups").and_then(|v| v.as_array()) {
        for g in gs {
            let name = g
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let file = g
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let interval = g.get("interval").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let mut rules = Vec::new();
            if let Some(rs) = g.get("rules").and_then(|v| v.as_array()) {
                for r in rs {
                    rule_count += 1;
                    let active_count = r
                        .get("alerts")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    rules.push(json!({
                        "name": r.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "type": r.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                        "query": r.get("query").and_then(|v| v.as_str()).unwrap_or(""),
                        "state": r.get("state").and_then(|v| v.as_str()).unwrap_or(""),
                        "for": format_seconds(r.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0)),
                        "labels": r.get("labels").cloned().unwrap_or(json!({})),
                        "annotations": r.get("annotations").cloned().unwrap_or(json!({})),
                        "health": r.get("health").and_then(|v| v.as_str()).unwrap_or(""),
                        "last_error": r.get("lastError").and_then(|v| v.as_str()).unwrap_or(""),
                        "active_count": active_count
                    }));
                }
            }
            groups.push(json!({
                "name": name, "file": file,
                "interval": format_seconds(interval),
                "rules": rules
            }));
        }
    }
    json!({ "groups": groups, "group_count": groups.len(), "rule_count": rule_count })
}

fn format_seconds(s: f64) -> String {
    if s == 0.0 {
        return String::new();
    }
    let total = s as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let sec = total % 60;
    if h > 0 {
        format!("{h}h{m}m{sec}s")
    } else if m > 0 {
        format!("{m}m{sec}s")
    } else {
        format!("{sec}s")
    }
}

// ---------------------------------------------------------------------------
// Dashboard parsing
// ---------------------------------------------------------------------------

fn extract_dashboard_panels(
    panels_raw: &[Value],
    parent_path: Vec<String>,
) -> (Vec<Value>, Vec<Value>) {
    let mut panels = Vec::new();
    let mut queries = Vec::new();
    for raw_panel in panels_raw {
        let id = raw_panel.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let title = raw_panel
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let typ = raw_panel
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut path = parent_path.clone();
        path.push(title.clone());
        let ds_val = raw_panel.get("datasource").cloned().unwrap_or(Value::Null);
        let panel_ds = target_from_value(&ds_val);

        let mut panel_targets = Vec::new();
        if let Some(targets) = raw_panel.get("targets").and_then(|v| v.as_array()) {
            for t in targets {
                let q = extract_query(id, &title, &panel_ds, t);
                let expr = q
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let qtext = q
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !expr.is_empty() || !qtext.is_empty() {
                    panel_targets.push(q.clone());
                    queries.push(q);
                }
            }
        }
        if !panel_targets.is_empty() || typ != "row" {
            panels.push(json!({
                "id": id, "title": title, "type": typ,
                "datasource": panel_ds, "targets": panel_targets, "panel_path": path
            }));
        }
        // Recurse into row-embedded panels
        if let Some(sub_panels) = raw_panel.get("panels").and_then(|v| v.as_array()) {
            let (sub_p, sub_q) = extract_dashboard_panels(sub_panels, path.clone());
            panels.extend(sub_p);
            queries.extend(sub_q);
        }
    }
    (panels, queries)
}

fn extract_query(panel_id: i64, panel_title: &str, panel_ds: &Value, t: &Value) -> Value {
    let ref_id = t
        .get("refId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let target_ds_val = t.get("datasource").cloned().unwrap_or(Value::Null);
    let target_ds = target_from_value(&target_ds_val);
    let ds = if target_ds.get("type").and_then(|v| v.as_str()).is_some()
        || target_ds.get("uid").and_then(|v| v.as_str()).is_some()
    {
        target_ds
    } else {
        panel_ds.clone()
    };
    let expr = t
        .get("expr")
        .or_else(|| t.get("expression"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let query_text = t
        .get("query")
        .or_else(|| t.get("queryText"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let query_type = t
        .get("queryType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // Loki: query = expr
    let query_final = if query_text.is_empty()
        && !expr.is_empty()
        && ds.get("type").and_then(|v| v.as_str()) == Some("loki")
    {
        expr.clone()
    } else {
        query_text
    };
    json!({
        "panel_id": panel_id, "panel_title": panel_title,
        "ref_id": ref_id, "datasource": ds,
        "expression": expr, "query": query_final, "query_type": query_type,
        "raw": t
    })
}

fn target_from_value(v: &Value) -> Value {
    match v {
        Value::String(s) => json!({ "type": "", "uid": s }),
        Value::Object(m) => {
            let typ = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let uid = m.get("uid").and_then(|v| v.as_str()).unwrap_or("");
            json!({ "type": normalize_type(typ), "uid": uid })
        }
        _ => json!({ "type": "", "uid": "" }),
    }
}

// ---------------------------------------------------------------------------
// Alertmanager parsing
// ---------------------------------------------------------------------------

fn parse_am_alerts(raw: &Value) -> Vec<Value> {
    let arr = match raw.as_array() {
        Some(a) => a,
        None => return vec![],
    };
    arr.iter()
        .map(|a| {
            let labels: Value = a.get("labels").cloned().unwrap_or(json!({}));
            let annotations: Value = a.get("annotations").cloned().unwrap_or(json!({}));
            let status = a.get("status").cloned().unwrap_or(json!({}));
            json!({
                "name": labels.get("alertname").and_then(|v| v.as_str()).unwrap_or(""),
                "state": status.get("state").and_then(|v| v.as_str()).unwrap_or(""),
                "severity": labels.get("severity").and_then(|v| v.as_str()).unwrap_or(""),
                "starts_at": a.get("startsAt").and_then(|v| v.as_str()).unwrap_or(""),
                "ends_at": a.get("endsAt").and_then(|v| v.as_str()).unwrap_or(""),
                "silenced_by": status.get("silencedBy").cloned().unwrap_or(json!([])),
                "inhibited_by": status.get("inhibitedBy").cloned().unwrap_or(json!([])),
                "fingerprint": a.get("fingerprint").and_then(|v| v.as_str()).unwrap_or(""),
                "generator_url": a.get("generatorURL").and_then(|v| v.as_str()).unwrap_or(""),
                "labels": labels,
                "annotations": annotations
            })
        })
        .collect()
}

fn parse_silences(raw: &Value) -> Vec<Value> {
    let arr = match raw.as_array() {
        Some(a) => a,
        None => return vec![],
    };
    arr.iter().map(|s| {
        let matchers: Vec<Value> = s.get("matchers").and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(|m| json!({
                "name": m.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "value": m.get("value").and_then(|v| v.as_str()).unwrap_or(""),
                "is_regex": m.get("isRegex").and_then(|v| v.as_bool()).unwrap_or(false),
                "is_equal": m.get("isEqual").cloned()
            })).collect())
            .unwrap_or_default();
        json!({
            "id": s.get("id").and_then(|v| v.as_str()).unwrap_or(""),
            "state": s.get("status").and_then(|v| v.get("state")).and_then(|v| v.as_str()).unwrap_or(""),
            "matchers": matchers,
            "starts_at": s.get("startsAt").and_then(|v| v.as_str()).unwrap_or(""),
            "ends_at": s.get("endsAt").and_then(|v| v.as_str()).unwrap_or(""),
            "created_by": s.get("createdBy").and_then(|v| v.as_str()).unwrap_or(""),
            "comment": s.get("comment").and_then(|v| v.as_str()).unwrap_or("")
        })
    }).collect()
}

// ---------------------------------------------------------------------------
// Tempo parsing
// ---------------------------------------------------------------------------

const MAX_SPANS: usize = 200;

fn parse_tempo_search(data: &Value) -> Vec<Value> {
    data.get("traces")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(|t| {
            let trace_id = t.get("traceID").or_else(|| t.get("traceId")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let start_ns = t.get("startTimeUnixNano").and_then(|v| v.as_str()).unwrap_or("");
            let start_time = if !start_ns.is_empty() {
                let ns: i64 = start_ns.parse().unwrap_or(0);
                unix_to_rfc3339(ns / 1_000_000_000)
            } else { String::new() };
            let dur_ms = t.get("durationMs").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
            json!({
                "trace_id": trace_id,
                "root_service_name": t.get("rootServiceName").and_then(|v| v.as_str()).unwrap_or(""),
                "root_trace_name": t.get("rootTraceName").and_then(|v| v.as_str()).unwrap_or(""),
                "start_time": start_time,
                "duration_ms": dur_ms
            })
        }).collect())
        .unwrap_or_default()
}

fn parse_tempo_trace(data: &Value) -> (Vec<Value>, Vec<String>, String, i64, bool) {
    let batches = data
        .get("batches")
        .or_else(|| data.get("resourceSpans"))
        .and_then(|v| v.as_array());
    let batches = match batches {
        Some(b) => b,
        None => return (vec![], vec![], String::new(), 0, false),
    };
    let mut service_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut spans: Vec<Value> = Vec::new();
    for batch in batches {
        let service = batch
            .get("resource")
            .and_then(|r| r.get("attributes"))
            .and_then(|a| a.as_array())
            .and_then(|attrs| {
                attrs
                    .iter()
                    .find(|a| a.get("key").and_then(|v| v.as_str()) == Some("service.name"))
            })
            .and_then(|a| a.get("value"))
            .and_then(|v| v.get("stringValue"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !service.is_empty() {
            service_set.insert(service.clone());
        }
        let scope_spans = batch
            .get("scopeSpans")
            .or_else(|| batch.get("instrumentationLibrarySpans"))
            .and_then(|v| v.as_array());
        if let Some(scope_spans) = scope_spans {
            for scope in scope_spans {
                if let Some(span_arr) = scope.get("spans").and_then(|v| v.as_array()) {
                    for span in span_arr {
                        let span_id = span
                            .get("spanId")
                            .or_else(|| span.get("spanID"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let parent_id = span
                            .get("parentSpanId")
                            .or_else(|| span.get("parentSpanID"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = span
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let start_ns: i64 = span
                            .get("startTimeUnixNano")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        let end_ns: i64 = span
                            .get("endTimeUnixNano")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        let dur_ms = if end_ns > start_ns && start_ns > 0 {
                            (end_ns - start_ns) / 1_000_000
                        } else {
                            0
                        };
                        let start_time = if start_ns > 0 {
                            unix_to_rfc3339(start_ns / 1_000_000_000)
                        } else {
                            String::new()
                        };
                        let status_code = span
                            .get("status")
                            .and_then(|s| s.get("code"))
                            .map(|c| match c {
                                Value::Number(n) => match n.as_i64().unwrap_or(0) {
                                    0 => "unset",
                                    1 => "ok",
                                    2 => "error",
                                    _ => "unset",
                                }
                                .to_string(),
                                Value::String(s) => match s.as_str() {
                                    "STATUS_CODE_OK" => "ok".to_string(),
                                    "STATUS_CODE_ERROR" => "error".to_string(),
                                    _ => "unset".to_string(),
                                },
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        spans.push(json!({
                            "span_id": span_id,
                            "parent_span_id": parent_id,
                            "service": service,
                            "name": name,
                            "start_time": start_time,
                            "duration_ms": dur_ms,
                            "status_code": status_code
                        }));
                    }
                }
            }
        }
    }
    // Sort: roots (no parent) first, then by start_time
    spans.sort_by(|a, b| {
        let a_root = a
            .get("parent_span_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty();
        let b_root = b
            .get("parent_span_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty();
        if a_root != b_root {
            return if a_root {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        let ta = a.get("start_time").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("start_time").and_then(|v| v.as_str()).unwrap_or("");
        ta.cmp(tb)
    });
    let root_span = spans
        .first()
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let duration_ms = spans
        .first()
        .and_then(|s| s.get("duration_ms"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let truncated = spans.len() > MAX_SPANS;
    if truncated {
        spans.truncate(MAX_SPANS);
    }
    let mut services: Vec<String> = service_set.into_iter().collect();
    services.sort();
    (spans, services, root_span, duration_ms, truncated)
}

// ---------------------------------------------------------------------------
// Contribute helpers
// ---------------------------------------------------------------------------

fn contribute_dashboards(host: &mut Host, base_url: &str, dashboards: &[Value]) {
    let records: Vec<Record> = dashboards
        .iter()
        .filter_map(|d| {
            let uid = d
                .get("uid")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            let title = d.get("title").and_then(|v| v.as_str()).unwrap_or(uid);
            Some(Record::new(
                Source::new(base_url),
                "grafana.dashboard",
                uid,
                title,
                d.to_string(),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_annotations(host: &mut Host, base_url: &str, annotations: &[Value]) {
    let records: Vec<Record> = annotations
        .iter()
        .filter_map(|a| {
            let id = a.get("id").and_then(|v| v.as_i64())?;
            let text = a
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("annotation");
            Some(Record::new(
                Source::new(base_url),
                "grafana.annotation",
                &id.to_string() as &str,
                text,
                a.to_string(),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

// ---------------------------------------------------------------------------
// Op handlers
// ---------------------------------------------------------------------------

fn op_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let mut healthy = false;
    let mut version = String::new();
    let mut database = String::new();
    let mut health_error = String::new();
    let mut authenticated = false;
    let mut org_name = String::new();
    let mut auth_error = String::new();
    let mut hint = String::new();

    // Unauthenticated health probe
    match host.http("GET", &format!("{base}/api/health"), None, &[], None) {
        Ok(resp) if resp.is_success() => {
            if let Ok(v) = resp.json() {
                database = v
                    .get("database")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                version = v
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                healthy = database.to_lowercase() == "ok";
            }
        }
        Ok(resp) => {
            health_error = format!("HTTP {} {}", resp.status, resp.body);
            hint = "Grafana unreachable — check GRAFANA_URL".into();
        }
        Err(e) => {
            health_error = e.clone();
            hint = "Grafana unreachable — check GRAFANA_URL".into();
        }
    }

    // Authenticated /api/org probe
    let ap = auth_purpose(host);
    match host.http("GET", &format!("{base}/api/org"), ap, &[], None) {
        Ok(resp) if resp.is_success() => {
            if let Ok(v) = resp.json() {
                org_name = v
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                authenticated = true;
            }
        }
        Ok(resp) => {
            auth_error = format!("HTTP {} {}", resp.status, resp.body);
            if hint.is_empty() {
                hint = "instance healthy but credentials probe failed — check GRAFANA_API_TOKEN or GRAFANA_USERNAME/GRAFANA_PASSWORD".into();
            }
        }
        Err(e) => {
            auth_error = e;
            if hint.is_empty() {
                hint = "instance healthy but credentials probe failed — check GRAFANA_API_TOKEN or GRAFANA_USERNAME/GRAFANA_PASSWORD".into();
            }
        }
    }

    Ok(json!({
        "url": base, "healthy": healthy, "version": version, "database": database,
        "authenticated": authenticated, "org_name": org_name,
        "health_error": health_error, "auth_error": auth_error, "hint": hint
    }))
}

fn op_datasource_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let datasources = fetch_datasources(host)?;
    // Build clusters and types maps
    let mut clusters: Map<String, Value> = Map::new();
    let mut types: Map<String, Value> = Map::new();
    for ds in &datasources {
        let typ = ds_type(ds);
        let uid = ds_uid(ds).to_string();
        if !typ.is_empty() && !uid.is_empty() {
            types
                .entry(typ.clone())
                .or_insert(json!([]))
                .as_array_mut()
                .unwrap()
                .push(json!(uid));
        }
        let cluster = cluster_from_uid(&typ, &uid);
        if !cluster.is_empty() && !typ.is_empty() && !uid.is_empty() {
            let entry = clusters.entry(cluster).or_insert(json!({}));
            entry.as_object_mut().unwrap().insert(typ, json!(uid));
        }
    }
    let count = datasources.len();
    let ds_with_cluster: Vec<Value> = datasources
        .into_iter()
        .map(|mut d| {
            let typ = d
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let uid = d
                .get("uid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cluster = cluster_from_uid(normalize_type(&typ), &uid);
            if let Some(obj) = d.as_object_mut() {
                obj.insert("cluster".into(), json!(cluster));
            }
            d
        })
        .collect();
    Ok(json!({
        "url": base, "count": count,
        "datasources": ds_with_cluster,
        "clusters": clusters, "types": types
    }))
}

fn op_datasource_health(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let uid = flex_str(&input, "uid").ok_or("`uid` required")?;
    let path = format!("/api/datasources/uid/{uid}/health");
    match gf_get(host, &path) {
        Ok(v) => Ok(json!({
            "url": base, "uid": uid,
            "status": v.get("status").and_then(|v| v.as_str()).unwrap_or(""),
            "message": v.get("message").and_then(|v| v.as_str()).unwrap_or(""),
            "source": "datasource_health"
        })),
        Err(_) => {
            // Fallback: try alertmanager proxy status
            let proxy = proxy_path(&uid, "/api/v2/status");
            match gf_get(host, &proxy) {
                Ok(_) => Ok(json!({
                    "url": base, "uid": uid,
                    "status": "OK",
                    "message": "alertmanager status endpoint reachable",
                    "source": "alertmanager_status"
                })),
                Err(e) => Ok(json!({
                    "url": base, "uid": uid,
                    "status": "error", "error": e,
                    "source": "alertmanager_status"
                })),
            }
        }
    }
}

fn op_folder_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let limit = flex_i64(&input, "limit").unwrap_or(0);
    let path = if limit > 0 {
        format!("/api/folders?limit={limit}")
    } else {
        "/api/folders".to_string()
    };
    let folders = gf_get(host, &path)?;
    let arr = folders.as_array().cloned().unwrap_or_default();
    Ok(json!({ "url": base, "count": arr.len(), "folders": arr }))
}

fn op_dashboard_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let query = flex_str(&input, "query").unwrap_or_default();
    let folder_uid = flex_str(&input, "folder_uid").unwrap_or_default();
    let tags = flex_arr(&input, "tags");
    let limit = flex_i64(&input, "limit").unwrap_or(0);
    let mut pairs: Vec<(&str, String)> = vec![("type", "dash-db".into())];
    if !query.is_empty() {
        pairs.push(("query", query));
    }
    if !folder_uid.is_empty() {
        pairs.push(("folderUIDs", folder_uid));
    }
    for tag in &tags {
        pairs.push(("tag", tag.clone()));
    }
    if limit > 0 {
        pairs.push(("limit", limit.to_string()));
    }
    let q = qs(&pairs
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let path = if q.is_empty() {
        "/api/search?type=dash-db".to_string()
    } else {
        format!("/api/search?{q}")
    };
    let dashboards_raw = gf_get(host, &path)?;
    let dashboards: Vec<Value> = dashboards_raw.as_array().cloned().unwrap_or_default();
    contribute_dashboards(host, &base, &dashboards);
    Ok(json!({ "url": base, "count": dashboards.len(), "dashboards": dashboards }))
}

fn op_dashboard_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let uid = flex_str(&input, "uid").ok_or("`uid` required")?;
    let raw = gf_get(host, &format!("/api/dashboards/uid/{uid}"))?;
    let dashboard_raw = raw.get("dashboard").cloned().unwrap_or(raw.clone());
    let db_uid = dashboard_raw
        .get("uid")
        .and_then(|v| v.as_str())
        .unwrap_or(&uid)
        .to_string();
    let title = dashboard_raw
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let panels_raw: Vec<Value> = dashboard_raw
        .get("panels")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let (panels, queries) = extract_dashboard_panels(&panels_raw, vec![]);
    Ok(json!({
        "url": base, "uid": db_uid, "title": title,
        "panels": panels, "queries": queries,
        "dashboard": dashboard_raw
    }))
}

fn op_annotation_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let mut pairs: Vec<(&str, String)> = vec![];
    if let Some(since) = flex_str(&input, "since") {
        let t = parse_time(&since)?;
        pairs.push(("from", (t * 1000).to_string()));
    }
    if let Some(until) = flex_str(&input, "until") {
        let t = parse_time(&until)?;
        pairs.push(("to", (t * 1000).to_string()));
    }
    let tags = flex_arr(&input, "tags");
    for tag in &tags {
        pairs.push(("tags", tag.clone()));
    }
    if let Some(duid) = flex_str(&input, "dashboard_uid") {
        pairs.push(("dashboardUID", duid));
    }
    if let Some(lim) = flex_i64(&input, "limit") {
        pairs.push(("limit", lim.to_string()));
    }
    let q = qs(&pairs
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let path = if q.is_empty() {
        "/api/annotations".to_string()
    } else {
        format!("/api/annotations?{q}")
    };
    let raw = gf_get(host, &path)?;
    let annotations_wire = raw.as_array().cloned().unwrap_or_default();
    let annotations: Vec<Value> = annotations_wire.iter().map(|a| {
        let time_ms = a.get("time").and_then(|v| v.as_i64()).unwrap_or(0);
        let time_end_ms = a.get("timeEnd").and_then(|v| v.as_i64()).unwrap_or(0);
        json!({
            "id": a.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
            "time": if time_ms > 0 { unix_ms_to_rfc3339(time_ms) } else { String::new() },
            "time_end": if time_end_ms > 0 && time_end_ms != time_ms { unix_ms_to_rfc3339(time_end_ms) } else { String::new() },
            "text": a.get("text").and_then(|v| v.as_str()).unwrap_or(""),
            "tags": a.get("tags").cloned().unwrap_or(json!([])),
            "dashboard_uid": a.get("dashboardUID").and_then(|v| v.as_str()).unwrap_or(""),
            "panel_id": a.get("panelId").and_then(|v| v.as_i64()).unwrap_or(0)
        })
    }).collect();
    contribute_annotations(host, &base, &annotations);
    Ok(json!({ "url": base, "annotations": annotations, "count": annotations.len() }))
}

fn op_annotation_add(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let text = flex_str(&input, "text").ok_or("`text` required")?;
    let now = now_unix();
    let ann_time_ms = if let Some(t) = flex_str(&input, "time") {
        parse_time(&t)? * 1000
    } else {
        now * 1000
    };
    let mut body: Map<String, Value> = Map::new();
    body.insert("time".into(), json!(ann_time_ms));
    body.insert("text".into(), json!(text));
    if let Some(tags) = input.get("tags").and_then(|v| v.as_array()) {
        body.insert("tags".into(), json!(tags));
    }
    if let Some(time_end_str) = flex_str(&input, "time_end") {
        let end_ms = parse_time(&time_end_str)? * 1000;
        if end_ms <= ann_time_ms {
            return Err("time_end must be after time".into());
        }
        body.insert("timeEnd".into(), json!(end_ms));
        body.insert("isRegion".into(), json!(true));
    }
    if let Some(duid) = flex_str(&input, "dashboard_uid") {
        body.insert("dashboardUID".into(), json!(duid));
    }
    if let Some(pid) = flex_i64(&input, "panel_id") {
        if pid > 0 {
            body.insert("panelId".into(), json!(pid));
        }
    }
    let resp = gf_post(host, "/api/annotations", &Value::Object(body))?;
    Ok(json!({
        "url": base,
        "id": resp.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
        "message": resp.get("message").and_then(|v| v.as_str()).unwrap_or("")
    }))
}

fn op_loki_labels(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let label = flex_str(&input, "label").unwrap_or_default();
    let query = flex_str(&input, "query").unwrap_or_default();
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "loki", &cluster, &explicit_uid)?;
    let backend_path = if label.is_empty() {
        "/loki/api/v1/labels".to_string()
    } else {
        format!("/loki/api/v1/label/{}/values", urlencode(&label))
    };
    let mut qs_parts: Vec<(&str, String)> = vec![];
    if !query.is_empty() {
        qs_parts.push(("query", query.clone()));
    }
    let q = qs(&qs_parts
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let full_path = if q.is_empty() {
        proxy_path(&uid, &backend_path)
    } else {
        format!("{}?{}", proxy_path(&uid, &backend_path), q)
    };
    let raw = gf_get(host, &full_path)?;
    let data = unwrap_data(raw);
    let mut values: Vec<String> = data
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    values.sort();
    Ok(json!({
        "url": base, "uid": uid, "cluster": cluster, "label": label,
        "values": values
    }))
}

fn op_loki_query(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let query = flex_str(&input, "query").ok_or("`query` required")?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let since = flex_str(&input, "since").unwrap_or_else(|| "1h".into());
    let until = flex_str(&input, "until").unwrap_or_else(|| "0s".into());
    let limit = flex_i64(&input, "limit").unwrap_or(100);
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "loki", &cluster, &explicit_uid)?;
    let start = parse_time(&since)? * 1_000_000_000; // ns
    let end = parse_time(&until)? * 1_000_000_000;
    if start >= end {
        return Err("since must be before until".into());
    }
    let mut qs_parts = vec![
        ("query", query.clone()),
        ("start", start.to_string()),
        ("end", end.to_string()),
    ];
    if limit > 0 {
        qs_parts.push(("limit", limit.to_string()));
    }
    let q = qs(&qs_parts
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let full_path = format!("{}?{}", proxy_path(&uid, "/loki/api/v1/query_range"), q);
    let raw = gf_get(host, &full_path)?;
    Ok(parse_loki_response(
        &base, &uid, &cluster, &query, limit, &raw,
    ))
}

fn op_loki_recent_logs(input: Value, host: &mut Host) -> Result<Value, String> {
    let namespace = flex_str(&input, "namespace").unwrap_or_default();
    let app = flex_str(&input, "app").unwrap_or_default();
    let contains = flex_str(&input, "contains").unwrap_or_default();
    let mut selectors = Vec::new();
    if !namespace.is_empty() {
        selectors.push(format!("namespace=\"{}\"", escape_logql(&namespace)));
    }
    if !app.is_empty() {
        selectors.push(format!("app=~\"{}\"", escape_logql(&app)));
    }
    let mut query = format!("{{{}}}", selectors.join(","));
    if !contains.is_empty() {
        query.push_str(&format!(" |= \"{}\"", escape_logql(&contains)));
    }
    let mut new_input = input.as_object().cloned().unwrap_or_default();
    new_input.insert("query".into(), json!(query));
    op_loki_query(Value::Object(new_input), host)
}

fn escape_logql(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn op_prometheus_query(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let query = flex_str(&input, "query").ok_or("`query` required")?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "prometheus", &cluster, &explicit_uid)?;
    let mut qs_parts = vec![("query", query.clone())];
    if let Some(time_str) = flex_str(&input, "time") {
        let t = parse_time(&time_str)?;
        qs_parts.push(("time", t.to_string()));
    }
    let q = qs(&qs_parts
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let full_path = format!("{}?{}", proxy_path(&uid, "/api/v1/query"), q);
    let raw = gf_get(host, &full_path)?;
    let data = unwrap_data(raw);
    let result_type = data
        .get("resultType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let result = data.get("result").cloned().unwrap_or(json!([]));
    let (samples, series, truncated) = parse_promql_data(&result_type, &result);
    let count = samples.len() + series.len();
    Ok(json!({
        "url": base, "uid": uid, "cluster": cluster, "query": query,
        "result_type": result_type, "samples": samples, "series": series,
        "count": count, "truncated": truncated
    }))
}

fn op_prometheus_range(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let query = flex_str(&input, "query").ok_or("`query` required")?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "prometheus", &cluster, &explicit_uid)?;
    let since = flex_str(&input, "since").unwrap_or_else(|| "1h".into());
    let until = flex_str(&input, "until").unwrap_or_else(|| "0s".into());
    let step = flex_str(&input, "step").unwrap_or_else(|| "1m".into());
    let start = parse_time(&since)?;
    let end = parse_time(&until)?;
    if start >= end {
        return Err("since must be before until".into());
    }
    let qs_parts = [
        ("query", query.clone()),
        ("start", start.to_string()),
        ("end", end.to_string()),
        ("step", step),
    ];
    let q = qs(&qs_parts
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let full_path = format!("{}?{}", proxy_path(&uid, "/api/v1/query_range"), q);
    let raw = gf_get(host, &full_path)?;
    let data = unwrap_data(raw);
    let result_type = data
        .get("resultType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let result = data.get("result").cloned().unwrap_or(json!([]));
    let (samples, series, truncated) = parse_promql_data(&result_type, &result);
    let count = samples.len() + series.len();
    Ok(json!({
        "url": base, "uid": uid, "cluster": cluster, "query": query,
        "result_type": result_type, "samples": samples, "series": series,
        "count": count, "truncated": truncated
    }))
}

fn op_prometheus_rules(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "prometheus", &cluster, &explicit_uid)?;
    let mut qs_parts: Vec<(&str, String)> = vec![];
    if let Some(typ) = flex_str(&input, "type") {
        qs_parts.push(("type", typ));
    }
    let q = qs(&qs_parts
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let full_path = if q.is_empty() {
        proxy_path(&uid, "/api/v1/rules")
    } else {
        format!("{}?{}", proxy_path(&uid, "/api/v1/rules"), q)
    };
    let raw = gf_get(host, &full_path)?;
    let data = unwrap_data(raw);
    let parsed = parse_prom_rules(&data);
    let groups = parsed.get("groups").cloned().unwrap_or(json!([]));
    let group_count = parsed
        .get("group_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let rule_count = parsed
        .get("rule_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Ok(json!({
        "url": base, "uid": uid, "cluster": cluster,
        "groups": groups, "group_count": group_count, "rule_count": rule_count
    }))
}

fn op_alerts_active(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let severity = flex_str(&input, "severity").unwrap_or_default();
    let namespace = flex_str(&input, "namespace").unwrap_or_default();
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "alertmanager", &cluster, &explicit_uid)?;
    let full_path = format!(
        "{}?active=true&silenced=false&inhibited=false",
        proxy_path(&uid, "/api/v2/alerts")
    );
    let raw = gf_get(host, &full_path)?;
    let mut alerts = parse_am_alerts(&raw);
    if !severity.is_empty() {
        alerts.retain(|a| a.get("severity").and_then(|v| v.as_str()).unwrap_or("") == severity);
    }
    if !namespace.is_empty() {
        alerts.retain(|a| {
            a.get("labels")
                .and_then(|l| l.get("namespace"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                == namespace
        });
    }
    Ok(
        json!({ "url": base, "uid": uid, "cluster": cluster, "count": alerts.len(), "alerts": alerts }),
    )
}

fn op_alerts_silences_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let filters = flex_arr(&input, "filter");
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "alertmanager", &cluster, &explicit_uid)?;
    let mut qs_parts: Vec<(&str, String)> = vec![];
    for f in &filters {
        qs_parts.push(("filter", f.clone()));
    }
    let q = qs(&qs_parts
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let full_path = if q.is_empty() {
        proxy_path(&uid, "/api/v2/silences")
    } else {
        format!("{}?{}", proxy_path(&uid, "/api/v2/silences"), q)
    };
    let raw = gf_get(host, &full_path)?;
    let silences = parse_silences(&raw);
    Ok(
        json!({ "url": base, "uid": uid, "cluster": cluster, "silences": silences, "count": silences.len() }),
    )
}

fn op_alerts_silences_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let matchers_raw = input
        .get("matchers")
        .and_then(|v| v.as_array())
        .ok_or("`matchers` (array) required")?;
    if matchers_raw.is_empty() {
        return Err("`matchers` must be non-empty".into());
    }
    let ends_at_str = flex_str(&input, "ends_at").ok_or("`ends_at` required")?;
    let comment = flex_str(&input, "comment").ok_or("`comment` required")?;
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "alertmanager", &cluster, &explicit_uid)?;
    let now = now_unix();
    let starts_at = if let Some(s) = flex_str(&input, "starts_at") {
        parse_time(&s)?
    } else {
        now
    };
    // ends_at: try duration first (future), then parse_time (which treats duration as "ago")
    let ends_at = if let Ok(d) = parse_duration(&ends_at_str) {
        now + d
    } else {
        parse_time(&ends_at_str)?
    };
    if starts_at >= ends_at {
        return Err("ends_at must be after starts_at".into());
    }
    let matchers: Vec<Value> = matchers_raw
        .iter()
        .map(|m| {
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let value = m.get("value").and_then(|v| v.as_str()).unwrap_or("");
            let is_regex = m.get("is_regex").and_then(|v| v.as_bool()).unwrap_or(false);
            let is_equal = m.get("is_equal").and_then(|v| v.as_bool()).unwrap_or(true);
            json!({ "name": name, "value": value, "isRegex": is_regex, "isEqual": is_equal })
        })
        .collect();
    let created_by = flex_str(&input, "created_by").unwrap_or_else(|| "flux".into());
    let body = json!({
        "matchers": matchers,
        "startsAt": unix_to_rfc3339(starts_at),
        "endsAt": unix_to_rfc3339(ends_at),
        "createdBy": created_by,
        "comment": comment
    });
    let ap = auth_purpose(host);
    let base_url = gf_base(host)?;
    let full_path = proxy_path(&uid, "/api/v2/silences");
    let resp = host.send_json("POST", &format!("{base_url}{full_path}"), ap, &body)?;
    let silence_id = resp
        .get("silenceID")
        .or_else(|| resp.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(json!({ "url": base, "uid": uid, "cluster": cluster, "silence_id": silence_id }))
}

fn op_alerts_silences_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let cluster = flex_str(&input, "cluster").unwrap_or_default();
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let silence_id = flex_str(&input, "silence_id").ok_or("`silence_id` required")?;
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "alertmanager", &cluster, &explicit_uid)?;
    let path = proxy_path(&uid, &format!("/api/v2/silence/{}", urlencode(&silence_id)));
    gf_delete(host, &path)?;
    Ok(
        json!({ "url": base, "uid": uid, "cluster": cluster, "silence_id": silence_id, "deleted": true }),
    )
}

fn op_tempo_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "tempo", "", &explicit_uid)?;
    let query = flex_str(&input, "query").unwrap_or_default();
    let mut qs_parts: Vec<(&str, String)> = vec![];
    if !query.is_empty() {
        qs_parts.push(("q", query.clone()));
    }
    if let Some(since) = flex_str(&input, "since") {
        let t = parse_time(&since)?;
        qs_parts.push(("start", t.to_string()));
    }
    if let Some(until) = flex_str(&input, "until") {
        let t = parse_time(&until)?;
        qs_parts.push(("end", t.to_string()));
    }
    if let Some(limit) = flex_i64(&input, "limit") {
        if limit > 0 {
            qs_parts.push(("limit", limit.to_string()));
        }
    }
    let q = qs(&qs_parts
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect::<Vec<_>>());
    let full_path = if q.is_empty() {
        proxy_path(&uid, "/api/search")
    } else {
        format!("{}?{}", proxy_path(&uid, "/api/search"), q)
    };
    let raw = gf_get(host, &full_path)?;
    let data = unwrap_data(raw);
    let traces = parse_tempo_search(&data);
    Ok(json!({
        "url": base, "uid": uid, "query": query,
        "traces": traces, "count": traces.len()
    }))
}

fn op_tempo_trace_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = gf_base(host)?;
    let trace_id = flex_str(&input, "trace_id").ok_or("`trace_id` required")?;
    let explicit_uid = flex_str(&input, "uid").unwrap_or_default();
    let datasources = fetch_datasources(host)?;
    let uid = resolve_uid(&datasources, "tempo", "", &explicit_uid)?;
    let full_path = proxy_path(&uid, &format!("/api/traces/{}", urlencode(&trace_id)));
    let raw = gf_get(host, &full_path)?;
    let data = unwrap_data(raw);
    let (spans, services, root_span, duration_ms, truncated) = parse_tempo_trace(&data);
    Ok(json!({
        "url": base, "uid": uid, "trace_id": trace_id,
        "root_span": root_span, "services": services,
        "span_count": spans.len(), "duration_ms": duration_ms,
        "spans": spans, "truncated": truncated
    }))
}

fn main() {
    manifest_builder().serve();
}

// ---------------------------------------------------------------------------
// Tests — one MockHost test per op
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn base_host() -> MockHost {
        MockHost::default()
            .with_endpoint("grafana.endpoint", "https://grafana.example.com")
            .with_secret("api_token", "glsa_test")
    }

    // -- test --

    #[test]
    fn test_healthy_and_authenticated() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "/api/health",
                json!({ "database": "ok", "version": "10.0.0" }),
            )
            .with_http("/api/org", json!({ "name": "Acme" }));
        let v = plugin.call("grafana.test", json!({}), &mut host).unwrap();
        assert_eq!(v["healthy"], json!(true));
        assert_eq!(v["authenticated"], json!(true));
        assert_eq!(v["org_name"], json!("Acme"));
        assert_eq!(v["version"], json!("10.0.0"));
    }

    // -- datasource.list --

    #[test]
    fn datasource_list_builds_clusters() {
        let plugin = manifest_builder().build();
        let mut host = base_host().with_http(
            "/api/datasources",
            json!([
                { "uid": "loki-prod", "name": "Loki Prod", "type": "loki" },
                { "uid": "prometheus-prod", "name": "Prom Prod", "type": "prometheus" }
            ]),
        );
        let v = plugin
            .call("grafana.datasource.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(v["count"], json!(2));
        assert!(v["clusters"]["prod"]["loki"] == json!("loki-prod"));
    }

    // -- datasource.health --

    #[test]
    fn datasource_health_ok() {
        let plugin = manifest_builder().build();
        let mut host = base_host().with_http(
            "/api/datasources/uid/loki-prod/health",
            json!({ "status": "OK", "message": "healthy" }),
        );
        let v = plugin
            .call(
                "grafana.datasource.health",
                json!({ "uid": "loki-prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["status"], json!("OK"));
        assert_eq!(v["uid"], json!("loki-prod"));
    }

    // -- folder.list --

    #[test]
    fn folder_list_returns_folders() {
        let plugin = manifest_builder().build();
        let mut host = base_host().with_http(
            "/api/folders",
            json!([{ "uid": "f1", "title": "Production" }]),
        );
        let v = plugin
            .call("grafana.folder.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["folders"][0]["title"], json!("Production"));
    }

    // -- dashboard.list --

    #[test]
    fn dashboard_list_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = base_host().with_http(
            "/api/search",
            json!([{ "uid": "abc", "title": "API Overview" }]),
        );
        let v = plugin
            .call("grafana.dashboard.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["dashboards"][0]["title"], json!("API Overview"));
        assert_eq!(host.contributed.borrow().len(), 1);
    }

    // -- dashboard.get --

    #[test]
    fn dashboard_get_extracts_queries() {
        let plugin = manifest_builder().build();
        let mut host = base_host().with_http(
            "/api/dashboards/uid/abc",
            json!({
                "dashboard": {
                    "uid": "abc", "title": "My Dashboard",
                    "panels": [{
                        "id": 1, "title": "Error rate", "type": "graph",
                        "targets": [{ "refId": "A", "expr": "rate(errors[5m])", "datasource": { "type": "prometheus", "uid": "prom" } }]
                    }]
                }
            }),
        );
        let v = plugin
            .call("grafana.dashboard.get", json!({ "uid": "abc" }), &mut host)
            .unwrap();
        assert_eq!(v["title"], json!("My Dashboard"));
        assert_eq!(v["queries"][0]["expression"], json!("rate(errors[5m])"));
    }

    // -- annotation.list --

    #[test]
    fn annotation_list_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = base_host().with_http(
            "/api/annotations",
            json!([{ "id": 42, "time": 1700000000000i64, "timeEnd": 0, "text": "deploy", "tags": ["deploy"], "dashboardUID": "", "panelId": 0 }]),
        );
        let v = plugin
            .call("grafana.annotation.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["annotations"][0]["text"], json!("deploy"));
        assert_eq!(host.contributed.borrow().len(), 1);
    }

    // -- annotation.add --

    #[test]
    fn annotation_add_returns_id() {
        let plugin = manifest_builder().build();
        let mut host = base_host().with_http(
            "/api/annotations",
            json!({ "id": 99, "message": "Annotation added" }),
        );
        let v = plugin
            .call(
                "grafana.annotation.add",
                json!({ "text": "Released v1.0" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["id"], json!(99));
        assert_eq!(v["message"], json!("Annotation added"));
    }

    // -- loki.labels --

    #[test]
    fn loki_labels_via_proxy() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/loki-prod/loki/api/v1/labels",
                json!({ "status": "success", "data": ["app", "namespace"] }),
            )
            .with_http(
                "/api/datasources",
                json!([{ "uid": "loki-prod", "type": "loki" }]),
            );
        let v = plugin
            .call(
                "grafana.loki.labels",
                json!({ "cluster": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["uid"], json!("loki-prod"));
        assert!(v["values"].as_array().unwrap().contains(&json!("app")));
    }

    // -- loki.query --

    #[test]
    fn loki_query_parses_streams() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/loki-prod/loki/api/v1/query_range",
                json!({
                    "status": "success",
                    "data": {
                        "resultType": "streams",
                        "result": [{
                            "stream": { "app": "api" },
                            "values": [["1700000000000000000", "error occurred"]]
                        }]
                    }
                }),
            )
            .with_http(
                "/api/datasources",
                json!([{ "uid": "loki-prod", "type": "loki" }]),
            );
        let v = plugin
            .call(
                "grafana.loki.query",
                json!({ "cluster": "prod", "query": "{app=\"api\"} |= \"error\"" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["result_type"], json!("streams"));
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["entries"][0]["line"], json!("error occurred"));
    }

    // -- loki.recent_logs --

    #[test]
    fn loki_recent_logs_builds_query() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/loki-prod/loki/api/v1/query_range",
                json!({
                    "status": "success",
                    "data": { "resultType": "streams", "result": [] }
                }),
            )
            .with_http(
                "/api/datasources",
                json!([{ "uid": "loki-prod", "type": "loki" }]),
            );
        let v = plugin
            .call(
                "grafana.loki.recent_logs",
                json!({ "cluster": "prod", "app": "myapp", "namespace": "default" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["uid"], json!("loki-prod"));
        assert_eq!(v["count"], json!(0));
    }

    // -- prometheus.query --

    #[test]
    fn prometheus_query_parses_vector() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/prometheus-prod/api/v1/query",
                json!({
                    "status": "success",
                    "data": {
                        "resultType": "vector",
                        "result": [{ "metric": { "job": "api" }, "value": [1700000000.0, "0.5"] }]
                    }
                }),
            )
            .with_http(
                "/api/datasources",
                json!([{ "uid": "prometheus-prod", "type": "prometheus" }]),
            );
        let v = plugin
            .call(
                "grafana.prometheus.query",
                json!({ "cluster": "prod", "query": "up" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["result_type"], json!("vector"));
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["samples"][0]["value"], json!("0.5"));
    }

    // -- prometheus.range --

    #[test]
    fn prometheus_range_parses_matrix() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/prometheus-prod/api/v1/query_range",
                json!({
                    "status": "success",
                    "data": {
                        "resultType": "matrix",
                        "result": [{ "metric": { "job": "api" }, "values": [[1700000000.0, "1.0"]] }]
                    }
                }),
            )
            .with_http("/api/datasources", json!([{ "uid": "prometheus-prod", "type": "prometheus" }]));
        let v = plugin
            .call(
                "grafana.prometheus.range",
                json!({ "cluster": "prod", "query": "rate(requests[5m])" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["result_type"], json!("matrix"));
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["series"][0]["points"][0]["value"], json!("1.0"));
    }

    // -- prometheus.rules --

    #[test]
    fn prometheus_rules_parses_groups() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/prometheus-prod/api/v1/rules",
                json!({
                    "status": "success",
                    "data": {
                        "groups": [{
                            "name": "alerts", "file": "alerts.yaml", "interval": 60.0,
                            "rules": [{ "name": "HighError", "type": "alerting", "query": "rate(errors[5m]) > 0.1", "state": "firing", "duration": 300.0, "labels": {}, "annotations": {}, "health": "ok", "lastError": "", "alerts": [{}, {}] }]
                        }]
                    }
                }),
            )
            .with_http("/api/datasources", json!([{ "uid": "prometheus-prod", "type": "prometheus" }]));
        let v = plugin
            .call(
                "grafana.prometheus.rules",
                json!({ "cluster": "prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["group_count"], json!(1));
        assert_eq!(v["rule_count"], json!(1));
        assert_eq!(v["groups"][0]["rules"][0]["name"], json!("HighError"));
        assert_eq!(v["groups"][0]["rules"][0]["active_count"], json!(2));
    }

    // -- alerts.active --

    #[test]
    fn alerts_active_filters_by_severity() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/alertmanager-prod/api/v2/alerts",
                json!([
                    { "labels": { "alertname": "HighLoad", "severity": "warning" }, "annotations": {}, "startsAt": "2024-01-01T00:00:00Z", "endsAt": "", "fingerprint": "abc", "generatorURL": "", "status": { "state": "active", "silencedBy": [], "inhibitedBy": [] } },
                    { "labels": { "alertname": "Down", "severity": "critical" }, "annotations": {}, "startsAt": "2024-01-01T00:00:00Z", "endsAt": "", "fingerprint": "def", "generatorURL": "", "status": { "state": "active", "silencedBy": [], "inhibitedBy": [] } }
                ]),
            )
            .with_http("/api/datasources", json!([{ "uid": "alertmanager-prod", "type": "alertmanager" }]));
        let v = plugin
            .call(
                "grafana.alerts.active",
                json!({ "uid": "alertmanager-prod", "severity": "critical" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["alerts"][0]["name"], json!("Down"));
    }

    // -- alerts.silences.list --

    #[test]
    fn alerts_silences_list_parses_silences() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/alertmanager-prod/api/v2/silences",
                json!([{
                    "id": "s1", "status": { "state": "active" },
                    "matchers": [{ "name": "alertname", "value": "HighLoad", "isRegex": false, "isEqual": true }],
                    "startsAt": "2024-01-01T00:00:00Z", "endsAt": "2024-01-02T00:00:00Z",
                    "createdBy": "admin", "comment": "maintenance"
                }]),
            )
            .with_http("/api/datasources", json!([{ "uid": "alertmanager-prod", "type": "alertmanager" }]));
        let v = plugin
            .call(
                "grafana.alerts.silences.list",
                json!({ "uid": "alertmanager-prod" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["silences"][0]["id"], json!("s1"));
        assert_eq!(v["silences"][0]["comment"], json!("maintenance"));
    }

    // -- alerts.silences.create --

    #[test]
    fn alerts_silences_create_returns_id() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/alertmanager-prod/api/v2/silences",
                json!({ "silenceID": "new-silence-123" }),
            )
            .with_http(
                "/api/datasources",
                json!([{ "uid": "alertmanager-prod", "type": "alertmanager" }]),
            );
        let v = plugin
            .call(
                "grafana.alerts.silences.create",
                json!({
                    "uid": "alertmanager-prod",
                    "matchers": [{ "name": "alertname", "value": "HighLoad" }],
                    "ends_at": "2h",
                    "comment": "maintenance window"
                }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["silence_id"], json!("new-silence-123"));
    }

    // -- alerts.silences.delete --

    #[test]
    fn alerts_silences_delete_ok() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/alertmanager-prod/api/v2/silence",
                json!({}),
            )
            .with_http(
                "/api/datasources",
                json!([{ "uid": "alertmanager-prod", "type": "alertmanager" }]),
            );
        let v = plugin
            .call(
                "grafana.alerts.silences.delete",
                json!({ "uid": "alertmanager-prod", "silence_id": "s1" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["deleted"], json!(true));
        assert_eq!(v["silence_id"], json!("s1"));
    }

    // -- tempo.search --

    #[test]
    fn tempo_search_returns_traces() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/tempo/api/search",
                json!({
                    "traces": [{
                        "traceID": "abc123",
                        "rootServiceName": "api",
                        "rootTraceName": "GET /users",
                        "startTimeUnixNano": "1700000000000000000",
                        "durationMs": 42.5
                    }]
                }),
            )
            .with_http(
                "/api/datasources",
                json!([{ "uid": "tempo", "type": "tempo" }]),
            );
        let v = plugin
            .call("grafana.tempo.search", json!({}), &mut host)
            .unwrap();
        assert_eq!(v["count"], json!(1));
        assert_eq!(v["traces"][0]["trace_id"], json!("abc123"));
        assert_eq!(v["traces"][0]["root_service_name"], json!("api"));
        assert_eq!(v["traces"][0]["duration_ms"], json!(42));
    }

    // -- tempo.trace.get --

    #[test]
    fn tempo_trace_get_parses_spans() {
        let plugin = manifest_builder().build();
        let mut host = base_host()
            .with_http(
                "datasources/proxy/uid/tempo/api/traces/abc123",
                json!({
                    "batches": [{
                        "resource": { "attributes": [{ "key": "service.name", "value": { "stringValue": "api" } }] },
                        "scopeSpans": [{
                            "spans": [{
                                "spanId": "span1", "parentSpanId": "",
                                "name": "GET /users",
                                "startTimeUnixNano": "1700000000000000000",
                                "endTimeUnixNano": "1700000000100000000",
                                "status": { "code": 1 }
                            }]
                        }]
                    }]
                }),
            )
            .with_http("/api/datasources", json!([{ "uid": "tempo", "type": "tempo" }]));
        let v = plugin
            .call(
                "grafana.tempo.trace.get",
                json!({ "trace_id": "abc123" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(v["trace_id"], json!("abc123"));
        assert_eq!(v["span_count"], json!(1));
        assert_eq!(v["spans"][0]["service"], json!("api"));
        assert_eq!(v["spans"][0]["status_code"], json!("ok"));
        assert_eq!(v["root_span"], json!("GET /users"));
    }
}

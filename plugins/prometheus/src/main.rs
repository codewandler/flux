//! `prometheus` — a flux integration plugin for the Prometheus HTTP API (v1).

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};

// Schema-only op input structs (D-36).

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct TestInput {}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct QueryInput {
    query: String,
    time: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct QueryRangeInput {
    query: String,
    since: Option<String>,
    until: Option<String>,
    step: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct LabelsInput {
    label: Option<String>,
    #[serde(rename = "match")]
    r#match: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SeriesInput {
    #[serde(rename = "match")]
    r#match: Vec<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum TargetState {
    Active,
    Dropped,
    Any,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct TargetsInput {
    state: Option<TargetState>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum RuleType {
    Alert,
    Record,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct RulesInput {
    #[serde(rename = "type")]
    r#type: Option<RuleType>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct AlertsInput {}

const MAX_SERIES_PER_RESULT: usize = 200;
const MAX_POINTS_PER_SERIES: usize = 500;

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("prometheus", "0.1.0")
        .capabilities(Caps {
            http: true,
            private_hosts: vec!["*".into()],
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "prometheus.endpoint".into(),
            env: vec!["PROMETHEUS_URL".into(), "PROM_URL".into()],
            http_hosts: Vec::new(),
            description: "Prometheus base URL (e.g. https://prom.example.com)".into(),
        })
        .datasource(ds(
            "prometheus.query_results",
            "prometheus.query_result",
            "Prometheus instant/range query result series.",
        ))
        .datasource(ds(
            "prometheus.labels",
            "prometheus.label",
            "Prometheus label names or values.",
        ))
        .datasource(ds(
            "prometheus.targets",
            "prometheus.target",
            "Prometheus scrape targets and their health.",
        ))
        .datasource(ds(
            "prometheus.alerts",
            "prometheus.alert",
            "Prometheus active alerts.",
        ))
        .operation(read_op_typed::<TestInput>("prometheus.test", "Check whether the Prometheus endpoint is reachable and ready."), test)
        .operation(read_op_typed::<QueryInput>("prometheus.query", "Evaluate a PromQL expression at a single instant (optionally at `time`)."), query)
        .operation(read_op_typed::<QueryRangeInput>("prometheus.query_range", "Evaluate a PromQL expression over a time range at a fixed step."), query_range)
        .operation(read_op_typed::<LabelsInput>("prometheus.labels", "List label names, or the values of one `label`; narrow with `match` selectors."), labels)
        .operation(read_op_typed::<SeriesInput>("prometheus.series", "List the series (label sets) matching one or more PromQL selectors."), series)
        .operation(read_op_typed::<TargetsInput>("prometheus.targets", "List the scrape targets and their health (state: active|dropped|any)."), targets)
        .operation(read_op_typed::<RulesInput>("prometheus.rules", "List alerting and recording rules with state and health (type: alert|record to filter)."), rules)
        .operation(read_op_typed::<AlertsInput>("prometheus.alerts", "List the currently active alerts."), alerts)
}

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into()],
        entity_schema: None,
    }
}

fn base_url(host: &mut Host) -> Result<String, String> {
    Ok(host
        .endpoint("prometheus.endpoint")?
        .trim_end_matches('/')
        .to_string())
}

fn prom_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let base = base_url(host)?;
    host.get_json(&format!("{base}{path}"), None)
}

fn req_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}` (string) required"))
}

fn req_query(input: &Value) -> Result<&str, String> {
    let q = req_str(input, "query")?.trim();
    if q.is_empty() {
        Err("`query` is required".into())
    } else {
        Ok(q)
    }
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

fn match_selectors(input: &Value) -> Vec<String> {
    match input.get("match") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        _ => Vec::new(),
    }
}

fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_time_value(value: &str, now: i64) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("time value is empty".into());
    }
    if let Some(secs) = parse_duration_secs(value) {
        return Ok((now - secs).to_string());
    }
    if value.parse::<i64>().is_ok() {
        return Ok(value.to_string());
    }
    Ok(urlencode(value))
}

fn parse_duration_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || (!s.as_bytes()[0].is_ascii_digit() && s.as_bytes()[0] != b'.') {
        return None;
    }
    let mut total = 0.0f64;
    let mut rest = s;
    while !rest.is_empty() {
        let num_len = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        if num_len == 0 {
            return None;
        }
        let num: f64 = rest[..num_len].parse().ok()?;
        rest = &rest[num_len..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        if unit_len == 0 {
            return None;
        }
        let mult = match &rest[..unit_len] {
            "ns" => 1e-9,
            "us" | "µs" | "μs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        total += num * mult;
        rest = &rest[unit_len..];
    }
    Some(total as i64)
}

fn test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = base_url(host)?;
    let start = SystemTime::now();
    let resp = host.http("GET", &format!("{base}/-/ready"), None, &[], None)?;
    let latency_ms = start.elapsed().map(|d| d.as_millis() as i64).unwrap_or(0);
    let ready = resp.is_success();
    let mut out = json!({"url": base, "ready": ready, "latency_ms": latency_ms});
    if !ready {
        out.as_object_mut().unwrap().insert(
            "error".into(),
            json!(format!("prometheus not ready, status {}", resp.status)),
        );
    }
    Ok(out)
}

fn query(input: Value, host: &mut Host) -> Result<Value, String> {
    let q = req_query(&input)?;
    let base = base_url(host)?;
    let mut path = format!("/api/v1/query?query={}", urlencode(q));
    if let Some(time) = opt_str(&input, "time") {
        path.push_str(&format!("&time={}", parse_time_value(time, now_unix())?));
    }
    let resp = prom_get(host, &path)?;
    let out = typed_query_result(&base, q, &resp)?;
    contribute_query_results(host, &out);
    Ok(out)
}

fn query_range(input: Value, host: &mut Host) -> Result<Value, String> {
    let q = req_query(&input)?;
    let base = base_url(host)?;
    let now = now_unix();
    let end = parse_time_value(opt_str(&input, "until").unwrap_or("0s"), now)?;
    let start = parse_time_value(opt_str(&input, "since").unwrap_or("1h"), now)?;
    let step = opt_str(&input, "step").unwrap_or("1m").to_string();
    let path = format!(
        "/api/v1/query_range?query={}&start={}&end={}&step={}",
        urlencode(q),
        start,
        end,
        urlencode(&step)
    );
    let resp = prom_get(host, &path)?;
    let out = typed_query_result(&base, q, &resp)?;
    contribute_query_results(host, &out);
    Ok(out)
}

fn labels(input: Value, host: &mut Host) -> Result<Value, String> {
    let label = opt_str(&input, "label").unwrap_or("");
    let mut path = if label.is_empty() {
        "/api/v1/labels".to_string()
    } else {
        format!("/api/v1/label/{}/values", urlencode(label))
    };
    for (i, sel) in match_selectors(&input).iter().enumerate() {
        path.push(if i == 0 { '?' } else { '&' });
        path.push_str(&format!("match[]={}", urlencode(sel)));
    }
    let base = base_url(host)?;
    let resp = prom_get(host, &path)?;
    let out = typed_labels_result(&base, label, &resp)?;
    contribute_labels(host, label, &resp);
    Ok(out)
}

fn series(input: Value, host: &mut Host) -> Result<Value, String> {
    let selectors = match_selectors(&input);
    if selectors.is_empty() {
        return Err("`match` (one or more PromQL selectors) required".into());
    }
    let limit = match input.get("limit").and_then(|v| v.as_i64()) {
        Some(v) if v > 0 => (v as usize).min(1000),
        _ => 100,
    };
    let now = now_unix();
    let base = base_url(host)?;
    let mut path = String::from("/api/v1/series");
    for (i, sel) in selectors.iter().enumerate() {
        path.push(if i == 0 { '?' } else { '&' });
        path.push_str(&format!("match[]={}", urlencode(sel)));
    }
    if let Some(since) = opt_str(&input, "since") {
        path.push_str(&format!("&start={}", parse_time_value(since, now)?));
    }
    if let Some(until) = opt_str(&input, "until") {
        path.push_str(&format!("&end={}", parse_time_value(until, now)?));
    }
    path.push_str(&format!("&limit={limit}"));
    let resp = prom_get(host, &path)?;
    let out = typed_series_result(&base, limit, &resp)?;
    Ok(out)
}

fn targets(input: Value, host: &mut Host) -> Result<Value, String> {
    let state = opt_str(&input, "state").unwrap_or("active");
    let path = if state.is_empty() || state == "any" {
        "/api/v1/targets".to_string()
    } else {
        format!("/api/v1/targets?state={}", urlencode(state))
    };
    let base = base_url(host)?;
    let resp = prom_get(host, &path)?;
    let out = typed_targets_result(&base, state, &resp)?;
    contribute_targets(host, &resp);
    Ok(out)
}

fn rules(input: Value, host: &mut Host) -> Result<Value, String> {
    let kind = opt_str(&input, "type")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let path = match kind.as_str() {
        "" => "/api/v1/rules".to_string(),
        "alert" | "record" => format!("/api/v1/rules?type={kind}"),
        _ => return Err("`type` must be alert or record".into()),
    };
    let base = base_url(host)?;
    let resp = prom_get(host, &path)?;
    let out = typed_rules_result(&base, &resp)?;
    Ok(out)
}

fn alerts(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = base_url(host)?;
    let resp = prom_get(host, "/api/v1/alerts")?;
    let out = typed_alerts_result(&base, &resp)?;
    contribute_alerts(host, &resp);
    Ok(out)
}

// Typed output helpers (fluxplane parity).

fn typed_query_result(url: &str, query: &str, resp: &Value) -> Result<Value, String> {
    let data = resp.get("data");
    let result_type = data
        .and_then(|d| d.get("resultType"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let result = data.and_then(|d| d.get("result"));
    let (samples, series, truncated) = parse_promql_data(result_type, result)?;
    Ok(
        json!({"url": url, "query": query, "result_type": result_type, "samples": samples, "series": series, "count": samples.len() + series.len(), "truncated": truncated}),
    )
}

fn parse_promql_data(
    result_type: &str,
    result: Option<&Value>,
) -> Result<(Vec<Value>, Vec<Value>, bool), String> {
    let mut truncated = false;
    match result_type.trim().to_ascii_lowercase().as_str() {
        "vector" => {
            let mut samples: Vec<Value> = Vec::new();
            if let Some(arr) = result.and_then(|v| v.as_array()) {
                for item in arr {
                    if let Some((ts, val)) = sample_point_from_pair(item.get("value")) {
                        samples.push(json!({"metric": item.get("metric").cloned().unwrap_or_else(|| json!({})), "timestamp": ts, "value": val}));
                    }
                }
            }
            if samples.len() > MAX_SERIES_PER_RESULT {
                samples.truncate(MAX_SERIES_PER_RESULT);
                truncated = true;
            }
            Ok((samples, Vec::new(), truncated))
        }
        "matrix" => {
            let mut series: Vec<Value> = Vec::new();
            if let Some(arr) = result.and_then(|v| v.as_array()) {
                for item in arr {
                    let values = item.get("values").and_then(|v| v.as_array());
                    let point_count = values.map(|v| v.len()).unwrap_or(0);
                    let mut series_truncated = false;
                    let kept: &[Value] = match values {
                        Some(v) if v.len() > MAX_POINTS_PER_SERIES => {
                            series_truncated = true;
                            truncated = true;
                            &v[v.len() - MAX_POINTS_PER_SERIES..]
                        }
                        Some(v) => v,
                        None => &[],
                    };
                    let points: Vec<Value> = kept
                        .iter()
                        .filter_map(|pair| {
                            sample_point_from_pair(Some(pair))
                                .map(|(ts, val)| json!({"timestamp": ts, "value": val}))
                        })
                        .collect();
                    series.push(json!({"metric": item.get("metric").cloned().unwrap_or_else(|| json!({})), "points": points, "point_count": point_count, "truncated": series_truncated}));
                }
            }
            if series.len() > MAX_SERIES_PER_RESULT {
                series.truncate(MAX_SERIES_PER_RESULT);
                truncated = true;
            }
            Ok((Vec::new(), series, truncated))
        }
        "scalar" | "string" => {
            let mut samples: Vec<Value> = Vec::new();
            if let Some((ts, val)) = sample_point_from_pair(result) {
                samples.push(json!({"timestamp": ts, "value": val}));
            }
            Ok((samples, Vec::new(), false))
        }
        "" => Ok((Vec::new(), Vec::new(), false)),
        other => Err(format!("unsupported result type \"{other}\"")),
    }
}

fn sample_point_from_pair(pair: Option<&Value>) -> Option<(String, String)> {
    let arr = pair.and_then(|v| v.as_array())?;
    if arr.len() != 2 {
        return None;
    }
    Some((timestamp_to_string(&arr[0]), value_to_string(&arr[1])))
}

fn timestamp_to_string(value: &Value) -> String {
    let secs = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    };
    match secs {
        Some(s) => format_rfc3339_utc(s as i64),
        None => value_to_string(value),
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn format_rfc3339_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (hour, min, sec) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn typed_labels_result(url: &str, label: &str, resp: &Value) -> Result<Value, String> {
    let values: Vec<String> = resp
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Map::new();
    out.insert("url".into(), json!(url));
    if !label.is_empty() {
        out.insert("label".into(), json!(label));
    }
    out.insert("values".into(), json!(values));
    Ok(Value::Object(out))
}

fn typed_series_result(url: &str, limit: usize, resp: &Value) -> Result<Value, String> {
    let mut series: Vec<Value> = resp
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| arr.to_vec())
        .unwrap_or_default();
    let truncated = series.len() > limit;
    if truncated {
        series.truncate(limit);
    }
    Ok(json!({"url": url, "series": series, "count": series.len(), "truncated": truncated}))
}

fn first_non_empty<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    a.filter(|s| !s.trim().is_empty())
        .or_else(|| b.filter(|s| !s.trim().is_empty()))
}

fn typed_targets_result(url: &str, state: &str, resp: &Value) -> Result<Value, String> {
    let data = resp.get("data");
    let mut active: Vec<Value> = Vec::new();
    let mut dropped: Vec<Value> = Vec::new();
    if let Some(arr) = data
        .and_then(|d| d.get("activeTargets"))
        .and_then(|v| v.as_array())
    {
        for t in arr {
            active.push(target_from_wire(t, false));
        }
    }
    if let Some(arr) = data
        .and_then(|d| d.get("droppedTargets"))
        .and_then(|v| v.as_array())
    {
        for t in arr {
            dropped.push(target_from_wire(t, true));
        }
    }
    let active_count = active.len();
    let dropped_count = dropped.len();
    let mut targets = active;
    targets.extend(dropped);
    Ok(
        json!({"url": url, "state": state, "targets": targets, "active_count": active_count, "dropped_count": dropped_count}),
    )
}

fn target_from_wire(t: &Value, dropped: bool) -> Value {
    let labels = t.get("labels").cloned().unwrap_or_else(|| json!({}));
    let discovered = t
        .get("discoveredLabels")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let empty = Map::new();
    let primary = labels
        .as_object()
        .filter(|m| !m.is_empty())
        .or_else(|| discovered.as_object())
        .unwrap_or(&empty);
    let job = primary
        .get("job")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            discovered
                .as_object()
                .and_then(|m| m.get("job"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        });
    let instance = first_non_empty(
        primary.get("instance").and_then(|v| v.as_str()),
        discovered
            .as_object()
            .and_then(|m| m.get("__address__"))
            .and_then(|v| v.as_str()),
    );
    json!({"job": job.unwrap_or(""), "instance": instance.unwrap_or(""), "health": t.get("health").and_then(|v| v.as_str()).unwrap_or(""), "scrape_pool": t.get("scrapePool").and_then(|v| v.as_str()).unwrap_or(""), "scrape_url": t.get("scrapeUrl").and_then(|v| v.as_str()).unwrap_or(""), "last_scrape": t.get("lastScrape").and_then(|v| v.as_str()).unwrap_or(""), "last_error": t.get("lastError").and_then(|v| v.as_str()).unwrap_or(""), "labels": labels, "dropped": dropped})
}

fn typed_rules_result(url: &str, resp: &Value) -> Result<Value, String> {
    let groups = resp
        .get("data")
        .and_then(|d| d.get("groups"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(rule_group_from_wire).collect::<Vec<Value>>())
        .unwrap_or_default();
    let rule_count: usize = groups
        .iter()
        .filter_map(|g| g.get("rules").and_then(|v| v.as_array()))
        .map(|r| r.len())
        .sum();
    Ok(json!({"url": url, "groups": groups, "group_count": groups.len(), "rule_count": rule_count}))
}

fn rule_group_from_wire(g: &Value) -> Value {
    let name = g.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let file = g.get("file").and_then(|v| v.as_str()).unwrap_or("");
    let interval = format_seconds(g.get("interval"));
    let rules = g
        .get("rules")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(rule_from_wire).collect::<Vec<Value>>())
        .unwrap_or_default();
    let mut out = Map::new();
    out.insert("name".into(), json!(name));
    if !file.is_empty() {
        out.insert("file".into(), json!(file));
    }
    if let Some(iv) = interval {
        out.insert("interval".into(), json!(iv));
    }
    out.insert("rules".into(), json!(rules));
    Value::Object(out)
}

fn rule_from_wire(r: &Value) -> Value {
    let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let rule_type = r.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let query = r.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let state = r.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let health = r.get("health").and_then(|v| v.as_str()).unwrap_or("");
    let last_error = r.get("lastError").and_then(|v| v.as_str()).unwrap_or("");
    let duration = format_seconds(r.get("duration"));
    let labels = r.get("labels").cloned().unwrap_or_else(|| json!({}));
    let annotations = r.get("annotations").cloned().unwrap_or_else(|| json!({}));
    let active_count = r
        .get("alerts")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut out = Map::new();
    out.insert("name".into(), json!(name));
    out.insert("type".into(), json!(rule_type));
    out.insert("query".into(), json!(query));
    if !state.is_empty() {
        out.insert("state".into(), json!(state));
    }
    if let Some(d) = duration {
        out.insert("for".into(), json!(d));
    }
    out.insert("labels".into(), labels);
    out.insert("annotations".into(), annotations);
    if !health.is_empty() {
        out.insert("health".into(), json!(health));
    }
    if !last_error.is_empty() {
        out.insert("last_error".into(), json!(last_error));
    }
    if active_count > 0 {
        out.insert("active_count".into(), json!(active_count));
    }
    Value::Object(out)
}

fn format_seconds(value: Option<&Value>) -> Option<String> {
    let seconds = value.and_then(|v| v.as_f64()).or_else(|| {
        value
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
    })?;
    if seconds <= 0.0 {
        return None;
    }
    let total = seconds as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        Some(format!("{h}h{m}m{s}s"))
    } else if m > 0 {
        Some(format!("{m}m{s}s"))
    } else {
        Some(format!("{s}s"))
    }
}

fn typed_alerts_result(url: &str, resp: &Value) -> Result<Value, String> {
    let alerts = resp
        .pointer("/data/alerts")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(alert_from_wire).collect::<Vec<Value>>())
        .unwrap_or_default();
    Ok(json!({"url": url, "alerts": alerts, "count": alerts.len()}))
}

fn alert_from_wire(a: &Value) -> Value {
    let labels = a
        .get("labels")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let annotations = a
        .get("annotations")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    json!({"name": labels.get("alertname").and_then(|v| v.as_str()).unwrap_or(""), "state": a.get("state").and_then(|v| v.as_str()).unwrap_or(""), "severity": labels.get("severity").and_then(|v| v.as_str()).unwrap_or(""), "active_at": a.get("activeAt").and_then(|v| v.as_str()).unwrap_or(""), "value": a.get("value").and_then(|v| v.as_str()).unwrap_or(""), "labels": labels, "annotations": annotations})
}

// Datasource contribution.

fn label_of<'a>(labels: Option<&'a Value>, key: &str) -> &'a str {
    labels
        .and_then(|l| l.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

fn metric_title(metric: &Value) -> String {
    let Some(obj) = metric.as_object() else {
        return String::new();
    };
    let name = obj.get("__name__").and_then(|v| v.as_str()).unwrap_or("");
    let mut parts: Vec<String> = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "__name__")
        .filter_map(|(k, v)| v.as_str().map(|s| format!("{k}=\"{s}\"")))
        .collect();
    parts.sort();
    match (name.is_empty(), parts.is_empty()) {
        (true, true) => String::new(),
        (false, true) => name.to_string(),
        (true, false) => format!("{{{}}}", parts.join(",")),
        (false, false) => format!("{name}{{{}}}", parts.join(",")),
    }
}

fn contribute_query_results(host: &mut Host, out: &Value) {
    let samples = out.get("samples").and_then(|v| v.as_array());
    let series = out.get("series").and_then(|v| v.as_array());
    let results: Vec<&Value> = samples
        .into_iter()
        .flatten()
        .chain(series.into_iter().flatten())
        .collect();
    let records: Vec<Record> = results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let title = r
                .get("metric")
                .map(metric_title)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("result {}", i + 1));
            Record::new(
                Source::new("prometheus"),
                "prometheus.query_result",
                title.clone(),
                title,
                r.to_string(),
            )
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_labels(host: &mut Host, label: &str, resp: &Value) {
    let Some(values) = resp.get("data").and_then(|v| v.as_array()) else {
        return;
    };
    let records: Vec<Record> = values
        .iter()
        .filter_map(|v| v.as_str())
        .map(|val| {
            let id = if label.is_empty() {
                val.to_string()
            } else {
                format!("{label}={val}")
            };
            Record::new(
                Source::new("prometheus"),
                "prometheus.label",
                id.clone(),
                id,
                String::new(),
            )
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_targets(host: &mut Host, resp: &Value) {
    let data = resp.get("data");
    let mut records = Vec::new();
    for (key, dropped) in [("activeTargets", false), ("droppedTargets", true)] {
        let Some(arr) = data.and_then(|d| d.get(key)).and_then(|v| v.as_array()) else {
            continue;
        };
        for (i, t) in arr.iter().enumerate() {
            let labels = t.get("labels").or_else(|| t.get("discoveredLabels"));
            let job = label_of(labels, "job");
            let instance = if label_of(labels, "instance").is_empty() {
                label_of(labels, "__address__")
            } else {
                label_of(labels, "instance")
            };
            let health = t.get("health").and_then(|v| v.as_str()).unwrap_or("");
            let ident = [instance, job]
                .into_iter()
                .find(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| i.to_string());
            let title = [job, instance]
                .into_iter()
                .find(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| format!("target {}", i + 1));
            let id = format!("{}:{ident}", if dropped { "dropped" } else { "active" });
            let body = if health.is_empty() {
                t.to_string()
            } else {
                format!("{health} — {t}")
            };
            records.push(Record::new(
                Source::new("prometheus"),
                "prometheus.target",
                id,
                title,
                body,
            ));
        }
    }
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_alerts(host: &mut Host, resp: &Value) {
    let Some(arr) = resp.pointer("/data/alerts").and_then(|v| v.as_array()) else {
        return;
    };
    let records: Vec<Record> = arr
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let labels = a.get("labels");
            let name = label_of(labels, "alertname");
            let state = a.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let summary = a
                .pointer("/annotations/summary")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let title = if name.is_empty() {
                format!("alert {}", i + 1)
            } else {
                name.to_string()
            };
            let id = format!("{title}:{}", labels.map(metric_title).unwrap_or_default());
            let body = [state, summary]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" — ");
            Record::new(
                Source::new("prometheus"),
                "prometheus.alert",
                id,
                title,
                if body.is_empty() { a.to_string() } else { body },
            )
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
    fn test_op_pings_readiness_and_reports_latency() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http("/-/ready", json!("Prometheus Server is Ready."));
        let out = plugin
            .call("prometheus.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert!(out["ready"].as_bool().unwrap());
        assert!(out["latency_ms"].is_number(), "latency_ms missing: {out}");
    }

    #[test]
    fn test_reports_error_when_prometheus_not_ready() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http_status_body("/-/ready", 503, "Service Unavailable");
        let out = plugin
            .call("prometheus.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["ready"], false);
        assert_eq!(out["error"], "prometheus not ready, status 503");
    }

    #[test]
    fn query_hits_endpoint_and_returns_typed_samples() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/query?",
                json!({"status": "success", "data": {"resultType": "vector", "result": [
                    {"metric": {"__name__": "up", "job": "api"}, "value": [1609459200, "1"]}
                ]}}),
            );
        let out = plugin
            .call(
                "prometheus.query",
                json!({"query": "up{job=\"api\"}"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert_eq!(out["result_type"], "vector");
        assert_eq!(out["count"], 1);
        assert_eq!(out["samples"][0]["value"], "1");
        assert_eq!(out["samples"][0]["timestamp"], "2021-01-01T00:00:00Z");
        assert_eq!(
            host.contributed.borrow()[0].entity,
            "prometheus.query_result"
        );
    }

    #[test]
    fn query_rejects_an_empty_expression() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_endpoint("prometheus.endpoint", "https://p.x");
        let err = plugin
            .call("prometheus.query", json!({"query": "  "}), &mut host)
            .unwrap_err();
        assert!(err.contains("query"), "err = {err}");
    }

    #[test]
    fn query_range_returns_typed_series() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x/")
            .with_http("/api/v1/query_range", json!({"status": "success", "data": {"resultType": "matrix", "result": [
                {"metric": {"__name__": "rps", "job": "api"}, "values": [[1609459200, "0.5"], [1609459230, "0.6"]]}
            ]}}));
        let out = plugin.call("prometheus.query_range", json!({"query": "rate(http_requests_total[5m])", "since": "1h", "until": "0s", "step": "30s"}), &mut host).unwrap();
        assert_eq!(out["result_type"], "matrix");
        assert_eq!(out["count"], 1);
        assert_eq!(out["series"][0]["point_count"], 2);
    }

    #[test]
    fn query_range_defaults_since_until_step() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "matrix", "result": []}}),
            );
        let out = plugin
            .call("prometheus.query_range", json!({"query": "up"}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 0);
    }

    #[test]
    fn typed_query_result_caps_points_and_flags_truncation() {
        let n = MAX_POINTS_PER_SERIES + 5;
        let values: Vec<Value> = (0..n)
            .map(|i| json!([1_609_459_200i64 + i as i64, i.to_string()]))
            .collect();
        let resp = json!({"data": {"resultType": "matrix", "result": [{"metric": {"__name__": "m"}, "values": values}]}});
        let out = typed_query_result("https://p.x", "m", &resp).unwrap();
        assert_eq!(out["truncated"], true);
        let one = &out["series"][0];
        assert_eq!(one["point_count"], n);
        assert_eq!(
            one["points"].as_array().unwrap().len(),
            MAX_POINTS_PER_SERIES
        );
        assert_eq!(one["points"][0]["value"], "5");
    }

    #[test]
    fn parse_duration_secs_handles_go_style_durations() {
        assert_eq!(parse_duration_secs("1h"), Some(3600));
        assert_eq!(parse_duration_secs("30m"), Some(1800));
        assert_eq!(parse_duration_secs("1h30m"), Some(5400));
        assert_eq!(parse_duration_secs("0s"), Some(0));
        assert_eq!(parse_duration_secs("500ms"), Some(0));
        assert_eq!(parse_duration_secs("2021-01-01T00:00:00Z"), None);
        assert_eq!(parse_duration_secs("1609459200"), None);
    }

    #[test]
    fn parse_time_value_resolves_relative_passes_through_absolute() {
        assert_eq!(parse_time_value("1h", 10_000).unwrap(), "6400");
        assert_eq!(
            parse_time_value("1609459200", 10_000).unwrap(),
            "1609459200"
        );
        assert_eq!(
            parse_time_value("2021-01-01T00:00:00Z", 10_000).unwrap(),
            "2021-01-01T00%3A00%3A00Z"
        );
        assert!(parse_time_value("  ", 0).is_err());
    }

    #[test]
    fn labels_returns_typed_result_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/label/job/values",
                json!({"status": "success", "data": ["api", "web"]}),
            );
        let out = plugin
            .call("prometheus.labels", json!({"label": "job"}), &mut host)
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert_eq!(out["label"], "job");
        assert_eq!(out["values"], json!(["api", "web"]));
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].entity, "prometheus.label");
        assert_eq!(recs[0].id, "job=api");
    }

    #[test]
    fn series_requires_match_and_returns_typed_result_with_limit() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/series",
                json!({"status": "success", "data": [
                    {"__name__": "up", "job": "api"},
                    {"__name__": "up", "job": "db"},
                    {"__name__": "up", "job": "web"},
                ]}),
            );
        assert!(plugin
            .call("prometheus.series", json!({}), &mut host)
            .is_err());
        let out = plugin
            .call(
                "prometheus.series",
                json!({"match": ["up{job=\"api\"}"], "since": "1h", "until": "0s", "limit": 2}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert_eq!(out["count"], 2);
        assert!(out["truncated"].as_bool().unwrap());
        assert_eq!(out["series"][0]["job"], "api");
    }

    #[test]
    fn targets_returns_typed_result_with_counts_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http("/api/v1/targets", json!({"status": "success", "data": {"activeTargets": [
                {"labels": {"job": "api", "instance": "10.0.0.1:9090"}, "health": "up", "scrapePool": "api", "scrapeUrl": "http://10.0.0.1:9090/metrics"}
            ], "droppedTargets": [
                {"discoveredLabels": {"job": "old", "__address__": "old:9090"}}
            ]}}));
        let out = plugin
            .call("prometheus.targets", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert_eq!(out["state"], "active");
        assert_eq!(out["active_count"], 1);
        assert_eq!(out["dropped_count"], 1);
        assert_eq!(out["targets"][0]["health"], "up");
        assert!(out["targets"][1]["dropped"].as_bool().unwrap());
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].entity, "prometheus.target");
        assert_eq!(recs[0].title, "api");
    }

    #[test]
    fn rules_returns_typed_result_with_counts_and_rejects_bad_type() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http("/api/v1/rules", json!({"status": "success", "data": {"groups": [
                {"name": "g1", "file": "rules.yml", "interval": 30, "rules": [
                    {"name": "HighErrors", "type": "alerting", "query": "rate(errors[5m]) > 0.1", "state": "firing", "duration": 300, "health": "ok", "labels": {"severity": "critical"}, "annotations": {"summary": "too many errors"}, "alerts": [{"state": "firing"}]}
                ]}
            ]}}));
        assert!(plugin
            .call("prometheus.rules", json!({"type": "bogus"}), &mut host)
            .is_err());
        let out = plugin
            .call("prometheus.rules", json!({"type": "alert"}), &mut host)
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert_eq!(out["group_count"], 1);
        assert_eq!(out["rule_count"], 1);
        assert_eq!(out["groups"][0]["interval"], "30s");
        let rule = &out["groups"][0]["rules"][0];
        assert_eq!(rule["name"], "HighErrors");
        assert_eq!(rule["state"], "firing");
        assert_eq!(rule["for"], "5m0s");
        assert_eq!(rule["active_count"], 1);
        assert_eq!(rule["labels"]["severity"], "critical");
    }

    #[test]
    fn alerts_returns_typed_result_with_count_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http("/api/v1/alerts", json!({"status": "success", "data": {"alerts": [
                {"labels": {"alertname": "HighErrors", "severity": "critical"}, "state": "firing", "activeAt": "2024-01-01T00:00:00Z", "value": "1", "annotations": {"summary": "too many 5xx"}}
            ]}}));
        let out = plugin
            .call("prometheus.alerts", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert_eq!(out["count"], 1);
        assert_eq!(out["alerts"][0]["state"], "firing");
        assert_eq!(out["alerts"][0]["name"], "HighErrors");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "prometheus.alert");
        assert_eq!(recs[0].title, "HighErrors");
    }

    #[test]
    fn manifest_declares_eight_read_ops_no_auth_and_datasources() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 8);
        assert!(m.auth.is_empty());
        assert_eq!(m.endpoints[0].name, "prometheus.endpoint");
        assert!(m.operations.iter().all(|o| o.effects == vec![Effect::Read]));
        assert_eq!(m.datasources.len(), 4);
        let entities: Vec<&str> = m.datasources.iter().map(|d| d.entity.as_str()).collect();
        assert!(entities.contains(&"prometheus.query_result"));
        assert!(entities.contains(&"prometheus.alert"));
    }
}

#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        ArrayStr,
        Enum(Vec<String>),
        Object,
    }

    struct Prop {
        name: &'static str,
        kind: Kind,
    }
    struct OpContract {
        props: Vec<Prop>,
        required: Vec<&'static str>,
    }

    fn p(name: &'static str, kind: Kind) -> Prop {
        Prop { name, kind }
    }
    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }

    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            ("prometheus.test", c(vec![], vec![])),
            (
                "prometheus.query",
                c(
                    vec![p("query", Kind::Str), p("time", Kind::Str)],
                    vec!["query"],
                ),
            ),
            (
                "prometheus.query_range",
                c(
                    vec![
                        p("query", Kind::Str),
                        p("since", Kind::Str),
                        p("until", Kind::Str),
                        p("step", Kind::Str),
                    ],
                    vec!["query"],
                ),
            ),
            (
                "prometheus.labels",
                c(
                    vec![p("label", Kind::Str), p("match", Kind::ArrayStr)],
                    vec![],
                ),
            ),
            (
                "prometheus.series",
                c(
                    vec![
                        p("match", Kind::ArrayStr),
                        p("since", Kind::Str),
                        p("until", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec!["match"],
                ),
            ),
            (
                "prometheus.targets",
                c(
                    vec![p(
                        "state",
                        Kind::Enum(vec!["active".into(), "dropped".into(), "any".into()]),
                    )],
                    vec![],
                ),
            ),
            (
                "prometheus.rules",
                c(
                    vec![p("type", Kind::Enum(vec!["alert".into(), "record".into()]))],
                    vec![],
                ),
            ),
            ("prometheus.alerts", c(vec![], vec![])),
        ]
    }

    fn resolve<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
        if let Some(obj) = node.as_object() {
            if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
                if let Some(name) = r.strip_prefix("#/definitions/") {
                    return defs.get(name).unwrap_or(node);
                }
            }
            if let Some(any) = obj.get("anyOf").and_then(|v| v.as_array()) {
                for m in any {
                    if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                        return resolve(m, defs);
                    }
                }
            }
        }
        node
    }

    fn kind_of(node: &Value) -> Kind {
        let t = node.get("type");
        if let Some(arr) = t.and_then(|v| v.as_array()) {
            let first = arr
                .iter()
                .find(|v| v.as_str() != Some("null"))
                .and_then(|v| v.as_str())
                .unwrap_or("null");
            return base_kind(first, node);
        }
        base_kind(t.and_then(|v| v.as_str()).unwrap_or(""), node)
    }

    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "string" => {
                if let Some(e) = node.get("enum").and_then(|v| v.as_array()) {
                    let vals: Vec<String> = e
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    if !vals.is_empty() {
                        return Kind::Enum(vals);
                    }
                }
                Kind::Str
            }
            "array" => {
                let item_type = node
                    .pointer("/items/type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                assert_eq!(
                    item_type, "string",
                    "unsupported array item type: {item_type} ({node})"
                );
                Kind::ArrayStr
            }
            "object" | "" => Kind::Object,
            other => panic!("unsupported property type: {other} ({node})"),
        }
    }

    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        let defs = schema.get("definitions").cloned().unwrap_or(json!({}));
        assert_eq!(schema["type"], "object", "{op_name}: root type");

        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                got.insert(k.as_str(), kind_of(resolve(v, &defs)));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got.get(*name).unwrap_or_else(|| {
                panic!("{op_name}: missing property `{name}` in derived schema")
            });
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }

        let mut req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        req.sort();
        let mut want_req = contract.required.clone();
        want_req.sort();
        assert_eq!(req, want_req, "{op_name}: required set");
    }

    #[test]
    fn derived_schemas_match_authoritative_contract() {
        let ops = contracts();
        let manifest = manifest_builder().build().manifest();
        let by_name: BTreeMap<&str, &OperationSpec> = manifest
            .operations
            .iter()
            .map(|o| (o.name.as_str(), o))
            .collect();
        assert_eq!(by_name.len(), ops.len(), "op count changed");
        for (name, contract) in &ops {
            let spec = by_name
                .get(*name)
                .unwrap_or_else(|| panic!("missing op {name}"));
            assert_contract(name, &spec.input_schema, contract);
        }
    }
}

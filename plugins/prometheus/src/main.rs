//! `prometheus` — a flux integration plugin for the Prometheus HTTP API (v1): readiness, instant and
//! range PromQL queries, label/series discovery, scrape targets, alerting/recording rules, and active
//! alerts. The base URL is the `prometheus.endpoint`; Prometheus is queried anonymously — the plugin
//! declares no auth, only network access. All ops are read-only.
//!
//! The query/labels/targets/alerts ops contribute records (keyed by stable metric/label/target/alert
//! identity, so a re-run upserts current state rather than appending) to the matching
//! `prometheus.query_results` / `prometheus.labels` / `prometheus.targets` / `prometheus.alerts`
//! datasources, making the live state searchable.

use host_kit::*;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

/// Result-size caps. Range queries can return thousands of points per series; the caps keep
/// operation output agent-readable and signal truncation explicitly instead of dumping everything.
/// Mirrors the reference's `maxSeriesPerResult` / `maxPointsPerSeries`.
const MAX_SERIES_PER_RESULT: usize = 200;
const MAX_POINTS_PER_SERIES: usize = 500;

fn manifest_builder() -> PluginBuilder {
    let query_arg = json!({ "query": {"type": "string", "description": "a PromQL expression"} });
    PluginBuilder::new("prometheus", "0.1.0")
        .capabilities(Caps {
            http: true,
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "prometheus.endpoint".into(),
            env: vec!["PROMETHEUS_URL".into(), "PROM_URL".into()],
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
        .operation(
            read_op(
                "prometheus.test",
                "Check whether the Prometheus endpoint is reachable and ready.",
                json!({"type": "object", "properties": {}}),
            ),
            test,
        )
        .operation(
            read_op(
                "prometheus.query",
                "Evaluate a PromQL expression at a single instant (optionally at `time`). Results are parsed into samples (vector/scalar/string).",
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
                "Evaluate a PromQL expression over a time range at a fixed step. Results are parsed into series of timestamped points.",
                json!({"type": "object", "properties": {
                    "query": query_arg["query"],
                    "since": {"type": "string", "description": "start time as RFC3339, unix timestamp, or duration ago (e.g. \"1h\"); defaults to 1h"},
                    "until": {"type": "string", "description": "end time as RFC3339, unix timestamp, or duration ago; defaults to now"},
                    "step": {"type": "string", "description": "resolution step (e.g. \"30s\"); defaults to 1m"}
                }, "required": ["query"]}),
            ),
            query_range,
        )
        .operation(
            read_op(
                "prometheus.labels",
                "List label names, or the values of one `label`; narrow with `match` selectors.",
                json!({"type": "object", "properties": {
                    "label": {"type": "string", "description": "a label name; when set, returns its values instead of label names"},
                    "match": {"type": "array", "items": {"type": "string"}, "description": "PromQL series selectors that scope the result"}
                }}),
            ),
            labels,
        )
        .operation(
            read_op(
                "prometheus.series",
                "List the series (label sets) matching one or more PromQL selectors.",
                json!({"type": "object", "properties": {
                    "match": {"type": "array", "items": {"type": "string"}, "description": "PromQL series selectors, e.g. up{job=\"api\"} (at least one required)"},
                    "start": {"type": "string", "description": "range start (RFC3339 or unix)"},
                    "end": {"type": "string", "description": "range end (RFC3339 or unix)"},
                    "limit": {"type": "integer", "description": "max series to return"}
                }, "required": ["match"]}),
            ),
            series,
        )
        .operation(
            read_op(
                "prometheus.targets",
                "List the scrape targets and their health (state: active|dropped|any).",
                json!({"type": "object", "properties": {
                    "state": {"type": "string", "description": "active (default), dropped, or any"}
                }}),
            ),
            targets,
        )
        .operation(
            read_op(
                "prometheus.rules",
                "List alerting and recording rules with state and health (type: alert|record to filter).",
                json!({"type": "object", "properties": {
                    "type": {"type": "string", "description": "alert or record; omit for both"}
                }}),
            ),
            rules,
        )
        .operation(
            read_op(
                "prometheus.alerts",
                "List the currently active alerts.",
                json!({"type": "object", "properties": {}}),
            ),
            alerts,
        )
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

/// Resolve the Prometheus base URL (trailing slash trimmed) from the configured endpoint.
fn base_url(host: &mut Host) -> Result<String, String> {
    Ok(host
        .endpoint("prometheus.endpoint")?
        .trim_end_matches('/')
        .to_string())
}

/// GET `{base}{path}` anonymously and return the parsed JSON body.
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

/// The required, non-empty PromQL `query` (trimmed) — matching the reference's "query is required"
/// rejection of an absent or whitespace-only expression.
fn req_query(input: &Value) -> Result<&str, String> {
    let q = req_str(input, "query")?.trim();
    if q.is_empty() {
        return Err("`query` is required".into());
    }
    Ok(q)
}

/// Percent-encode a value for a URL query: alphanumerics and `-_.~` pass through, everything else
/// becomes `%XX`. Used for PromQL expressions, label names, selectors, and timestamps.
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

/// Collect the non-empty `match` selectors (accepts a single string or an array of strings).
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

/// A trimmed, non-empty string input value, or `None` when absent/blank.
fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Seconds since the Unix epoch, now.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Resolve a time expression to a unix-seconds string for the Prometheus API. Accepts a
/// duration-ago (e.g. "1h", "30m", "1h30m" → `now - d`), a bare unix timestamp (passed through),
/// or an RFC3339 string (passed through verbatim — Prometheus accepts it natively). Mirrors the
/// reference's `parseTimeValue`. Returns an error for a blank value.
fn parse_time_value(value: &str, now: i64) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("time value is empty".into());
    }
    if let Some(secs) = parse_duration_secs(value) {
        return Ok((now - secs).to_string());
    }
    // A bare integer is already a unix timestamp; anything else (RFC3339) passes through encoded.
    if value.parse::<i64>().is_ok() {
        return Ok(value.to_string());
    }
    Ok(urlencode(value))
}

/// Parse a Go-style duration ("300ms", "1.5h", "2h45m", "30s") into whole seconds, or `None` if the
/// string is not a duration. Supports `ns`/`us`/`µs`/`ms`/`s`/`m`/`h` unit suffixes (sub-second
/// units floor to 0s). Used for `since`/`until` relative offsets.
fn parse_duration_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() || !s.as_bytes()[0].is_ascii_digit() && s.as_bytes()[0] != b'.' {
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
    let resp = host.http("GET", &format!("{base}/-/ready"), None, &[], None)?;
    Ok(json!({ "url": base, "ready": resp.is_success(), "status": resp.status }))
}

fn query(input: Value, host: &mut Host) -> Result<Value, String> {
    let q = req_query(&input)?;
    let base = base_url(host)?;
    let mut path = format!("/api/v1/query?query={}", urlencode(q));
    if let Some(time) = input.get("time").and_then(|v| v.as_str()).map(str::trim) {
        if !time.is_empty() {
            let resolved = parse_time_value(time, now_unix())?;
            path.push_str(&format!("&time={resolved}"));
        }
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
    // `since`/`until` accept RFC3339, a unix timestamp, or a duration-ago (e.g. "1h"); they default
    // to 1h-ago and now. `step` defaults to 1m. Matches the reference's input shape and defaulting.
    let end = parse_time_value(opt_str(&input, "until").unwrap_or("0s"), now)?;
    let start = parse_time_value(opt_str(&input, "since").unwrap_or("1h"), now)?;
    let step = match opt_str(&input, "step") {
        Some(s) => s.to_string(),
        None => "1m".to_string(),
    };
    let path = format!(
        "/api/v1/query_range?query={}&start={}&end={}&step={}",
        urlencode(q),
        start,
        end,
        urlencode(&step),
    );
    let resp = prom_get(host, &path)?;
    let out = typed_query_result(&base, q, &resp)?;
    contribute_query_results(host, &out);
    Ok(out)
}

fn labels(input: Value, host: &mut Host) -> Result<Value, String> {
    let label = input
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let mut path = if label.is_empty() {
        "/api/v1/labels".to_string()
    } else {
        format!("/api/v1/label/{}/values", urlencode(label))
    };
    for (i, sel) in match_selectors(&input).iter().enumerate() {
        path.push(if i == 0 { '?' } else { '&' });
        path.push_str(&format!("match[]={}", urlencode(sel)));
    }
    let out = prom_get(host, &path)?;
    contribute_labels(host, label, &out);
    Ok(out)
}

fn series(input: Value, host: &mut Host) -> Result<Value, String> {
    let selectors = match_selectors(&input);
    if selectors.is_empty() {
        return Err("`match` (one or more PromQL selectors) required".into());
    }
    let mut path = String::from("/api/v1/series");
    for (i, sel) in selectors.iter().enumerate() {
        path.push(if i == 0 { '?' } else { '&' });
        path.push_str(&format!("match[]={}", urlencode(sel)));
    }
    if let Some(start) = input.get("start").and_then(|v| v.as_str()) {
        path.push_str(&format!("&start={}", urlencode(start)));
    }
    if let Some(end) = input.get("end").and_then(|v| v.as_str()) {
        path.push_str(&format!("&end={}", urlencode(end)));
    }
    if let Some(limit) = input.get("limit").and_then(|v| v.as_i64()) {
        path.push_str(&format!("&limit={limit}"));
    }
    prom_get(host, &path)
}

fn targets(input: Value, host: &mut Host) -> Result<Value, String> {
    let state = input
        .get("state")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("active");
    let path = if state.is_empty() || state == "any" {
        "/api/v1/targets".to_string()
    } else {
        format!("/api/v1/targets?state={}", urlencode(state))
    };
    let out = prom_get(host, &path)?;
    contribute_targets(host, &out);
    Ok(out)
}

fn rules(input: Value, host: &mut Host) -> Result<Value, String> {
    let kind = input
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let path = match kind.as_str() {
        "" => "/api/v1/rules".to_string(),
        "alert" | "record" => format!("/api/v1/rules?type={kind}"),
        _ => return Err("`type` must be alert or record".into()),
    };
    prom_get(host, &path)
}

fn alerts(_input: Value, host: &mut Host) -> Result<Value, String> {
    let out = prom_get(host, "/api/v1/alerts")?;
    contribute_alerts(host, &out);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Datasource contribution — parse the read responses into searchable records.
// ---------------------------------------------------------------------------

/// One label's value from a metric/label object, or `""` when absent.
fn label_of<'a>(labels: Option<&'a Value>, key: &str) -> &'a str {
    labels
        .and_then(|l| l.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// Build a readable title from a metric label set: `name{k="v",…}` (or just the joined labels).
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

/// Decode the Prometheus `{status, data: {resultType, result}}` envelope into the reference's typed
/// shape: `{url, query, result_type, samples, series, count, truncated}`. Vector/scalar/string land
/// in `samples` (one value per metric); matrix lands in `series` (timestamped points per metric),
/// with per-series and total-series caps and an explicit `truncated` flag. Mirrors the reference's
/// `Service.query` + `parsePromQLData`.
fn typed_query_result(url: &str, query: &str, resp: &Value) -> Result<Value, String> {
    let data = resp.get("data");
    let result_type = data
        .and_then(|d| d.get("resultType"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let result = data.and_then(|d| d.get("result"));
    let (samples, series, truncated) = parse_promql_data(result_type, result)?;
    let count = samples.len() + series.len();
    Ok(json!({
        "url": url,
        "query": query,
        "result_type": result_type,
        "samples": samples,
        "series": series,
        "count": count,
        "truncated": truncated,
    }))
}

/// Parse the `data.result` payload for all four PromQL result types into `(samples, series,
/// truncated)`. Vector/scalar/string → samples; matrix → series with caps.
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
                    let Some((timestamp, value)) = sample_point_from_pair(item.get("value")) else {
                        continue;
                    };
                    samples.push(json!({
                        "metric": item.get("metric").cloned().unwrap_or_else(|| json!({})),
                        "timestamp": timestamp,
                        "value": value,
                    }));
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
                            &v[v.len() - MAX_POINTS_PER_SERIES..] // keep the newest
                        }
                        Some(v) => v,
                        None => &[],
                    };
                    let points: Vec<Value> = kept
                        .iter()
                        .filter_map(|pair| {
                            sample_point_from_pair(Some(pair)).map(|(timestamp, value)| {
                                json!({"timestamp": timestamp, "value": value})
                            })
                        })
                        .collect();
                    series.push(json!({
                        "metric": item.get("metric").cloned().unwrap_or_else(|| json!({})),
                        "points": points,
                        "point_count": point_count,
                        "truncated": series_truncated,
                    }));
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
            if let Some((timestamp, value)) = sample_point_from_pair(result) {
                samples.push(json!({"timestamp": timestamp, "value": value}));
            }
            Ok((samples, Vec::new(), false))
        }
        "" => Ok((Vec::new(), Vec::new(), false)),
        other => Err(format!("unsupported result type \"{other}\"")),
    }
}

/// Decode Prometheus's `[unixSeconds, "value"]` pair into `(rfc3339-ish timestamp, value-string)`.
/// The value stays a string because Prometheus legitimately returns "NaN"/"+Inf"/"-Inf".
fn sample_point_from_pair(pair: Option<&Value>) -> Option<(String, String)> {
    let arr = pair.and_then(|v| v.as_array())?;
    if arr.len() != 2 {
        return None;
    }
    Some((timestamp_to_string(&arr[0]), value_to_string(&arr[1])))
}

/// Render the timestamp half of a sample pair: a unix-seconds number (or numeric string) becomes a
/// UTC `YYYY-MM-DDTHH:MM:SSZ` string; anything else passes through as its string form.
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

/// The string form of a JSON scalar (strings unquoted/trimmed; numbers/bools stringified; null → "").
fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Format unix seconds as a UTC `YYYY-MM-DDTHH:MM:SSZ` timestamp (proleptic Gregorian, no leap
/// seconds) without pulling in a date crate.
fn format_rfc3339_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (hour, min, sec) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    // Civil-from-days (Howard Hinnant's algorithm), epoch = 1970-01-01.
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

/// Contribute one record per query sample/series (upserted by metric identity, falling back to a
/// positional id). Reads the typed `samples`/`series` so contribution stays aligned with the result.
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

/// Contribute one record per returned label name or value (upserted by id).
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

/// Contribute one record per scrape target (active + dropped), upserted by job/instance identity.
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

/// Contribute one record per active alert (upserted by alertname + label fingerprint).
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
    fn test_op_pings_readiness() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http("/-/ready", json!("Prometheus Server is Ready."));
        let out = plugin
            .call("prometheus.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["url"], "https://p.x");
        assert!(out["ready"].as_bool().unwrap());
    }

    #[test]
    fn query_hits_the_instant_endpoint_and_returns_typed_samples() {
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
                json!({ "query": "up{job=\"api\"}" }),
                &mut host,
            )
            .unwrap();
        // Typed shape: result_type/samples/series/count, not the raw envelope.
        assert_eq!(out["url"], "https://p.x");
        assert_eq!(out["query"], "up{job=\"api\"}");
        assert_eq!(out["result_type"], "vector");
        assert_eq!(out["count"], 1);
        assert_eq!(out["truncated"], false);
        assert_eq!(out["series"].as_array().unwrap().len(), 0);
        let sample = &out["samples"][0];
        assert_eq!(sample["metric"]["job"], "api");
        assert_eq!(sample["value"], "1");
        assert_eq!(sample["timestamp"], "2021-01-01T00:00:00Z");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "prometheus.query_result");
        assert_eq!(recs[0].title, "up{job=\"api\"}");
    }

    #[test]
    fn query_rejects_an_empty_expression() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_endpoint("prometheus.endpoint", "https://p.x");
        let err = plugin
            .call("prometheus.query", json!({ "query": "  " }), &mut host)
            .unwrap_err();
        assert!(err.contains("query"), "err = {err}");
    }

    #[test]
    fn query_range_hits_the_range_endpoint_and_returns_typed_series() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x/")
            .with_http(
                "/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "matrix", "result": [
                    {"metric": {"__name__": "rps", "job": "api"}, "values": [[1609459200, "0.5"], [1609459230, "0.6"]]}
                ]}}),
            );
        let out = plugin
            .call(
                "prometheus.query_range",
                json!({"query": "rate(http_requests_total[5m])", "since": "1h", "until": "0s", "step": "30s"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["result_type"], "matrix");
        assert_eq!(out["count"], 1);
        assert_eq!(out["samples"].as_array().unwrap().len(), 0);
        let one = &out["series"][0];
        assert_eq!(one["metric"]["job"], "api");
        assert_eq!(one["point_count"], 2);
        assert_eq!(one["truncated"], false);
        assert_eq!(one["points"].as_array().unwrap().len(), 2);
        assert_eq!(one["points"][0]["value"], "0.5");
        assert_eq!(one["points"][0]["timestamp"], "2021-01-01T00:00:00Z");
    }

    #[test]
    fn query_range_defaults_since_until_and_step_when_omitted() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "matrix", "result": []}}),
            );
        // Only `query` is supplied → since=1h, until=now, step=1m are applied without error.
        let out = plugin
            .call(
                "prometheus.query_range",
                json!({"query": "rate(http_requests_total[5m])"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["result_type"], "matrix");
        assert_eq!(out["count"], 0);
    }

    #[test]
    fn typed_query_result_caps_points_per_series_and_flags_truncation() {
        // A matrix series with more than MAX_POINTS_PER_SERIES points keeps the newest and flags it.
        let n = MAX_POINTS_PER_SERIES + 5;
        let values: Vec<Value> = (0..n)
            .map(|i| json!([1_609_459_200i64 + i as i64, i.to_string()]))
            .collect();
        let resp = json!({"data": {"resultType": "matrix", "result": [
            {"metric": {"__name__": "m"}, "values": values}
        ]}});
        let out = typed_query_result("https://p.x", "m", &resp).unwrap();
        assert_eq!(out["truncated"], true);
        let one = &out["series"][0];
        assert_eq!(one["point_count"], n);
        assert_eq!(one["truncated"], true);
        assert_eq!(
            one["points"].as_array().unwrap().len(),
            MAX_POINTS_PER_SERIES
        );
        // Newest kept: first retained point is index 5 (value "5").
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
        // Duration-ago resolves against now.
        assert_eq!(parse_time_value("1h", 10_000).unwrap(), "6400");
        // Bare unix timestamp passes through unchanged.
        assert_eq!(
            parse_time_value("1609459200", 10_000).unwrap(),
            "1609459200"
        );
        // RFC3339 passes through (url-encoded).
        assert_eq!(
            parse_time_value("2021-01-01T00:00:00Z", 10_000).unwrap(),
            "2021-01-01T00%3A00%3A00Z"
        );
        assert!(parse_time_value("  ", 0).is_err());
    }

    #[test]
    fn labels_fetches_values_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/label/job/values",
                json!({"status": "success", "data": ["api", "web"]}),
            );
        let out = plugin
            .call("prometheus.labels", json!({ "label": "job" }), &mut host)
            .unwrap();
        assert_eq!(out["data"][0], "api");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].entity, "prometheus.label");
        assert_eq!(recs[0].id, "job=api");
    }

    #[test]
    fn series_requires_match_and_lists_series() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/series",
                json!({"status": "success", "data": [{"__name__": "up", "job": "api"}]}),
            );
        // missing `match` → error before any request
        assert!(plugin
            .call("prometheus.series", json!({}), &mut host)
            .is_err());
        let out = plugin
            .call(
                "prometheus.series",
                json!({ "match": ["up{job=\"api\"}"] }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["data"][0]["job"], "api");
    }

    #[test]
    fn targets_lists_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/targets",
                json!({"status": "success", "data": {"activeTargets": [
                    {"labels": {"job": "api", "instance": "10.0.0.1:9090"}, "health": "up"}
                ], "droppedTargets": []}}),
            );
        let out = plugin
            .call("prometheus.targets", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["data"]["activeTargets"][0]["health"], "up");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "prometheus.target");
        assert_eq!(recs[0].title, "api");
    }

    #[test]
    fn rules_lists_groups_and_rejects_bad_type() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/rules",
                json!({"status": "success", "data": {"groups": [
                    {"name": "g1", "rules": [{"name": "HighErrors", "type": "alerting", "state": "firing"}]}
                ]}}),
            );
        assert!(plugin
            .call("prometheus.rules", json!({ "type": "bogus" }), &mut host)
            .is_err());
        let out = plugin
            .call("prometheus.rules", json!({ "type": "alert" }), &mut host)
            .unwrap();
        assert_eq!(out["data"]["groups"][0]["rules"][0]["name"], "HighErrors");
    }

    #[test]
    fn alerts_hits_the_alerts_endpoint_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("prometheus.endpoint", "https://p.x")
            .with_http(
                "/api/v1/alerts",
                json!({"status": "success", "data": {"alerts": [
                    {"labels": {"alertname": "HighErrors", "severity": "critical"}, "state": "firing", "annotations": {"summary": "too many 5xx"}}
                ]}}),
            );
        let out = plugin
            .call("prometheus.alerts", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["data"]["alerts"][0]["state"], "firing");
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

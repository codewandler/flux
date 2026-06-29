//! `loki` — a flux integration plugin for the Grafana Loki HTTP API: readiness checks, LogQL stream
//! queries, LogQL metric queries (rate/count over a window), recent-log lookups by app/pod/container,
//! and label discovery. The base URL is the `loki.endpoint` (resolved from `LOKI_URL`/`LOKI_ADDR`).
//!
//! Auth is the point of this slice — Loki is **not** Bearer-authenticated. Two optional credentials are
//! declared and injected by the host (D-12), so plain unauthenticated Lokis keep working (purpose names
//! mirror the fluxplane reference's `basic_password`/`tenant_id`):
//!   * `basic_password` — HTTP **Basic** auth (`LOKI_USERNAME` + `LOKI_PASSWORD`); the host injects
//!     `Authorization: Basic base64(user:pass)` when `http.do` names `auth_purpose: "basic_password"`.
//!   * `tenant_id` — a **Header**-scheme method (`X-Scope-OrgID` ← `LOKI_TENANT_ID`) for multi-tenant
//!     Loki. Because `http.do` injects only one auth scheme per call, the tenant value is resolved via
//!     the host `secret` capability and set as an explicit `X-Scope-OrgID` header alongside Basic auth.
//!
//! All ops are read-only. `query`/`recent_logs` also contribute `loki.log_entry` records and `labels`
//! contributes `loki.label` records to the host index (the `loki.log_entries`/`loki.labels` datasources).

use host_kit::*;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Loki's default page size when the caller omits `limit`.
const DEFAULT_LIMIT: i64 = 100;
/// Hard cap on `limit` so a single page stays bounded.
const MAX_LIMIT: i64 = 1000;

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("loki", "0.1.0")
        .capabilities(Caps {
            http: true,
            // The two secret-resolved env keys. The Basic username (`LOKI_USERNAME`) is config-like
            // (a `user_env`), so it is resolved without a secret grant — like an endpoint.
            secrets: vec!["LOKI_PASSWORD".into(), "LOKI_TENANT_ID".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            description: "HTTP Basic auth for the Loki API (LOKI_USERNAME / LOKI_PASSWORD). Optional — \
                omit both for an unauthenticated Loki."
                .into(),
            ..AuthMethod::basic(
                "basic_password",
                vec!["LOKI_USERNAME".into()],
                vec!["LOKI_PASSWORD".into()],
            )
        })
        .auth(AuthMethod {
            description: "Loki tenant id, sent as the X-Scope-OrgID header (multi-tenant Loki). Optional."
                .into(),
            ..AuthMethod::header("tenant_id", "X-Scope-OrgID", vec!["LOKI_TENANT_ID".into()])
        })
        .endpoint(EndpointSpec {
            name: "loki.endpoint".into(),
            env: vec!["LOKI_URL".into(), "LOKI_ADDR".into()],
            description: "Loki base URL (e.g. https://loki.example.com)".into(),
        })
        .datasource(ds(
            "loki.log_entries",
            "loki.log_entry",
            "Loki log entries.",
        ))
        .datasource(ds("loki.labels", "loki.label", "Loki label names or values."))
        .operation(
            read_op(
                "loki.test",
                "Check Loki readiness (GET /ready) and report latency.",
                json!({"type": "object", "properties": {}}),
            ),
            test,
        )
        .operation(
            read_op(
                "loki.query",
                "Run a LogQL stream query over a time window (query_range) and return the matching log entries.",
                json!({"type": "object", "properties": {
                    "query": {"type": "string", "description": "a LogQL stream query, e.g. {namespace=\"core\"} |= \"error\""},
                    "since": {"type": "string", "description": "start time: RFC3339, unix seconds, duration ago (30m, 24h), or now. Defaults to 1h."},
                    "until": {"type": "string", "description": "end time: RFC3339, unix seconds, duration ago, or now. Defaults to now."},
                    "limit": {"type": "integer", "description": "max entries (default 100, capped at 1000)"},
                    "direction": {"type": "string", "description": "backward (default) or forward", "enum": ["backward", "forward"]}
                }, "required": ["query"]}),
            ),
            query,
        )
        .operation(
            read_op(
                "loki.metric",
                "Run a LogQL metric query over a window (query_range, matrix result) — one call for rate/count \
                 questions like \"when did this error start and how many per day\" instead of paging raw streams.",
                json!({"type": "object", "properties": {
                    "query": {"type": "string", "description": "a LogQL metric query, e.g. sum(count_over_time({namespace=\"core\"} |= \"error\" [1d])). A bare stream selector is rejected."},
                    "since": {"type": "string", "description": "start time: RFC3339, unix seconds, duration ago, or now. Defaults to 24h."},
                    "until": {"type": "string", "description": "end time: RFC3339, unix seconds, duration ago, or now. Defaults to now."},
                    "step": {"type": "string", "description": "resolution step (15s, 5m, 1h, 1d). Defaults to ~100 points across the window."}
                }, "required": ["query"]}),
            ),
            metric,
        )
        .operation(
            read_op(
                "loki.labels",
                "List Loki label names, or the values of one label.",
                json!({"type": "object", "properties": {
                    "label": {"type": "string", "description": "optional label name; when set, returns its values instead of label names"},
                    "query": {"type": "string", "description": "optional LogQL stream selector to scope the labels"},
                    "since": {"type": "string", "description": "optional start time: RFC3339, unix seconds, duration ago, or now"},
                    "until": {"type": "string", "description": "optional end time: RFC3339, unix seconds, duration ago, or now"}
                }}),
            ),
            labels,
        )
        .operation(
            read_op(
                "loki.recent_logs",
                "Query recent logs by app, pod, container, namespace, or text filter (builds the LogQL selector for you).",
                json!({"type": "object", "properties": {
                    "app": {"type": "string", "description": "exact app label filter"},
                    "namespace": {"type": "string", "description": "exact namespace label filter"},
                    "pod": {"type": "string", "description": "exact pod label filter"},
                    "container": {"type": "string", "description": "exact container label filter"},
                    "contains": {"type": "string", "description": "line substring filter"},
                    "since": {"type": "string", "description": "start time: RFC3339, unix seconds, duration ago, or now. Defaults to 1h."},
                    "limit": {"type": "integer", "description": "max entries (default 100, capped at 1000)"}
                }}),
            ),
            recent_logs,
        )
}

/// A search-only datasource declaration.
fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into()],
        entity_schema: None,
    }
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

fn test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = base_url(host)?;
    let (auth, tenant) = auth_bits(host);
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(t) = tenant.as_deref() {
        headers.push(("X-Scope-OrgID", t));
    }
    let started = SystemTime::now();
    let resp = host.http("GET", &format!("{base}/ready"), auth, &headers, None)?;
    let latency_ms = started.elapsed().map(|d| d.as_millis() as u64).unwrap_or(0);
    let ready = resp.is_success();
    let mut out = json!({ "url": base, "ready": ready, "latency_ms": latency_ms });
    if !ready {
        out["error"] = json!(format!("loki not ready, status {}", resp.status));
    }
    Ok(out)
}

fn query(input: Value, host: &mut Host) -> Result<Value, String> {
    let expr = req_str(&input, "query")?.trim();
    if expr.is_empty() {
        return Err("`query` (string) required".into());
    }
    let base = base_url(host)?;
    let result = run_query(
        host,
        &base,
        expr,
        input.get("since").and_then(|v| v.as_str()),
        input.get("until").and_then(|v| v.as_str()),
        input.get("limit").and_then(|v| v.as_i64()),
        input.get("direction").and_then(|v| v.as_str()),
    )?;
    contribute_log_entries(host, &result);
    Ok(result)
}

fn recent_logs(input: Value, host: &mut Host) -> Result<Value, String> {
    let expr = build_recent_query(&input);
    let base = base_url(host)?;
    let result = run_query(
        host,
        &base,
        &expr,
        input.get("since").and_then(|v| v.as_str()),
        None,
        input.get("limit").and_then(|v| v.as_i64()),
        Some("backward"),
    )?;
    contribute_log_entries(host, &result);
    Ok(result)
}

fn metric(input: Value, host: &mut Host) -> Result<Value, String> {
    let expr = req_str(&input, "query")?.trim();
    if expr.is_empty() {
        return Err("`query` (string) required".into());
    }
    let base = base_url(host)?;
    let now = now_nanos();
    let end = parse_time_nanos(time_or(&input, "until", "0s"), now)?;
    let start = parse_time_nanos(time_or(&input, "since", "24h"), now)?;
    let step = match input
        .get("step")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) if is_prom_duration(s) => s.to_string(),
        Some(_) => return Err("`step` must be a duration like 15s, 5m, 1h, or 1d".into()),
        None => {
            let mut secs = (end - start).max(0) / 100 / 1_000_000_000;
            if secs < 15 {
                secs = 15;
            }
            format!("{secs}s")
        }
    };
    let params = [
        ("query", expr.to_string()),
        ("start", start.to_string()),
        ("end", end.to_string()),
        ("step", step.clone()),
    ];
    let resp = loki_get(host, &base, "/loki/api/v1/query_range", &params)?;
    if resp.get("status").and_then(|v| v.as_str()) != Some("success") {
        return Err(format!(
            "loki metric query failed with status {}",
            resp.get("status").and_then(|v| v.as_str()).unwrap_or("?")
        ));
    }
    let result_type = resp
        .pointer("/data/resultType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if result_type != "matrix" {
        return Err(format!(
            "query returned {result_type} — wrap the stream selector in a metric function like \
             sum(count_over_time({{...}}[1h]))"
        ));
    }
    let mut series: Vec<Value> = Vec::new();
    if let Some(results) = resp.pointer("/data/result").and_then(|v| v.as_array()) {
        for r in results {
            let labels = r.get("metric").cloned().unwrap_or_else(|| json!({}));
            let mut samples: Vec<Value> = Vec::new();
            if let Some(values) = r.get("values").and_then(|v| v.as_array()) {
                for pair in values {
                    let Some(a) = pair.as_array() else { continue };
                    if a.len() < 2 {
                        continue;
                    }
                    let (Some(ts), Some(raw)) = (a[0].as_f64(), a[1].as_str()) else {
                        continue;
                    };
                    let Ok(value) = raw.parse::<f64>() else {
                        continue;
                    };
                    samples.push(json!({ "timestamp": ts as i64, "value": value }));
                }
            }
            series.push(json!({ "labels": labels, "samples": samples }));
        }
    }
    let count = series.len() as i64;
    Ok(json!({
        "url": base,
        "normalized_query": expr,
        "step": step,
        "series": series,
        "count": count,
    }))
}

fn labels(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = base_url(host)?;
    let label = input
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if !label.is_empty() && !is_label_name(label) {
        return Err("`label` must be a valid Loki label name".into());
    }
    let path = if label.is_empty() {
        "/loki/api/v1/labels".to_string()
    } else {
        format!("/loki/api/v1/label/{}/values", urlencode(label))
    };
    let now = now_nanos();
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(q) = input
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.push(("query", q.to_string()));
    }
    if let Some(s) = input
        .get("since")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.push(("start", parse_time_nanos(s, now)?.to_string()));
    }
    if let Some(u) = input
        .get("until")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.push(("end", parse_time_nanos(u, now)?.to_string()));
    }
    let resp = loki_get(host, &base, &path, &params)?;
    if resp.get("status").and_then(|v| v.as_str()) != Some("success") {
        return Err(format!(
            "loki label query failed with status {}",
            resp.get("status").and_then(|v| v.as_str()).unwrap_or("?")
        ));
    }
    let mut values: Vec<String> = resp
        .get("data")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    values.sort();
    contribute_labels(host, label, &values);
    Ok(json!({ "url": base, "label": label, "values": values }))
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

/// Build a `query_range` stream query, parse the response into sorted log entries, and flag a full page
/// as `truncated`. Shared by `query` and `recent_logs`.
fn run_query(
    host: &mut Host,
    base: &str,
    query: &str,
    since: Option<&str>,
    until: Option<&str>,
    limit: Option<i64>,
    direction: Option<&str>,
) -> Result<Value, String> {
    let limit = clamp_limit(limit.unwrap_or(0));
    let now = now_nanos();
    let end = parse_time_nanos(or_default(until, "0s"), now)?;
    let start = parse_time_nanos(or_default(since, "1h"), now)?;
    let direction = normalize_direction(direction.unwrap_or("backward"))?;
    let params = [
        ("query", query.to_string()),
        ("start", start.to_string()),
        ("end", end.to_string()),
        ("limit", limit.to_string()),
        ("direction", direction.to_string()),
    ];
    let resp = loki_get(host, base, "/loki/api/v1/query_range", &params)?;
    if resp.get("status").and_then(|v| v.as_str()) != Some("success") {
        return Err(format!(
            "loki query failed with status {}",
            resp.get("status").and_then(|v| v.as_str()).unwrap_or("?")
        ));
    }
    let mut entries: Vec<Value> = Vec::new();
    if let Some(results) = resp.pointer("/data/result").and_then(|v| v.as_array()) {
        for stream in results {
            let stream_labels = stream.get("stream").cloned().unwrap_or_else(|| json!({}));
            let Some(values) = stream.get("values").and_then(|v| v.as_array()) else {
                continue;
            };
            for pair in values {
                let Some(a) = pair.as_array() else { continue };
                if a.len() < 2 {
                    continue;
                }
                // `raw_ts` is Loki's nanosecond string: it both feeds the SHA1 entry id (unchanged from the
                // reference) and is formatted to RFC3339Nano for the emitted/sorted `timestamp`.
                let raw_ts = a[0].as_str().unwrap_or("");
                let line = a[1].as_str().unwrap_or("").to_string();
                let id = entry_id(&stream_labels, raw_ts, &line);
                entries.push(json!({
                    "id": id,
                    "timestamp": format_ns_rfc3339(raw_ts),
                    "labels": stream_labels,
                    "line": line,
                }));
            }
        }
    }
    entries.sort_by(|a, b| {
        // Sort on the RFC3339Nano timestamp string (left-aligned fractions sort lexically = chronologically),
        // matching the reference's `entries[i].Timestamp < entries[j].Timestamp`.
        let ta = entry_ts(a);
        let tb = entry_ts(b);
        if direction == "forward" {
            ta.cmp(tb)
        } else {
            tb.cmp(ta)
        }
    });
    let count = entries.len() as i64;
    Ok(json!({
        "url": base,
        "normalized_query": query,
        "entries": entries,
        "count": count,
        "limit": limit,
        // A full page means Loki likely cut the result at limit — narrow the window or raise limit.
        "truncated": count >= limit,
    }))
}

/// The RFC3339Nano timestamp string of a parsed entry (for stable ordering). RFC3339Nano sorts lexically
/// in chronological order, so comparing these strings matches the reference's ordering.
fn entry_ts(entry: &Value) -> &str {
    entry
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// Build the LogQL selector for `recent_logs` from the label filters (sorted for a stable query) plus an
/// optional `contains` line filter.
fn build_recent_query(input: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for key in ["app", "namespace", "pod", "container"] {
        if let Some(v) = input
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(format!("{key}={}", logql_quote(v)));
        }
    }
    parts.sort();
    let mut query = if parts.is_empty() {
        "{job=~\".+\"}".to_string()
    } else {
        format!("{{{}}}", parts.join(","))
    };
    if let Some(c) = input
        .get("contains")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        query.push_str(&format!(" |= {}", logql_quote(c)));
    }
    query
}

/// A content-addressed id for a log entry, matching the fluxplane reference's scheme: the hex-encoded
/// SHA1 of `json(labels) + "\x00" + ts + "\x00" + line`, where `labels` is the stream's label map
/// serialized as a compact, **sorted-key** JSON object (Go's `json.Marshal(map[string]string)`) and `ts`
/// is the **raw nanosecond** timestamp string Loki returns (not the formatted RFC3339Nano value).
fn entry_id(labels: &Value, ts: &str, line: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(canonical_labels_json(labels).as_bytes());
    hasher.update(b"\x00");
    hasher.update(ts.as_bytes());
    hasher.update(b"\x00");
    hasher.update(line.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Serialize a stream's label object the way Go's `json.Marshal(map[string]string)` does: a compact JSON
/// object with keys sorted lexically (so the SHA1 entry id is stable and matches the reference). Non-object
/// values (Loki always sends an object) serialize as the empty object, matching an empty Go map's `{}`.
fn canonical_labels_json(labels: &Value) -> String {
    let map: BTreeMap<&str, &str> = labels
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s)))
                .collect()
        })
        .unwrap_or_default();
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
}

/// Format a raw unix-nanosecond timestamp string as RFC3339 with nanosecond precision (UTC), matching the
/// reference's `time.Unix(0, ns).UTC().Format(time.RFC3339Nano)`. A non-numeric input yields an empty
/// string (the reference's zero-time path renders nothing useful either); trailing-zero fractions are
/// trimmed, exactly like Go's RFC3339Nano.
fn format_ns_rfc3339(ns: &str) -> String {
    let Ok(nanos) = ns.trim().parse::<i128>() else {
        return String::new();
    };
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_default()
}

fn clamp_limit(limit: i64) -> i64 {
    if limit <= 0 {
        DEFAULT_LIMIT
    } else if limit > MAX_LIMIT {
        MAX_LIMIT
    } else {
        limit
    }
}

fn normalize_direction(direction: &str) -> Result<&'static str, String> {
    match direction.trim().to_ascii_lowercase().as_str() {
        "" | "backward" => Ok("backward"),
        "forward" => Ok("forward"),
        _ => Err("`direction` must be backward or forward".into()),
    }
}

/// Quote a string as a LogQL double-quoted literal (escaping `\` and `"`), matching Go's `strconv.Quote`
/// for the characters that appear in label values and line filters.
fn logql_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A valid Loki label name: `^[A-Za-z_][A-Za-z0-9_]*$`.
fn is_label_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

// ---------------------------------------------------------------------------
// Time parsing
// ---------------------------------------------------------------------------

fn now_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

/// The trimmed string at `key`, falling back to `default` when absent/blank.
fn time_or<'a>(input: &'a Value, key: &str, default: &'a str) -> &'a str {
    or_default(input.get(key).and_then(|v| v.as_str()), default)
}

fn or_default<'a>(value: Option<&'a str>, default: &'a str) -> &'a str {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
}

/// Resolve a time expression to absolute unix nanoseconds. Accepts: `now`/empty → `now`; a duration ago
/// (`30m`, `24h`, `1d`); unix seconds; or RFC3339.
fn parse_time_nanos(value: &str, now: i128) -> Result<i128, String> {
    let v = value.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("now") {
        return Ok(now);
    }
    if let Some(nanos) = parse_duration_nanos(v) {
        return Ok(now - nanos);
    }
    if let Ok(secs) = v.parse::<i64>() {
        return Ok(i128::from(secs) * 1_000_000_000);
    }
    if let Some(nanos) = parse_rfc3339_nanos(v) {
        return Ok(nanos);
    }
    Err(format!(
        "invalid time {v:?} — accepted: RFC3339 (2026-06-11T10:00:00Z), unix seconds, \
         duration ago (30m, 24h), or now"
    ))
}

/// Parse a single-unit duration (`<digits><unit>`, unit in ms/s/m/h/d/w/y) to nanoseconds.
fn parse_duration_nanos(s: &str) -> Option<i128> {
    const UNITS: [(&str, i128); 7] = [
        ("ms", 1_000_000),
        ("s", 1_000_000_000),
        ("m", 60 * 1_000_000_000),
        ("h", 3_600 * 1_000_000_000),
        ("d", 86_400 * 1_000_000_000),
        ("w", 604_800 * 1_000_000_000),
        ("y", 31_536_000 * 1_000_000_000),
    ];
    for (suffix, mult) in UNITS {
        if let Some(num) = s.strip_suffix(suffix) {
            if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
                return num.parse::<i128>().ok().map(|n| n * mult);
            }
        }
    }
    None
}

/// Whether `s` is a Prometheus-style step duration Loki accepts: `^[0-9]+(ms|s|m|h|d|w|y)$`.
fn is_prom_duration(s: &str) -> bool {
    for suffix in ["ms", "s", "m", "h", "d", "w", "y"] {
        if let Some(num) = s.strip_suffix(suffix) {
            return !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit());
        }
    }
    false
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]` to unix nanoseconds (UTC). Dep-free; rejects malformed
/// input by returning `None`.
fn parse_rfc3339_nanos(s: &str) -> Option<i128> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let two = |i: usize| -> Option<i64> { s.get(i..i + 2)?.parse().ok() };
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month = two(5)?;
    let day = two(8)?;
    let hour = two(11)?;
    let minute = two(14)?;
    let second = two(17)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut rest = &s[19..];
    let mut frac: i128 = 0;
    if let Some(r) = rest.strip_prefix('.') {
        let digits: String = r.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        rest = &r[digits.len()..];
        let mut padded = digits;
        if padded.len() > 9 {
            padded.truncate(9);
        }
        while padded.len() < 9 {
            padded.push('0');
        }
        frac = padded.parse::<i128>().ok()?;
    }
    let offset = if rest.is_empty() || rest.eq_ignore_ascii_case("z") {
        0
    } else {
        let sign = match rest.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let off = rest.get(1..)?;
        if off.len() < 5 || off.as_bytes()[2] != b':' {
            return None;
        }
        let oh: i64 = off.get(0..2)?.parse().ok()?;
        let om: i64 = off.get(3..5)?.parse().ok()?;
        sign * (oh * 3600 + om * 60)
    };
    let secs =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Some(i128::from(secs) * 1_000_000_000 + frac)
}

/// Days from the Unix epoch (1970-01-01) to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ---------------------------------------------------------------------------
// HTTP + auth
// ---------------------------------------------------------------------------

/// The Loki base URL (no default — must be configured).
fn base_url(host: &mut Host) -> Result<String, String> {
    Ok(host
        .endpoint("loki.endpoint")?
        .trim_end_matches('/')
        .to_string())
}

/// Decide the optional auth bits for a call: the Basic `basic_password` purpose (used only when a
/// password is configured) and the resolved `X-Scope-OrgID` tenant value (`tenant_id`, when configured).
/// Both absent → a plain unauthenticated request, matching how Loki is often deployed.
fn auth_bits(host: &mut Host) -> (Option<&'static str>, Option<String>) {
    let auth = if host.secret("basic_password").is_ok() {
        Some("basic_password")
    } else {
        None
    };
    let tenant = host.secret("tenant_id").ok();
    (auth, tenant)
}

/// GET a Loki API path with query params, injecting HTTP Basic creds (via `auth_purpose`) and the
/// `X-Scope-OrgID` tenant header when configured. Returns the parsed JSON body.
fn loki_get(
    host: &mut Host,
    base: &str,
    path: &str,
    params: &[(&str, String)],
) -> Result<Value, String> {
    let mut url = format!("{base}{path}");
    if !params.is_empty() {
        let qs: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencode(v)))
            .collect();
        url.push('?');
        url.push_str(&qs.join("&"));
    }
    let (auth, tenant) = auth_bits(host);
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(t) = tenant.as_deref() {
        headers.push(("X-Scope-OrgID", t));
    }
    let resp = host.http("GET", &url, auth, &headers, None)?;
    if !resp.is_success() {
        return Err(format!("loki GET {path} → {} {}", resp.status, resp.body));
    }
    resp.json()
}

/// Percent-encode a string so a query-string value (a LogQL expression, a time) is transmitted safely:
/// alnum and `-_.~` pass through, everything else becomes `%XX`.
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

// ---------------------------------------------------------------------------
// Datasource contribution
// ---------------------------------------------------------------------------

/// Contribute the parsed log entries as `loki.log_entry` records (title = pod/app/line, body = line).
fn contribute_log_entries(host: &mut Host, result: &Value) {
    let Some(entries) = result.get("entries").and_then(|v| v.as_array()) else {
        return;
    };
    let records: Vec<Record> = entries
        .iter()
        .filter_map(|e| {
            let id = e.get("id").and_then(|v| v.as_str())?;
            let line = e.get("line").and_then(|v| v.as_str()).unwrap_or("");
            let labels = e.get("labels");
            let title = labels
                .and_then(|l| l.get("pod"))
                .and_then(|v| v.as_str())
                .or_else(|| labels.and_then(|l| l.get("app")).and_then(|v| v.as_str()))
                .unwrap_or(line);
            Some(Record::new(
                Source::new("loki"),
                "loki.log_entry",
                id,
                title,
                line,
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

/// Contribute label names/values as `loki.label` records (id `<label>=<value>` for values, else `<name>`).
fn contribute_labels(host: &mut Host, label: &str, values: &[String]) {
    let records: Vec<Record> = values
        .iter()
        .map(|v| {
            let id = if label.is_empty() {
                v.clone()
            } else {
                format!("{label}={v}")
            };
            Record::new(Source::new("loki"), "loki.label", id.clone(), id, v.clone())
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
    fn test_checks_readiness() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http("/ready", json!("ready"));
        let out = plugin.call("loki.test", json!({}), &mut host).unwrap();
        assert_eq!(out["ready"], true);
        assert_eq!(out["url"], "https://loki.x");
    }

    #[test]
    fn query_runs_a_stream_query_and_contributes_entries() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "streams", "result": [
                    {"stream": {"app": "web", "pod": "web-1"}, "values": [["1710000000000000000", "boom"]]}
                ]}}),
            );
        let out = plugin
            .call(
                "loki.query",
                json!({ "query": "{app=\"web\"}", "since": "30m" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["normalized_query"], "{app=\"web\"}");
        assert_eq!(out["entries"][0]["line"], "boom");
        // The entry id is the reference's SHA1 scheme: hex(sha1(`{"app":"web","pod":"web-1"}` + \x00 +
        // raw-ns + \x00 + line)). Known vector, matches the fluxplane reference byte-for-byte.
        assert_eq!(
            out["entries"][0]["id"],
            "d5f985ea43d2f80b0cbec2c3134a87f9bf2c1bf3"
        );
        // The emitted timestamp is RFC3339Nano (raw ns 1710000000000000000 → no fractional part).
        assert_eq!(out["entries"][0]["timestamp"], "2024-03-09T16:00:00Z");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].entity, "loki.log_entry");
        assert_eq!(recs[0].title, "web-1");
        // The contributed record carries the same SHA1 id as the entry.
        assert_eq!(recs[0].id, "d5f985ea43d2f80b0cbec2c3134a87f9bf2c1bf3");
    }

    #[test]
    fn query_returns_an_empty_array_not_null_for_no_hits() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "streams", "result": []}}),
            );
        let out = plugin
            .call("loki.query", json!({ "query": "{app=\"web\"}" }), &mut host)
            .unwrap();
        assert_eq!(out["count"], 0);
        assert!(out["entries"].is_array());
    }

    #[test]
    fn query_works_with_basic_creds_and_tenant_configured() {
        // Exercises the auth_bits branches; MockHost ignores the injected auth/headers.
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_secret("basic_password", "s3cr3t")
            .with_secret("tenant_id", "acme")
            .with_http(
                "/loki/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "streams", "result": []}}),
            );
        let out = plugin
            .call("loki.query", json!({ "query": "{app=\"x\"}" }), &mut host)
            .unwrap();
        assert_eq!(out["count"], 0);
    }

    #[test]
    fn metric_parses_a_matrix_result() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "matrix", "result": [
                    {"metric": {"namespace": "core"}, "values": [[1710000000, "42"], [1710086400, "7"]]}
                ]}}),
            );
        let out = plugin
            .call(
                "loki.metric",
                json!({ "query": "sum(count_over_time({namespace=\"core\"} |= \"error\" [1d]))", "since": "720h", "step": "1d" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["step"], "1d");
        assert_eq!(out["series"][0]["labels"]["namespace"], "core");
        assert_eq!(out["series"][0]["samples"][0]["value"], 42.0);
    }

    #[test]
    fn metric_rejects_non_matrix_results() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "streams", "result": []}}),
            );
        let err = plugin
            .call("loki.metric", json!({ "query": "{app=\"x\"}" }), &mut host)
            .unwrap_err();
        assert!(err.contains("count_over_time"), "err = {err}");
    }

    #[test]
    fn metric_rejects_a_bad_step() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_endpoint("loki.endpoint", "https://loki.x");
        let err = plugin
            .call(
                "loki.metric",
                json!({ "query": "sum(x)", "step": "daily" }),
                &mut host,
            )
            .unwrap_err();
        assert!(err.contains("step"), "err = {err}");
    }

    #[test]
    fn labels_lists_and_sorts_names_and_contributes() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/labels",
                json!({"status": "success", "data": ["namespace", "app"]}),
            );
        let out = plugin.call("loki.labels", json!({}), &mut host).unwrap();
        assert_eq!(out["values"][0], "app");
        let recs = host.contributed.borrow();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].entity, "loki.label");
    }

    #[test]
    fn labels_fetches_values_for_a_named_label() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/label/app/values",
                json!({"status": "success", "data": ["web", "api"]}),
            );
        let out = plugin
            .call("loki.labels", json!({ "label": "app" }), &mut host)
            .unwrap();
        assert_eq!(out["label"], "app");
        assert_eq!(out["values"][0], "api");
    }

    #[test]
    fn labels_rejects_invalid_label_names() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_endpoint("loki.endpoint", "https://loki.x");
        let err = plugin
            .call("loki.labels", json!({ "label": "bad/name" }), &mut host)
            .unwrap_err();
        assert!(err.contains("valid Loki label name"), "err = {err}");
    }

    #[test]
    fn recent_logs_builds_a_selector_and_queries() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_endpoint("loki.endpoint", "https://loki.x")
            .with_http(
                "/loki/api/v1/query_range",
                json!({"status": "success", "data": {"resultType": "streams", "result": [
                    {"stream": {"app": "api"}, "values": [["1710000000000000000", "timeout!"]]}
                ]}}),
            );
        let out = plugin
            .call(
                "loki.recent_logs",
                json!({ "app": "api", "contains": "timeout" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["normalized_query"], "{app=\"api\"} |= \"timeout\"");
        assert_eq!(out["entries"][0]["line"], "timeout!");
        // SHA1 id over the single-label stream + RFC3339Nano timestamp (known vectors).
        assert_eq!(
            out["entries"][0]["id"],
            "1101ffdaaf6acf9433b7dee8217e54f99ac201bf"
        );
        assert_eq!(out["entries"][0]["timestamp"], "2024-03-09T16:00:00Z");
    }

    #[test]
    fn recent_query_sorts_labels() {
        let q =
            build_recent_query(&json!({"app": "api", "namespace": "prod", "contains": "error"}));
        assert_eq!(q, r#"{app="api",namespace="prod"} |= "error""#);
    }

    #[test]
    fn recent_query_escapes_quotes() {
        let q = build_recent_query(&json!({"app": "a\"b"}));
        assert_eq!(q, r#"{app="a\"b"}"#);
    }

    #[test]
    fn time_parsing_accepts_each_format() {
        assert_eq!(parse_time_nanos("now", 42), Ok(42));
        assert_eq!(parse_time_nanos("", 42), Ok(42));
        assert_eq!(parse_time_nanos("1h", 3_600_000_000_000), Ok(0));
        assert_eq!(
            parse_time_nanos("1700000000", 0),
            Ok(1_700_000_000_i128 * 1_000_000_000)
        );
        assert_eq!(
            parse_rfc3339_nanos("2021-01-01T00:00:00Z"),
            Some(1_609_459_200_i128 * 1_000_000_000)
        );
        let err = parse_time_nanos("yesterday", 0).unwrap_err();
        assert!(
            err.contains("RFC3339") && err.contains("now"),
            "err = {err}"
        );
    }

    #[test]
    fn entry_id_is_reference_sha1_over_labels_ts_line() {
        // Known vector against the fluxplane reference: hex(sha1(json(labels)+\x00+ts+\x00+line)) with
        // labels serialized as compact, sorted-key JSON.
        let labels = json!({"app": "web", "pod": "web-1"});
        assert_eq!(
            entry_id(&labels, "1710000000000000000", "boom"),
            "d5f985ea43d2f80b0cbec2c3134a87f9bf2c1bf3"
        );
        // Key order in the input object must not change the id — labels are canonicalized to sorted keys.
        let reordered = json!({"pod": "web-1", "app": "web"});
        assert_eq!(
            entry_id(&reordered, "1710000000000000000", "boom"),
            entry_id(&labels, "1710000000000000000", "boom"),
        );
        // The id is a 40-char lowercase hex SHA1 digest.
        let id = entry_id(&labels, "1710000000000000000", "boom");
        assert_eq!(id.len(), 40);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    #[test]
    fn format_ns_is_rfc3339nano() {
        // Whole second → no fractional part (Go's RFC3339Nano trims trailing zeros).
        assert_eq!(
            format_ns_rfc3339("1710000000000000000"),
            "2024-03-09T16:00:00Z"
        );
        // Sub-second nanoseconds are preserved at nanosecond precision.
        assert_eq!(
            format_ns_rfc3339("1710000000123456789"),
            "2024-03-09T16:00:00.123456789Z"
        );
        // Trailing zeros in the fraction are trimmed (500ms → .5, not .500000000).
        assert_eq!(
            format_ns_rfc3339("1710000000500000000"),
            "2024-03-09T16:00:00.5Z"
        );
        // Non-numeric input yields an empty string (defensive — Loki always sends a ns string).
        assert_eq!(format_ns_rfc3339("not-a-number"), "");
    }

    #[test]
    fn manifest_declares_ops_auth_and_datasources() {
        let m = manifest_builder().build().manifest();
        assert_eq!(m.operations.len(), 5);
        assert_eq!(m.auth[0].purpose, "basic_password");
        assert_eq!(m.auth[0].scheme, AuthScheme::Basic);
        assert_eq!(m.auth[1].purpose, "tenant_id");
        assert!(matches!(m.auth[1].scheme, AuthScheme::Header { .. }));
        assert_eq!(m.endpoints[0].name, "loki.endpoint");
        assert!(m.datasources.iter().any(|d| d.entity == "loki.log_entry"));
        assert!(m.datasources.iter().any(|d| d.entity == "loki.label"));
        assert!(m.operations.iter().all(|o| o.effects == vec![Effect::Read]));
    }
}

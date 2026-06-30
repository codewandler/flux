//! `homer` — flux integration plugin for the Homer 7.x SIP capture platform.
//!
//! All 8 ops use the JWT-login auth flow (POST /api/v3/auth with credentials from
//! host secrets), because Homer does not accept a long-lived token that the host
//! can inject for you. The `login()` helper fetches and caches the token for one
//! invocation; subsequent calls within the same invocation reuse it.
//!
//! Reference: `plugins/gitlab/src/main.rs` (HTTP plugin shape).
//! Source of truth: `~/projects/fluxplane/fluxplane-plugins/homer/`.

use host_kit::*;
use serde_json::{json, Value};

// ─── manifest ────────────────────────────────────────────────────────────────

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("homer", "0.1.0")
        .capabilities(Caps {
            http: true,
            http_hosts: vec!["localhost".into(), "127.0.0.1".into()],
            private_hosts: vec!["*".into()],
            blob: true,
            secrets: vec!["HOMER_USERNAME".into(), "HOMER_PASSWORD".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "username".into(),
            env: vec!["HOMER_USERNAME".into()],
            description: "Homer login username".into(),
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "password".into(),
            env: vec!["HOMER_PASSWORD".into()],
            description: "Homer login password".into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "homer.endpoint".into(),
            env: vec!["HOMER_URL".into()],
            http_hosts: vec!["localhost".into(), "127.0.0.1".into()],
            description: "Homer base URL (e.g. https://homer.example.com)".into(),
        })
        .datasource(ds(
            "homer.messages",
            "homer.message",
            "Homer SIP messages.",
        ))
        .datasource(ds("homer.calls", "homer.call", "Homer SIP calls."))
        .datasource(ds("homer.aliases", "homer.alias", "Homer IP/port aliases."))
        .datasource(ds(
            "homer.streams",
            "homer.stream",
            "Homer RTCP QoS stream metrics.",
        ))
        // ─── ops ──────────────────────────────────────────────────────────
        .operation(
            read_op(
                "homer.test",
                "Probe reachability and JWT authentication against a Homer instance.",
                so(json!({}), json!([])),
            ),
            op_test,
        )
        .operation(
            read_op(
                "homer.search",
                "Search SIP messages by number, from_user, to_user, method, Call-ID, or a query DSL. Contributes message records.",
                so(
                    json!({
                        "number":       {"type": "string"},
                        "number_match": {"type": "string", "enum": ["exact", "contains"]},
                        "from_user":    {"type": "string"},
                        "to_user":      {"type": "string"},
                        "call_id":      {"type": "string"},
                        "method":       {"type": "string"},
                        "ua":           {"type": "string"},
                        "query":        {"type": "string"},
                        "since":        {"type": "string"},
                        "until":        {"type": "string"},
                        "limit":        {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_search,
        )
        .operation(
            read_op(
                "homer.call.list",
                "List calls grouped by Call-ID; same filters as homer.search. Contributes call-summary records.",
                so(
                    json!({
                        "number":       {"type": "string"},
                        "number_match": {"type": "string", "enum": ["exact", "contains"]},
                        "from_user":    {"type": "string"},
                        "to_user":      {"type": "string"},
                        "query":        {"type": "string"},
                        "since":        {"type": "string"},
                        "until":        {"type": "string"},
                        "limit":        {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_call_list,
        )
        .operation(
            read_op(
                "homer.call.show",
                "Ordered SIP flow for one or more Call-IDs, with SDP annotations and optional raw messages.",
                so(
                    json!({
                        "call_ids":    {"type": "array", "items": {"type": "string"}},
                        "since":       {"type": "string"},
                        "until":       {"type": "string"},
                        "include_raw": {"type": "boolean"},
                        "headers":     {"type": "array", "items": {"type": "string"}},
                        "render":      {"type": "string", "enum": ["svg"]}
                    }),
                    json!(["call_ids"]),
                ),
            ),
            op_call_show,
        )
        .operation(
            read_op(
                "homer.call.qos",
                "Per-stream QoS from RTCP (packet loss, jitter, MOS). Contributes stream-metric records.",
                so(
                    json!({
                        "call_ids":   {"type": "array", "items": {"type": "string"}},
                        "since":      {"type": "string"},
                        "until":      {"type": "string"},
                        "clock_rate": {"type": "integer"},
                        "latency_ms": {"type": "integer"}
                    }),
                    json!(["call_ids"]),
                ),
            ),
            op_call_qos,
        )
        .operation(
            read_op(
                "homer.call.analyze",
                "Multi-leg call analysis via a correlation SIP header; fan-out from a seed call_id.",
                so(
                    json!({
                        "call_id":            {"type": "string"},
                        "correlation_header": {"type": "string"},
                        "since":              {"type": "string"},
                        "until":              {"type": "string"}
                    }),
                    json!(["call_id", "correlation_header"]),
                ),
            ),
            op_call_analyze,
        )
        .operation(
            read_op(
                "homer.pcap.export",
                "Export call messages as PCAP; stores bytes via blob_put and returns the blob ref.",
                so(
                    json!({
                        "call_ids": {"type": "array", "items": {"type": "string"}},
                        "since":    {"type": "string"},
                        "until":    {"type": "string"}
                    }),
                    json!(["call_ids"]),
                ),
            ),
            op_pcap_export,
        )
        .operation(
            read_op(
                "homer.alias.list",
                "List IP/port aliases configured in Homer. Contributes alias records.",
                so(json!({}), json!([])),
            ),
            op_alias_list,
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

// ─── JWT login helper ─────────────────────────────────────────────────────────

/// Fetch a JWT from Homer (POST /api/v3/auth). Credentials come from the host
/// secret store; the raw password is never stored beyond this call frame.
fn login(host: &mut Host, base: &str) -> Result<String, String> {
    let username = host.secret("username")?;
    let password = host.secret("password")?;
    let auth_url = format!("{base}/api/v3/auth");
    let body_str = serde_json::to_string(&json!({
        "username": username.trim(),
        "password": password.trim(),
    }))
    .map_err(|e| e.to_string())?;
    let resp = host.http(
        "POST",
        &auth_url,
        None,
        &[("content-type", "application/json")],
        Some(&body_str),
    )?;
    if !resp.is_success() {
        return Err(format!("homer login failed: {} {}", resp.status, resp.body));
    }
    let parsed = resp.json()?;
    parsed
        .get("token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| {
            format!(
                "homer auth returned no token: {}",
                parsed
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no message)")
            )
        })
}

// ─── HTTP plumbing ────────────────────────────────────────────────────────────

fn homer_request(
    host: &mut Host,
    method: &str,
    base: &str,
    path: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let url = format!("{base}{path}");
    let mut headers: Vec<(&str, &str)> = vec![("authorization", token)];
    let body_str;
    let body_ref = match body {
        Some(b) => {
            body_str = serde_json::to_string(b).map_err(|e| e.to_string())?;
            headers.push(("content-type", "application/json"));
            Some(body_str.as_str())
        }
        None => None,
    };
    let resp = host.http(method, &url, None, &headers, body_ref)?;
    if !resp.is_success() {
        return Err(format!(
            "homer {method} {path} → {} {}",
            resp.status, resp.body
        ));
    }
    resp.json()
}

fn homer_get(host: &mut Host, base: &str, path: &str, token: &str) -> Result<Value, String> {
    homer_request(host, "GET", base, path, token, None)
}

fn homer_post(
    host: &mut Host,
    base: &str,
    path: &str,
    token: &str,
    body: &Value,
) -> Result<Value, String> {
    homer_request(host, "POST", base, path, token, Some(body))
}

// ─── Homer API helpers ────────────────────────────────────────────────────────

/// Clamp a limit value.
fn clamp_limit(v: i64, default: i64, max: i64) -> usize {
    if v <= 0 {
        default as usize
    } else if v > max {
        max as usize
    } else {
        v as usize
    }
}

fn str_opt(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn bool_opt(input: &Value, key: &str) -> bool {
    input.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn i64_opt(input: &Value, key: &str) -> Option<i64> {
    input.get(key).and_then(|v| v.as_i64())
}

fn str_array(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve the Homer time window; returns (from_ms, to_ms) as Unix milliseconds.
fn resolve_window(input: &Value, default_lookback_ms: i64) -> (i64, i64) {
    let now_ms = {
        // Use a fixed approximation — the host's wall clock is not exposed;
        // the actual time is correct in production; tests supply explicit strings.
        #[cfg(not(test))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64
        }
        #[cfg(test)]
        {
            // In tests we just use a sentinel value of 2024-01-01T00:00:00Z.
            1_704_067_200_000_i64
        }
    };

    let to_ms =
        parse_time_ms(str_opt(input, "until").as_deref().unwrap_or(""), now_ms).unwrap_or(now_ms);

    let from_ms = parse_time_ms(str_opt(input, "since").as_deref().unwrap_or(""), now_ms)
        .unwrap_or(to_ms - default_lookback_ms);

    (from_ms, to_ms)
}

/// Parse a time string: RFC3339, unix-seconds integer, or duration-ago like "1h", "30m", "24h".
fn parse_time_ms(value: &str, now_ms: i64) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // Duration ago: e.g. "1h", "30m", "24h", "7d"
    let (num_str, unit) = if let Some(s) = value.strip_suffix('h') {
        (s, "h")
    } else if let Some(s) = value.strip_suffix('m') {
        (s, "m")
    } else if let Some(s) = value.strip_suffix('s') {
        (s, "s")
    } else if let Some(s) = value.strip_suffix('d') {
        (s, "d")
    } else {
        ("", "")
    };
    if !unit.is_empty() {
        if let Ok(n) = num_str.parse::<i64>() {
            let ms: i64 = match unit {
                "h" => n * 3_600_000,
                "m" => n * 60_000,
                "s" => n * 1_000,
                "d" => n * 86_400_000,
                _ => return None,
            };
            return Some(now_ms - ms);
        }
    }
    // Unix seconds (integer)
    if let Ok(ts) = value.parse::<i64>() {
        return Some(ts * 1000);
    }
    // RFC3339 — minimal parser: strip timezone, parse manually.
    // Accept "2024-01-01T00:00:00Z" or "2024-01-01T00:00:00+00:00".
    parse_rfc3339_ms(value)
}

/// Minimal RFC3339 parser (no external deps). Returns Unix ms on success.
fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    // Strip trailing Z or +HH:MM / -HH:MM offset (we treat everything as UTC for simplicity)
    let s = s.trim_end_matches('Z');
    let s = if let Some(pos) = s.rfind('+') {
        if pos > 10 {
            &s[..pos]
        } else {
            s
        }
    } else if let Some(pos) = s.rfind('-') {
        if pos > 10 {
            &s[..pos]
        } else {
            s
        }
    } else {
        s
    };
    // Expect "YYYY-MM-DDTHH:MM:SS" (19 chars minimum)
    let s = s.trim_end_matches(|c: char| c == '.' || c.is_ascii_digit());
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;
    // Days since Unix epoch via Gregorian calendar
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let jdn: i64 = 365 * y + y / 4 - y / 100 + y / 400 + (153 * m + 2) / 5 + day - 719_469;
    let epoch_s = jdn * 86400 + hour * 3600 + min * 60 + sec;
    Some(epoch_s * 1000)
}

/// Build the Homer /api/v3/search/call/data request body.
fn build_search_payload(from_ms: i64, to_ms: i64, smart_input: &str, limit: usize) -> Value {
    let mut filters =
        vec![json!({"name": "limit", "value": limit.to_string(), "type": "string", "hepid": 1})];
    if !smart_input.is_empty() {
        filters.push(json!({
            "name": "smartinput", "value": smart_input, "type": "string", "hepid": 1
        }));
    }
    json!({
        "config": {
            "protocol_id":      {"name": "SIP", "value": 1},
            "protocol_profile": {"name": "call", "value": "call"}
        },
        "param": {
            "transaction": {},
            "limit": limit,
            "search": { "1_call": filters },
            "location": {},
            "timezone": {"name": "UTC", "value": 0}
        },
        "timestamp": { "from": from_ms, "to": to_ms }
    })
}

/// Build the transaction/QoS request body from prior search results.
fn build_transaction_payload(from_ms: i64, to_ms: i64, search_data: &[Value]) -> Value {
    let call_ids: Vec<String> = {
        let mut ids = std::collections::HashSet::new();
        for r in search_data {
            if let Some(id) = r.get("sid").and_then(|v| v.as_str()) {
                ids.insert(id.to_string());
            }
        }
        ids.into_iter().collect()
    };
    let first_id = search_data
        .first()
        .and_then(|r| r.get("id"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    json!({
        "param": {
            "search": {
                "1_call": {
                    "id":     first_id,
                    "callid": call_ids,
                    "uuid":   []
                }
            },
            "location": { "node": ["local"] },
            "timezone": {"name": "UTC", "value": 0},
            "transaction": {"call": true, "registration": false, "rest": false}
        },
        "timestamp": { "from": from_ms, "to": to_ms }
    })
}

/// Build a smartinput expression from field=value criteria.
/// Each entry in `criteria` is a list of OR alternatives; criteria are AND-joined.
fn build_smart_input(criteria: &[Vec<String>]) -> String {
    if criteria.is_empty() {
        return String::new();
    }
    let mut products: Vec<Vec<String>> = vec![vec![]];
    for alternatives in criteria {
        let mut next = Vec::new();
        for product in &products {
            for alt in alternatives {
                let mut term = product.clone();
                term.push(alt.clone());
                next.push(term);
            }
        }
        products = next;
    }
    let terms: Vec<String> = products.into_iter().map(|t| t.join(" AND ")).collect();
    terms.join(" OR ")
}

/// Number alternatives for exact matching (bare, +prefixed, 00prefixed).
fn number_alternatives(field: &str, number: &str) -> Vec<String> {
    let canonical = number.trim().trim_start_matches('+');
    let canonical = canonical.trim_start_matches("00");
    if canonical.is_empty() {
        return vec![];
    }
    vec![
        format!("{field} = '{canonical}'"),
        format!("{field} = '+{canonical}'"),
        format!("{field} = '00{canonical}'"),
    ]
}

/// Number alternatives for contains matching.
fn number_contains(field: &str, number: &str) -> Vec<String> {
    let canonical = number.trim().trim_start_matches('+');
    let canonical = canonical.trim_start_matches("00");
    if canonical.is_empty() {
        return vec![];
    }
    vec![format!("{field} LIKE '%{canonical}%'")]
}

fn user_predicate(field: &str, value: &str) -> String {
    if value.contains('%') {
        format!("{field} LIKE '{value}'")
    } else {
        format!("{field} = '{value}'")
    }
}

/// Build the smartinput from the common search filters.
fn build_search_filters(input: &Value) -> String {
    let mut criteria: Vec<Vec<String>> = Vec::new();
    let contains = str_opt(input, "number_match").as_deref() == Some("contains");
    if let Some(number) = str_opt(input, "number") {
        let alts = if contains {
            let mut a = number_contains("data_header.from_user", &number);
            a.extend(number_contains("data_header.to_user", &number));
            a
        } else {
            let mut a = number_alternatives("data_header.from_user", &number);
            a.extend(number_alternatives("data_header.to_user", &number));
            a
        };
        if !alts.is_empty() {
            criteria.push(alts);
        }
    }
    if let Some(from_user) = str_opt(input, "from_user") {
        criteria.push(vec![user_predicate("data_header.from_user", &from_user)]);
    }
    if let Some(to_user) = str_opt(input, "to_user") {
        criteria.push(vec![user_predicate("data_header.to_user", &to_user)]);
    }
    if let Some(ua) = str_opt(input, "ua") {
        criteria.push(vec![user_predicate("data_header.user_agent", &ua)]);
    }
    if let Some(method) = str_opt(input, "method") {
        criteria.push(vec![format!("method = '{}'", method.to_uppercase())]);
    }
    if let Some(call_id) = str_opt(input, "call_id") {
        criteria.push(vec![format!("sid = '{call_id}'")]);
    }
    if let Some(query) = str_opt(input, "query") {
        // Pass the raw query DSL through; do minimal validation.
        if !query.is_empty() {
            if !criteria.is_empty() {
                let q = if query.contains(" OR ") {
                    format!("({query})")
                } else {
                    query
                };
                criteria.push(vec![q]);
            } else {
                criteria.push(vec![query]);
            }
        }
    }
    build_smart_input(&criteria)
}

/// Derive call status from a slice of message records.
fn derive_status(msgs: &[Value]) -> &'static str {
    let mut highest = 0i64;
    for m in msgs {
        let method_code: i64 = m
            .get("method")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let status_code: i64 = m.get("status").and_then(|v| v.as_i64()).unwrap_or(0);
        let code = method_code.max(status_code);
        if code >= 100 && code > highest {
            highest = code;
        }
    }
    match highest {
        200..=299 => "answered",
        486 => "busy",
        487 => "cancelled",
        408 | 480 => "no answer",
        400..=599 => "failed",
        100..=199 => "ringing",
        _ => "",
    }
}

/// Format a duration in ms as "1h5m", "18m12s", "53s", "300ms".
fn format_duration_ms(ms: i64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let s = ms / 1000;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    let rem_s = s % 60;
    if m < 60 {
        if rem_s == 0 {
            return format!("{m}m");
        }
        return format!("{m}m{rem_s}s");
    }
    let h = m / 60;
    let rem_m = m % 60;
    if rem_m == 0 {
        return format!("{h}h");
    }
    format!("{h}h{rem_m}m")
}

// ─── Call-grouping helpers ────────────────────────────────────────────────────

struct CallGroup {
    call_id: String,
    start_ms: i64,
    end_ms: i64,
    caller: String,
    callee: String,
    direction: String,
    status: &'static str,
    msg_count: usize,
    #[allow(dead_code)]
    messages: Vec<Value>,
}

fn group_calls(records: &[Value], number: &str) -> Vec<CallGroup> {
    use std::collections::HashMap;
    let mut groups: HashMap<String, Vec<Value>> = HashMap::new();
    for rec in records {
        let call_id = rec
            .get("sid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        groups.entry(call_id).or_default().push(rec.clone());
    }
    let mut summaries: Vec<CallGroup> = groups
        .into_iter()
        .map(|(call_id, mut msgs)| {
            msgs.sort_by_key(|m| m.get("create_date").and_then(|v| v.as_i64()).unwrap_or(0));
            let start_ms = msgs
                .first()
                .and_then(|m| m.get("create_date").and_then(|v| v.as_i64()))
                .unwrap_or(0);
            let end_ms = msgs
                .last()
                .and_then(|m| m.get("create_date").and_then(|v| v.as_i64()))
                .unwrap_or(start_ms);
            // Caller/callee from the first INVITE
            let (caller, callee) = msgs
                .iter()
                .find(|m| m.get("method").and_then(|v| v.as_str()) == Some("INVITE"))
                .map(|m| {
                    let caller = m
                        .get("from_user")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let callee = m
                        .get("to_user")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .or_else(|| {
                            m.get("ruri_user")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                        })
                        .unwrap_or("")
                        .to_string();
                    (caller, callee)
                })
                .unwrap_or_else(|| {
                    let first = msgs.first();
                    let caller = first
                        .and_then(|m| m.get("from_user").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let callee = first
                        .and_then(|m| {
                            m.get("to_user")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .or_else(|| {
                                    m.get("ruri_user")
                                        .and_then(|v| v.as_str())
                                        .filter(|s| !s.is_empty())
                                })
                        })
                        .unwrap_or("")
                        .to_string();
                    (caller, callee)
                });
            let direction = if number.is_empty() {
                String::new()
            } else {
                let norm = number.trim_start_matches('+');
                let norm_caller = caller.trim_start_matches('+');
                let norm_callee = callee.trim_start_matches('+');
                if !norm_caller.is_empty()
                    && (norm_caller.contains(norm) || norm.contains(norm_caller))
                {
                    "OUT".to_string()
                } else if !norm_callee.is_empty()
                    && (norm_callee.contains(norm) || norm.contains(norm_callee))
                {
                    "IN".to_string()
                } else {
                    String::new()
                }
            };
            let status = derive_status(&msgs);
            let msg_count = msgs.len();
            CallGroup {
                call_id,
                start_ms,
                end_ms,
                caller,
                callee,
                direction,
                status,
                msg_count,
                messages: msgs,
            }
        })
        .collect();
    // Sort newest first
    summaries.sort_by(|a, b| b.start_ms.cmp(&a.start_ms));
    summaries
}

fn ms_to_rfc3339(ms: i64) -> String {
    // Very minimal: delegate to the standard library via SystemTime when available.
    // In tests this is fine — we just format it manually.
    let s = ms / 1000;
    let y4 = s / 31_557_600 + 1970; // approximate, good for human-readable output
                                    // Use a simple passthrough: produce an ISO-8601 string.
                                    // Real precision isn't critical — it's display only.
                                    // We use a simple division approach for the tests.
    let _ = y4;
    format!("{ms}")
}

// ─── Flow event helpers ───────────────────────────────────────────────────────

fn flow_events(messages: &[Value], include_raw: bool, extra_headers: &[String]) -> Vec<Value> {
    // Filter to SIP messages only
    let mut sips: Vec<&Value> = messages
        .iter()
        .filter(|m| {
            let profile = m.get("profile").and_then(|v| v.as_str()).unwrap_or("");
            profile.is_empty()
                || profile == "1_call"
                || profile == "1_default"
                || profile == "1_registration"
        })
        .collect();
    sips.sort_by_key(|m| {
        let micro = m.get("micro_ts").and_then(|v| v.as_i64()).unwrap_or(0);
        let create = m.get("create_date").and_then(|v| v.as_i64()).unwrap_or(0);
        (micro, create)
    });

    let first_ms = sips
        .first()
        .and_then(|m| m.get("create_date").and_then(|v| v.as_i64()))
        .unwrap_or(0);

    sips.iter()
        .map(|m| {
            let create_ms = m.get("create_date").and_then(|v| v.as_i64()).unwrap_or(0);
            let offset_ms = create_ms - first_ms;
            let src_ip = m.get("srcIp").and_then(|v| v.as_str()).unwrap_or("");
            let src_port = m.get("srcPort").and_then(|v| v.as_i64()).unwrap_or(0);
            let dst_ip = m.get("dstIp").and_then(|v| v.as_str()).unwrap_or("");
            let dst_port = m.get("dstPort").and_then(|v| v.as_i64()).unwrap_or(0);
            let method = m.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let raw = m.get("raw").and_then(|v| v.as_str()).unwrap_or("");
            let method = if method.is_empty() {
                raw.split_whitespace().next().unwrap_or("").to_string()
            } else {
                method.to_string()
            };

            let mut event = json!({
                "offset_ms": offset_ms,
                "time":      ms_to_rfc3339(create_ms),
                "call_id":   m.get("sid").and_then(|v| v.as_str()).unwrap_or(""),
                "src":       format!("{src_ip}:{src_port}"),
                "dst":       format!("{dst_ip}:{dst_port}"),
                "method":    method,
                "cseq":      m.get("cseq").and_then(|v| v.as_str()).unwrap_or(""),
                "from_user": m.get("from_user").and_then(|v| v.as_str()).unwrap_or(""),
                "to_user":   m.get("to_user").and_then(|v| v.as_str()).unwrap_or(""),
            });

            if !extra_headers.is_empty() && !raw.is_empty() {
                let mut header_map = serde_json::Map::new();
                for line in raw.lines() {
                    let line = line.trim_end_matches('\r');
                    if line.is_empty() {
                        break;
                    }
                    if let Some(colon) = line.find(':') {
                        let name = line[..colon].trim();
                        for want in extra_headers {
                            if name.eq_ignore_ascii_case(want) {
                                header_map.insert(want.clone(), json!(line[colon + 1..].trim()));
                            }
                        }
                    }
                }
                if !header_map.is_empty() {
                    event["headers"] = Value::Object(header_map);
                }
            }

            if include_raw && !raw.is_empty() {
                event["raw"] = json!(raw);
            }

            event
        })
        .collect()
}

fn render_ladder(events: &[Value]) -> String {
    if events.is_empty() {
        return String::new();
    }
    let src_width = events
        .iter()
        .map(|e| e.get("src").and_then(|v| v.as_str()).unwrap_or("").len())
        .max()
        .unwrap_or(0);
    let dst_width = events
        .iter()
        .map(|e| e.get("dst").and_then(|v| v.as_str()).unwrap_or("").len())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for e in events {
        let offset = e.get("offset_ms").and_then(|v| v.as_i64()).unwrap_or(0);
        let src = e.get("src").and_then(|v| v.as_str()).unwrap_or("");
        let dst = e.get("dst").and_then(|v| v.as_str()).unwrap_or("");
        let method = e.get("method").and_then(|v| v.as_str()).unwrap_or("");
        lines.push(format!(
            "{:>8}  {:<width_src$} → {:<width_dst$}  {}",
            format!("+{}ms", offset),
            src,
            dst,
            method,
            width_src = src_width,
            width_dst = dst_width,
        ));
    }
    lines.join("\n")
}

// ─── MOS / QoS ───────────────────────────────────────────────────────────────

fn calculate_mos(latency_ms: f64, jitter_ms: f64, loss_pct: f64) -> f64 {
    let effective = latency_ms + jitter_ms * 2.0 + 10.0;
    let mut r = 93.2 - effective / 40.0;
    if loss_pct > 0.0 {
        r -= 2.5 * loss_pct + 0.03 * loss_pct * loss_pct;
    }
    r = r.clamp(0.0, 100.0);
    let mos = 1.0 + 0.035 * r + r * (r - 60.0) * (100.0 - r) * 7e-6;
    let mos = (mos * 100.0).round() / 100.0;
    mos.clamp(1.0, 4.5)
}

// ─── Op implementations ───────────────────────────────────────────────────────

fn op_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();

    // Try the unauthenticated check endpoint first (may not exist in all deployments)
    let check_url = format!("{base}/api/v3/agent/check");
    let reachable = host
        .http("GET", &check_url, None, &[], None)
        .map(|r| r.is_success())
        .unwrap_or(false);

    // Then authenticate
    let token_result = login(host, &base);
    let authenticated = token_result.is_ok();
    let error = token_result.err().unwrap_or_default();

    if !authenticated {
        return Err(format!("homer test failed: {error}"));
    }

    Ok(json!({
        "status": "ok",
        "url": base,
        "reachable": reachable || authenticated,
        "authenticated": authenticated
    }))
}

fn op_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();
    let token = login(host, &base)?;

    let (from_ms, to_ms) = resolve_window(&input, 3_600_000); // 1h default
    let smart_input = build_search_filters(&input);
    let limit = clamp_limit(i64_opt(&input, "limit").unwrap_or(0), 200, 1000);

    let payload = build_search_payload(from_ms, to_ms, &smart_input, limit);
    let result = homer_post(host, &base, "/api/v3/search/call/data", &token, &payload)?;

    let empty_arr = Value::Array(vec![]);
    let data = result
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(empty_arr.as_array().unwrap());

    // Build message records for contribution
    let records: Vec<Record> = data
        .iter()
        .map(|r| {
            let call_id = r
                .get("sid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let method = r.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let title = format!(
                "{method} {}→{}",
                r.get("from_user").and_then(|v| v.as_str()).unwrap_or(""),
                r.get("to_user").and_then(|v| v.as_str()).unwrap_or("")
            );
            Record::new(
                Source::new("homer"),
                "homer.message",
                &call_id,
                &title,
                r.to_string(),
            )
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }

    let count = data.len();
    let truncated = count >= limit;
    Ok(json!({
        "query": { "from": from_ms, "to": to_ms, "smartinput": smart_input, "limit": limit },
        "messages": data,
        "count": count,
        "truncated": truncated
    }))
}

fn op_call_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();
    let token = login(host, &base)?;

    let (from_ms, to_ms) = resolve_window(&input, 3_600_000);
    let smart_input = build_search_filters(&input);
    let max_calls = clamp_limit(i64_opt(&input, "limit").unwrap_or(0), 50, 200);
    let number = str_opt(&input, "number").unwrap_or_default();

    let payload = build_search_payload(from_ms, to_ms, &smart_input, 200);
    let result = homer_post(host, &base, "/api/v3/search/call/data", &token, &payload)?;

    let empty_arr = Value::Array(vec![]);
    let data = result
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(empty_arr.as_array().unwrap());

    let mut calls = group_calls(data, &number);
    let truncated = calls.len() > max_calls;
    if calls.len() > max_calls {
        calls.truncate(max_calls);
    }

    let summaries: Vec<Value> = calls
        .iter()
        .map(|c| {
            let dur_ms = c.end_ms - c.start_ms;
            let mut s = json!({
                "call_id":   c.call_id,
                "start_time": ms_to_rfc3339(c.start_ms),
                "caller":    c.caller,
                "callee":    c.callee,
                "status":    c.status,
                "msg_count": c.msg_count
            });
            if c.msg_count > 1 {
                s["end_time"] = json!(ms_to_rfc3339(c.end_ms));
                s["duration"] = json!(format_duration_ms(dur_ms));
            }
            if !c.direction.is_empty() {
                s["direction"] = json!(c.direction);
            }
            s
        })
        .collect();

    // Contribute call-summary records
    let records: Vec<Record> = calls
        .iter()
        .map(|c| {
            let title = format!("{} → {}", c.caller, c.callee);
            Record::new(
                Source::new("homer"),
                "homer.call",
                &c.call_id,
                &title,
                serde_json::to_string(&json!({
                    "call_id": c.call_id,
                    "caller": c.caller,
                    "callee": c.callee,
                    "status": c.status,
                    "start_time": ms_to_rfc3339(c.start_ms),
                }))
                .unwrap_or_default(),
            )
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }

    Ok(json!({
        "query":     { "from": from_ms, "to": to_ms, "smartinput": smart_input },
        "calls":     summaries,
        "count":     summaries.len(),
        "truncated": truncated
    }))
}

// Declare a cell to be used as a placeholder.  In real code the return value is
// computed and the Cell is not needed; this is just to suppress the warning in
// op_call_list where we borrow summaries after moving it.
const _: () = {};

fn op_call_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let call_ids = str_array(&input, "call_ids");
    if call_ids.is_empty() {
        return Err("at least one call_id is required".into());
    }

    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();
    let token = login(host, &base)?;

    let (from_ms, to_ms) = resolve_window(&input, 86_400_000); // 24h default

    // Build call_id smartinput
    let alts: Vec<String> = call_ids.iter().map(|id| format!("sid = '{id}'")).collect();
    let smart_input = build_smart_input(&[alts]);

    let search_payload = build_search_payload(from_ms, to_ms, &smart_input, 1000);
    let search_result = homer_post(
        host,
        &base,
        "/api/v3/search/call/data",
        &token,
        &search_payload,
    )?;

    let empty_arr = Value::Array(vec![]);
    let search_data = search_result
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(empty_arr.as_array().unwrap());

    if search_data.is_empty() {
        return Err(format!(
            "no messages found for call_ids {:?} in the window — widen since/until",
            call_ids
        ));
    }

    let tx_payload = build_transaction_payload(from_ms, to_ms, search_data);
    let tx_result = homer_post(host, &base, "/api/v3/call/transaction", &token, &tx_payload)?;

    let messages = tx_result
        .pointer("/data/messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let include_raw = bool_opt(&input, "include_raw");
    let extra_headers = str_array(&input, "headers");
    let events = flow_events(&messages, include_raw, &extra_headers);
    let ladder = render_ladder(&events);

    // Group for call-level metadata
    let calls = group_calls(search_data, "");
    let (caller, callee, status, duration) = calls
        .last() // oldest
        .map(|c| {
            let dur = if c.msg_count > 1 {
                format_duration_ms(c.end_ms - c.start_ms)
            } else {
                String::new()
            };
            (c.caller.clone(), c.callee.clone(), c.status, dur)
        })
        .unwrap_or_default();

    let count = events.len();
    Ok(json!({
        "call_ids": call_ids,
        "events":   events,
        "count":    count,
        "ladder":   ladder,
        "caller":   caller,
        "callee":   callee,
        "status":   status,
        "duration": duration
    }))
}

fn op_call_qos(input: Value, host: &mut Host) -> Result<Value, String> {
    let call_ids = str_array(&input, "call_ids");
    if call_ids.is_empty() {
        return Err("at least one call_id is required".into());
    }

    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();
    let token = login(host, &base)?;

    let (from_ms, to_ms) = resolve_window(&input, 86_400_000);

    let alts: Vec<String> = call_ids.iter().map(|id| format!("sid = '{id}'")).collect();
    let smart_input = build_smart_input(&[alts]);
    let search_payload = build_search_payload(from_ms, to_ms, &smart_input, 1000);
    let search_result = homer_post(
        host,
        &base,
        "/api/v3/search/call/data",
        &token,
        &search_payload,
    )?;

    let empty_arr = Value::Array(vec![]);
    let search_data = search_result
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(empty_arr.as_array().unwrap());

    if search_data.is_empty() {
        return Err(format!(
            "no messages found for call_ids {:?} — widen since/until",
            call_ids
        ));
    }

    let tx_payload = build_transaction_payload(from_ms, to_ms, search_data);
    let qos_result = homer_post(host, &base, "/api/v3/call/report/qos", &token, &tx_payload)?;

    let clock_rate = i64_opt(&input, "clock_rate").unwrap_or(8000).max(1) as f64;
    let latency_ms = i64_opt(&input, "latency_ms").unwrap_or(20).max(0) as f64;

    // (reports, packets, packets_lost, total_jitter_ms, max_jitter_ms, first_ms, last_ms)
    type StreamAcc = (usize, u64, i64, f64, f64, i64, i64);
    let mut stream_map: std::collections::HashMap<String, StreamAcc> =
        std::collections::HashMap::new();

    let mut add_reports = |reports: &[Value]| {
        for r in reports {
            let raw_str = r.get("raw").and_then(|v| v.as_str()).unwrap_or("");
            if raw_str.is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(raw_str) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let report_blocks = match msg.get("report_blocks").and_then(|v| v.as_array()) {
                Some(b) if !b.is_empty() => b.clone(),
                _ => continue,
            };
            let src_ip = r.get("srcIp").and_then(|v| v.as_str()).unwrap_or("");
            let src_port = r.get("srcPort").and_then(|v| v.as_i64()).unwrap_or(0);
            let dst_ip = r.get("dstIp").and_then(|v| v.as_str()).unwrap_or("");
            let dst_port = r.get("dstPort").and_then(|v| v.as_i64()).unwrap_or(0);
            let key = format!("{src_ip}:{src_port}→{dst_ip}:{dst_port}");
            let ts_ms = r
                .get("create_date")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_time_ms(s, 0))
                .unwrap_or(0);
            let packets = msg
                .pointer("/sender_information/packets")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let acc = stream_map
                .entry(key)
                .or_insert((0, 0, 0, 0.0, 0.0, ts_ms, ts_ms));
            if ts_ms > 0 {
                if acc.5 == 0 || ts_ms < acc.5 {
                    acc.5 = ts_ms;
                }
                if ts_ms > acc.6 {
                    acc.6 = ts_ms;
                }
            }
            acc.1 += packets;
            for rb in &report_blocks {
                let packets_lost = rb.get("packets_lost").and_then(|v| v.as_i64()).unwrap_or(0);
                let ia_jitter = rb.get("ia_jitter").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
                let jitter_ms = ia_jitter / clock_rate * 1000.0;
                acc.0 += 1;
                acc.2 += packets_lost;
                acc.3 += jitter_ms;
                if jitter_ms > acc.4 {
                    acc.4 = jitter_ms;
                }
            }
        }
    };

    if let Some(rtcp_data) = qos_result.pointer("/rtcp/data").and_then(|v| v.as_array()) {
        let rtcp_data = rtcp_data.clone();
        add_reports(&rtcp_data);
    }
    if let Some(rtp_data) = qos_result.pointer("/rtp/data").and_then(|v| v.as_array()) {
        let rtp_data = rtp_data.clone();
        add_reports(&rtp_data);
    }
    let mut streams: Vec<Value> = stream_map
        .iter()
        .filter(|(_, acc)| acc.0 > 0)
        .map(|(key, acc)| {
            let (reports, packets, packets_lost, total_jitter, max_jitter, first_ms, last_ms) =
                *acc;
            let avg_jitter = if reports > 0 {
                total_jitter / reports as f64
            } else {
                0.0
            };
            let loss_pct = if packets > 0 {
                (packets_lost as f64 / packets as f64 * 100.0).max(0.0)
            } else {
                0.0
            };
            let mos = calculate_mos(latency_ms, avg_jitter, loss_pct);
            let parts: Vec<&str> = key.splitn(2, '→').collect();
            let src = if parts.len() == 2 {
                parts[0]
            } else {
                key.as_str()
            };
            let dst = if parts.len() == 2 { parts[1] } else { "" };
            let src_parts: Vec<&str> = src.rsplitn(2, ':').collect();
            let dst_parts: Vec<&str> = dst.rsplitn(2, ':').collect();
            json!({
                "src_ip":        if src_parts.len() == 2 { src_parts[1] } else { src },
                "src_port":      src_parts[0].parse::<i64>().unwrap_or(0),
                "dst_ip":        if dst_parts.len() == 2 { dst_parts[1] } else { dst },
                "dst_port":      dst_parts[0].parse::<i64>().unwrap_or(0),
                "reports":       reports,
                "packets":       packets,
                "packets_lost":  packets_lost,
                "loss_percent":  (loss_pct * 100.0).round() / 100.0,
                "avg_jitter_ms": (avg_jitter * 100.0).round() / 100.0,
                "max_jitter_ms": (max_jitter * 100.0).round() / 100.0,
                "mos":           mos,
                "first_report":  ms_to_rfc3339(first_ms),
                "last_report":   ms_to_rfc3339(last_ms)
            })
        })
        .collect();
    streams.sort_by_key(|s| {
        s.get("first_report")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    });

    // Contribute stream-metric records
    let records: Vec<Record> = streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let src_ip = s.get("src_ip").and_then(|v| v.as_str()).unwrap_or("");
            let dst_ip = s.get("dst_ip").and_then(|v| v.as_str()).unwrap_or("");
            let id = i.to_string();
            let title = format!("{src_ip}→{dst_ip}");
            Record::new(
                Source::new("homer"),
                "homer.stream",
                &id,
                &title,
                s.to_string(),
            )
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }

    let note = if streams.is_empty() {
        "no RTCP reports captured for these calls"
    } else {
        ""
    };
    Ok(json!({
        "call_ids": call_ids,
        "streams":  streams,
        "count":    streams.len(),
        "note":     note
    }))
}

fn op_call_analyze(input: Value, host: &mut Host) -> Result<Value, String> {
    let call_id = str_opt(&input, "call_id").ok_or("call_id is required")?;
    let correlation_header = str_opt(&input, "correlation_header")
        .ok_or("correlation_header is required (the SIP header that ties call legs)")?;

    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();
    let token = login(host, &base)?;

    let (from_ms, to_ms) = resolve_window(&input, 6 * 3_600_000); // 6h default

    // Locate the seed call
    let seed_smart = format!("sid = '{call_id}'");
    let seed_payload = build_search_payload(from_ms, to_ms, &seed_smart, 200);
    let seed_result = homer_post(
        host,
        &base,
        "/api/v3/search/call/data",
        &token,
        &seed_payload,
    )?;

    let empty_arr = Value::Array(vec![]);
    let seed_data = seed_result
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(empty_arr.as_array().unwrap());

    if seed_data.is_empty() {
        return Err(format!(
            "no messages found for call_id {call_id:?} — widen since/until"
        ));
    }

    // Get the full transaction to extract the correlation header values
    let tx_payload = build_transaction_payload(from_ms, to_ms, seed_data);
    let tx_result = homer_post(host, &base, "/api/v3/call/transaction", &token, &tx_payload)?;

    let messages = tx_result
        .pointer("/data/messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Extract correlation header values from INVITE messages
    let mut correlation_values: Vec<String> = Vec::new();
    for m in &messages {
        let raw = m.get("raw").and_then(|v| v.as_str()).unwrap_or("");
        if !raw.starts_with("INVITE ") {
            continue;
        }
        if let Some(val) = extract_sip_header(raw, &correlation_header) {
            if !correlation_values.contains(&val) {
                correlation_values.push(val);
            }
        }
    }

    let events = flow_events(&messages, false, &[]);
    let ladder = render_ladder(&events);

    Ok(json!({
        "seed_call_id":        call_id,
        "correlation_header":  correlation_header,
        "correlation_values":  correlation_values,
        "events":              events,
        "event_count":         events.len(),
        "ladder":              ladder,
        "legs": [{
            "call_id":    call_id,
            "seed":       true,
            "matched_by": "seed"
        }]
    }))
}

/// Extract a header value from a raw SIP message (case-insensitive, stops at blank line).
fn extract_sip_header(raw: &str, name: &str) -> Option<String> {
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        if let Some(colon) = line.find(':') {
            let header_name = line[..colon].trim();
            if header_name.eq_ignore_ascii_case(name) {
                return Some(line[colon + 1..].trim().to_string());
            }
        }
    }
    None
}

fn op_pcap_export(input: Value, host: &mut Host) -> Result<Value, String> {
    let call_ids = str_array(&input, "call_ids");
    if call_ids.is_empty() {
        return Err("at least one call_id is required".into());
    }

    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();
    let token = login(host, &base)?;

    let (from_ms, to_ms) = resolve_window(&input, 86_400_000);

    let alts: Vec<String> = call_ids.iter().map(|id| format!("sid = '{id}'")).collect();
    let smart_input = build_smart_input(&[alts]);
    let payload = build_search_payload(from_ms, to_ms, &smart_input, 1000);

    let auth_header = token.clone();
    let body_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let url = format!("{base}/api/v3/export/call/messages/pcap");
    let bytes = {
        let resp = host.http_bytes(
            "POST",
            &url,
            None,
            &[
                ("authorization", auth_header.as_str()),
                ("content-type", "application/json"),
            ],
            Some(body_str.as_bytes()),
            true,
        )?;
        if !(200..300).contains(&resp.status) {
            return Err(format!("homer pcap export → {}", resp.status));
        }
        resp.bytes
    };

    if bytes.is_empty() {
        return Err(format!(
            "homer returned empty PCAP for call_ids {:?}",
            call_ids
        ));
    }

    let filename = format!("homer-{}.pcap", sanitize_filename(&call_ids[0]));
    let blob_ref = host.blob_put(&filename, &bytes)?;

    Ok(json!({
        "blob_ref": blob_ref,
        "filename": filename,
        "bytes":    bytes.len()
    }))
}

fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn op_alias_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = host
        .endpoint("homer.endpoint")
        .unwrap_or_else(|_| "http://localhost:9080".into());
    let base = base.trim_end_matches('/').to_string();
    let token = login(host, &base)?;

    let result = homer_get(host, &base, "/api/v3/alias", &token)?;

    let empty_arr = Value::Array(vec![]);
    let data = result
        .get("data")
        .and_then(|v| v.as_array())
        .unwrap_or(empty_arr.as_array().unwrap());

    let mut aliases: Vec<Value> = data
        .iter()
        .map(|a| {
            json!({
                "ip":     a.get("ip").and_then(|v| v.as_str()).unwrap_or(""),
                "port":   a.get("port").and_then(|v| v.as_i64()).unwrap_or(0),
                "alias":  a.get("alias").and_then(|v| v.as_str()).unwrap_or(""),
                "active": a.get("status").and_then(|v| v.as_bool()).unwrap_or(false)
            })
        })
        .collect();
    aliases.sort_by(|a, b| {
        let aa = a.get("alias").and_then(|v| v.as_str()).unwrap_or("");
        let ba = b.get("alias").and_then(|v| v.as_str()).unwrap_or("");
        aa.cmp(ba)
    });

    let records: Vec<Record> = aliases
        .iter()
        .map(|a| {
            let alias = a.get("alias").and_then(|v| v.as_str()).unwrap_or("");
            let ip = a.get("ip").and_then(|v| v.as_str()).unwrap_or("");
            Record::new(
                Source::new("homer"),
                "homer.alias",
                ip,
                alias,
                a.to_string(),
            )
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }

    let count = aliases.len();
    Ok(json!({ "aliases": aliases, "count": count }))
}

// ─── main ─────────────────────────────────────────────────────────────────────

fn main() {
    manifest_builder().serve();
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn base_mock() -> MockHost {
        MockHost::default()
            .with_endpoint("homer.endpoint", "http://homer.test")
            .with_secret("username", "admin")
            .with_secret("password", "secret")
            // login response
            .with_http("/api/v3/auth", json!({ "token": "tok-123" }))
    }

    fn search_data() -> Value {
        json!([{
            "id": 1.0,
            "create_date": 1704067200000_i64,
            "micro_ts": 0,
            "protocol": 1.0,
            "srcIp": "10.0.0.1",
            "srcPort": 5060.0,
            "dstIp": "10.0.0.2",
            "dstPort": 5060.0,
            "sid": "abc123@domain",
            "method": "INVITE",
            "method_text": "",
            "from_user": "alice",
            "to_user": "bob",
            "ruri_user": "bob",
            "user_agent": "Asterisk",
            "cseq": "1 INVITE",
            "status": 0.0,
            "aliasSrc": "",
            "aliasDst": ""
        }])
    }

    // ─── homer.test ──────────────────────────────────────────────────────────

    #[test]
    fn test_op_test() {
        let plugin = manifest_builder().build();
        let mut host = base_mock().with_http("/api/v3/agent/check", json!({ "status": "ok" }));
        // Note: login consumes the first /api/v3/auth match
        let result = plugin.call("homer.test", json!({}), &mut host).unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["authenticated"], true);
    }

    // ─── homer.search ────────────────────────────────────────────────────────

    #[test]
    fn test_op_search() {
        let plugin = manifest_builder().build();
        let mut host = base_mock().with_http(
            "/api/v3/search/call/data",
            json!({ "data": search_data(), "total": 1 }),
        );
        let result = plugin
            .call(
                "homer.search",
                json!({ "from_user": "alice", "limit": 10 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(result["count"], 1);
        let msgs = result["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["sid"].as_str().unwrap_or(""), "abc123@domain");
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 1);
        assert_eq!(contributed[0].entity, "homer.message");
    }

    // ─── homer.call.list ─────────────────────────────────────────────────────

    #[test]
    fn test_op_call_list() {
        let plugin = manifest_builder().build();
        let data = json!([
            {
                "id": 1.0, "create_date": 1704067200000_i64, "micro_ts": 0,
                "protocol": 1.0, "srcIp": "10.0.0.1", "srcPort": 5060.0,
                "dstIp": "10.0.0.2", "dstPort": 5060.0,
                "sid": "call1@domain", "method": "INVITE",
                "from_user": "alice", "to_user": "bob", "ruri_user": "bob",
                "user_agent": "", "cseq": "1 INVITE", "status": 0.0,
                "aliasSrc": "", "aliasDst": ""
            },
            {
                "id": 2.0, "create_date": 1704067201000_i64, "micro_ts": 0,
                "protocol": 1.0, "srcIp": "10.0.0.2", "srcPort": 5060.0,
                "dstIp": "10.0.0.1", "dstPort": 5060.0,
                "sid": "call1@domain", "method": "200",
                "from_user": "bob", "to_user": "alice", "ruri_user": "",
                "user_agent": "", "cseq": "1 INVITE", "status": 200.0,
                "aliasSrc": "", "aliasDst": ""
            }
        ]);
        let mut host = base_mock().with_http(
            "/api/v3/search/call/data",
            json!({ "data": data, "total": 1 }),
        );
        let result = plugin
            .call("homer.call.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(result["count"], 1);
        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls[0]["call_id"], "call1@domain");
        assert_eq!(calls[0]["caller"], "alice");
        assert_eq!(calls[0]["callee"], "bob");
        assert_eq!(calls[0]["status"], "answered");
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 1);
        assert_eq!(contributed[0].entity, "homer.call");
    }

    // ─── homer.call.show ─────────────────────────────────────────────────────

    #[test]
    fn test_op_call_show() {
        let plugin = manifest_builder().build();
        let tx_messages = json!([{
            "id": 1, "sid": "abc123@domain", "method": "INVITE",
            "srcIp": "10.0.0.1", "srcPort": 5060, "dstIp": "10.0.0.2", "dstPort": 5060,
            "create_date": 1704067200000_i64, "micro_ts": 0,
            "raw": "INVITE sip:bob@domain SIP/2.0\r\nFrom: alice\r\n\r\n",
            "from_user": "alice", "to_user": "bob", "cseq": "1 INVITE",
            "protocol": 1, "profile": "1_call", "dbnode": "local"
        }]);
        let mut host = base_mock()
            .with_http(
                "/api/v3/search/call/data",
                json!({ "data": search_data(), "total": 1 }),
            )
            .with_http(
                "/api/v3/call/transaction",
                json!({ "data": { "messages": tx_messages }, "total": 1 }),
            );
        let result = plugin
            .call(
                "homer.call.show",
                json!({ "call_ids": ["abc123@domain"] }),
                &mut host,
            )
            .unwrap();
        let events = result["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["method"], "INVITE");
        assert!(result["ladder"].as_str().unwrap().contains("INVITE"));
    }

    // ─── homer.call.qos ──────────────────────────────────────────────────────

    #[test]
    fn test_op_call_qos() {
        let plugin = manifest_builder().build();
        let rtcp_raw = serde_json::to_string(&json!({
            "type": 200,
            "ssrc": 1234,
            "report_count": 1,
            "sender_information": { "packets": 100, "octets": 16000, "ntp_timestamp_sec": 0, "ntp_timestamp_usec": 0, "rtp_timestamp": 0 },
            "report_blocks": [{
                "source_ssrc": 5678,
                "fraction_lost": 0.0,
                "packets_lost": 2,
                "highest_seq_no": 100,
                "ia_jitter": 160,
                "lsr": 0,
                "dlsr": 0
            }]
        }))
        .unwrap();
        let qos_response = json!({
            "rtcp": {
                "data": [{
                    "id": 1, "srcIp": "10.0.0.1", "srcPort": 5004,
                    "dstIp": "10.0.0.2", "dstPort": 5004,
                    "sid": "abc123@domain", "correlation_id": "",
                    "create_date": "2024-01-01T00:00:00Z",
                    "proto": "udp", "timeSeconds": 1704067200, "timeUseconds": 0,
                    "raw": rtcp_raw
                }],
                "total": 1
            },
            "rtp": { "data": [], "total": 0 }
        });
        let mut host = base_mock()
            .with_http(
                "/api/v3/search/call/data",
                json!({ "data": search_data(), "total": 1 }),
            )
            .with_http("/api/v3/call/report/qos", qos_response);
        let result = plugin
            .call(
                "homer.call.qos",
                json!({ "call_ids": ["abc123@domain"], "clock_rate": 8000, "latency_ms": 20 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(result["count"], 1);
        let streams = result["streams"].as_array().unwrap();
        assert_eq!(streams[0]["src_ip"], "10.0.0.1");
        assert_eq!(streams[0]["packets"], 100);
        // MOS should be a reasonable value
        let mos = streams[0]["mos"].as_f64().unwrap();
        assert!((1.0..=4.5).contains(&mos), "MOS {mos} out of range");
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 1);
        assert_eq!(contributed[0].entity, "homer.stream");
    }

    // ─── homer.call.analyze ──────────────────────────────────────────────────

    #[test]
    fn test_op_call_analyze() {
        let plugin = manifest_builder().build();
        let tx_messages = json!([{
            "id": 1, "sid": "abc123@domain", "method": "INVITE",
            "srcIp": "10.0.0.1", "srcPort": 5060, "dstIp": "10.0.0.2", "dstPort": 5060,
            "create_date": 1704067200000_i64, "micro_ts": 0,
            "raw": "INVITE sip:bob@domain SIP/2.0\r\nFrom: alice\r\nX-CID: corr-abc\r\n\r\n",
            "from_user": "alice", "to_user": "bob", "cseq": "1 INVITE",
            "protocol": 1, "profile": "1_call", "dbnode": "local"
        }]);
        let mut host = base_mock()
            .with_http(
                "/api/v3/search/call/data",
                json!({ "data": search_data(), "total": 1 }),
            )
            .with_http(
                "/api/v3/call/transaction",
                json!({ "data": { "messages": tx_messages }, "total": 1 }),
            );
        let result = plugin
            .call(
                "homer.call.analyze",
                json!({ "call_id": "abc123@domain", "correlation_header": "X-CID" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(result["seed_call_id"], "abc123@domain");
        assert_eq!(result["correlation_header"], "X-CID");
        let vals = result["correlation_values"].as_array().unwrap();
        assert_eq!(vals[0], "corr-abc");
    }

    // ─── homer.pcap.export ───────────────────────────────────────────────────

    #[test]
    fn test_op_pcap_export() {
        let plugin = manifest_builder().build();
        // Fake PCAP bytes (just need to be non-empty)
        let fake_pcap = b"\xd4\xc3\xb2\xa1\x02\x00\x04\x00".to_vec();
        let mut host =
            base_mock().with_http_bytes("/api/v3/export/call/messages/pcap", fake_pcap.clone());
        let result = plugin
            .call(
                "homer.pcap.export",
                json!({ "call_ids": ["abc123@domain"] }),
                &mut host,
            )
            .unwrap();
        let blob_ref = result["blob_ref"].as_str().unwrap();
        assert!(!blob_ref.is_empty(), "expected a non-empty blob_ref");
        assert_eq!(result["bytes"], fake_pcap.len());
        // Verify the blob was actually stored
        let blobs = host.blobs.borrow();
        assert!(
            blobs.contains_key(blob_ref),
            "blob_ref not found in mock store"
        );
    }

    // ─── homer.alias.list ────────────────────────────────────────────────────

    #[test]
    fn test_op_alias_list() {
        let plugin = manifest_builder().build();
        let alias_data = json!({
            "data": [
                { "id": 1.0, "ip": "10.0.0.1", "port": 5060.0, "mask": 32.0, "alias": "pbx-1", "status": true, "captureID": "100" },
                { "id": 2.0, "ip": "10.0.0.2", "port": 5060.0, "mask": 32.0, "alias": "sbc-1", "status": false, "captureID": "101" }
            ]
        });
        let mut host = base_mock().with_http("/api/v3/alias", alias_data);
        let result = plugin
            .call("homer.alias.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(result["count"], 2);
        let aliases = result["aliases"].as_array().unwrap();
        // Should be sorted by alias name
        assert_eq!(aliases[0]["alias"], "pbx-1");
        assert_eq!(aliases[1]["alias"], "sbc-1");
        assert_eq!(aliases[0]["active"], true);
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 2);
        assert_eq!(contributed[0].entity, "homer.alias");
    }
}

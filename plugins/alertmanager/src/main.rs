//! `alertmanager` — a flux integration plugin for the Alertmanager v2 API: readiness, active alert
//! listing with filter/state params, and silence management (list/create/delete).
//!
//! Auth is HTTP Basic but **optional** — many Alertmanager instances are unauthenticated. At request
//! time the plugin probes `host.secret("basic")` and passes the auth purpose only when creds are
//! configured. The base URL comes from the `alertmanager.endpoint` (env `ALERTMANAGER_URL`).
//!
//! Alert list ops contribute `alertmanager.alert` datasource records so the agent can search them.

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

// ===========================================================================
// Schema-only op input structs (D-36)
// ===========================================================================
// Each op's `input_schema` is derived from the typed structs below via schemars
// (`host_kit::read_op_typed::<T>` / `write_op_typed::<T>`) instead of a hand-written
// `json!({...})` literal, so the schema the model sees cannot drift from the handler
// contract. Handlers keep their existing `Value` extraction (D-34 schema-only precedent).

/// `alertmanager.test`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct TestInput {}

/// `alertmanager.alerts`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct AlertsInput {
    /// Label matchers e.g. ["severity=\"critical\"", "namespace=~\"prod-.*\""]
    filter: Option<Vec<String>>,
    /// Include active alerts (default true).
    active: Option<bool>,
    /// Include silenced alerts (default false).
    silenced: Option<bool>,
    /// Include inhibited alerts (default false).
    inhibited: Option<bool>,
    /// Maximum alerts to return (default 200).
    limit: Option<i64>,
}

/// Silence state filter.
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum SilenceState {
    Active,
    Pending,
    Expired,
}

/// `alertmanager.silence.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SilenceListInput {
    /// Filter by silence state.
    state: Option<SilenceState>,
}

/// A label matcher for a silence.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SilenceMatcherInput {
    /// Label name to match.
    name: String,
    /// Value (or regex when is_regex) to match.
    value: String,
    /// Treat value as a regular expression.
    is_regex: Option<bool>,
    /// Equality matcher. Defaults to true; false negates.
    is_equal: Option<bool>,
}

/// `alertmanager.silence.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SilenceCreateInput {
    /// Label matchers selecting the alerts to silence.
    matchers: Vec<SilenceMatcherInput>,
    /// Duration from now e.g. 30m, 2h (default 1h). Ignored when ends_at is set.
    duration: Option<String>,
    /// Explicit RFC3339 end time. Overrides duration.
    ends_at: Option<String>,
    /// Why this silence exists (shown in the Alertmanager UI).
    comment: String,
    /// Creator label (default flux-plugin).
    created_by: Option<String>,
}

/// `alertmanager.silence.delete`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SilenceDeleteInput {
    /// Silence id to expire.
    id: String,
}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("alertmanager", "0.1.0")
        .capabilities(Caps {
            http: true,
            private_hosts: vec!["*".into()],
            secrets: vec!["ALERTMANAGER_PASSWORD".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "basic".into(),
            scheme: AuthScheme::Basic,
            env: vec!["ALERTMANAGER_PASSWORD".into()],
            user_env: vec!["ALERTMANAGER_USERNAME".into()],
            description: "HTTP Basic auth (optional — Alertmanager is often unauthenticated)."
                .into(),
        })
        .endpoint(EndpointSpec {
            name: "alertmanager.endpoint".into(),
            env: vec!["ALERTMANAGER_URL".into()],
            http_hosts: Vec::new(),
            description: "Alertmanager base URL (e.g. http://alertmanager.example.com:9093)".into(),
        })
        .datasource(Declaration {
            name: "alertmanager.alerts".into(),
            entity: "alertmanager.alert".into(),
            description: Some("Active Alertmanager alerts.".into()),
            capabilities: vec!["search".into(), "get".into(), "index".into()],
            entity_schema: None,
        })
        // ---- ops ----
        .operation(
            read_op_typed::<TestInput>(
                "alertmanager.test",
                "Check Alertmanager readiness and return version/cluster status.",
            ),
            test,
        )
        .operation(
            read_op_typed::<AlertsInput>(
                "alertmanager.alerts",
                "List alerts from Alertmanager with optional label matchers and state filters (active/silenced/inhibited).",
            ),
            alerts,
        )
        .operation(
            read_op_typed::<SilenceListInput>(
                "alertmanager.silence.list",
                "List silences with their matchers, state (active/pending/expired), creator, and comment.",
            ),
            silence_list,
        )
        .operation(
            write_op_typed::<SilenceCreateInput>(
                "alertmanager.silence.create",
                "Create a silence: label matchers, duration or explicit end time, creator, and comment.",
            ),
            silence_create,
        )
        .operation(
            write_op_typed::<SilenceDeleteInput>(
                "alertmanager.silence.delete",
                "Expire (delete) a silence by id.",
            ),
            silence_delete,
        )
}

// ---------------------------------------------------------------------------
// HTTP plumbing.
// ---------------------------------------------------------------------------

/// `auth_purpose` — `Some("basic")` when creds are present, `None` when not.
fn am_auth(host: &mut Host) -> Option<&'static str> {
    if host.secret("basic").is_ok() {
        Some("basic")
    } else {
        None
    }
}

fn am_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let auth = am_auth(host);
    host.get_json_ref("alertmanager.endpoint", path, auth)
}

fn am_post(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    let auth = am_auth(host);
    host.send_json_ref("alertmanager.endpoint", "POST", path, auth, body)
}

/// DELETE request — Alertmanager replies 200 with no body.
fn am_delete(host: &mut Host, path: &str) -> Result<(), String> {
    let auth = am_auth(host);
    let resp = host.http_ref("alertmanager.endpoint", "DELETE", path, auth, None)?;
    if !resp.is_success() {
        return Err(format!("DELETE {path} → {} {}", resp.status, resp.body));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Parse a Go-style duration string (e.g. `30m`, `2h`, `1h30m`) into seconds.
/// Supported units: s, m, h, d (days = 86400 s). Compound durations like `1h30m` are supported.
fn parse_go_duration_secs(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".into());
    }
    let mut total: u64 = 0;
    let mut num_start = 0usize;
    let mut in_num = false;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            if !in_num {
                num_start = i;
                in_num = true;
            }
        } else if c.is_ascii_alphabetic() {
            if !in_num {
                return Err(format!("invalid duration {s:?}: unit without number"));
            }
            let n: u64 = s[num_start..i]
                .parse()
                .map_err(|_| format!("invalid duration {s:?}"))?;
            let mul = match c {
                's' => 1,
                'm' => 60,
                'h' => 3600,
                'd' => 86400,
                other => return Err(format!("unknown duration unit {other:?} in {s:?}")),
            };
            total += n * mul;
            in_num = false;
        } else {
            return Err(format!("invalid char in duration {s:?}"));
        }
    }
    if in_num {
        // trailing number without unit
        return Err(format!("missing unit in duration {s:?}"));
    }
    if total == 0 {
        return Err(format!("zero duration {s:?}"));
    }
    Ok(total)
}

/// Emit records contributed per alert fingerprint.
fn contribute_alerts(host: &mut Host, alerts: &[Value]) {
    let records: Vec<Record> = alerts
        .iter()
        .filter_map(|a| {
            let fp = a.get("fingerprint")?.as_str()?;
            let alertname = a
                .get("labels")
                .and_then(|l| l.get("alertname"))
                .and_then(|v| v.as_str())
                .unwrap_or(fp);
            Some(Record::new(
                Source::new("alertmanager"),
                "alertmanager.alert",
                fp,
                alertname,
                a.to_string(),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

// ---------------------------------------------------------------------------
// Op handlers.
// ---------------------------------------------------------------------------

fn test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let start = std::time::Instant::now();
    let raw = am_get(host, "/api/v2/status")?;
    let latency_ms: i64 = start.elapsed().as_millis().try_into().unwrap_or(i64::MAX);
    let version = raw
        .get("versionInfo")
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cluster_status = raw
        .get("cluster")
        .and_then(|c| c.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let peers = raw
        .get("cluster")
        .and_then(|c| c.get("peers"))
        .and_then(|p| p.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Ok(json!({
        "url": "alertmanager.endpoint",
        "ready": true,
        "version": version,
        "cluster_status": cluster_status,
        "cluster_peers": peers,
        "latency_ms": latency_ms
    }))
}

fn alerts(input: Value, host: &mut Host) -> Result<Value, String> {
    // Build query string manually — the host's http path doesn't know Alertmanager's
    // multi-value `filter` param, so we construct the query ourselves and carry it in the path.
    let active = input
        .get("active")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let silenced = input
        .get("silenced")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let inhibited = input
        .get("inhibited")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = input
        .get("limit")
        .and_then(|v| v.as_i64())
        .filter(|&n| n > 0)
        .unwrap_or(200) as usize;

    let mut qs = format!(
        "active={}&silenced={}&inhibited={}",
        active, silenced, inhibited
    );
    if let Some(arr) = input.get("filter").and_then(|v| v.as_array()) {
        for f in arr {
            if let Some(s) = f.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    qs.push_str("&filter=");
                    qs.push_str(&percent_encode(s));
                }
            }
        }
    }

    let auth = am_auth(host);
    let wire = host.get_json_ref(
        "alertmanager.endpoint",
        &format!("/api/v2/alerts?{qs}"),
        auth,
    )?;

    let wire_alerts = wire.as_array().cloned().unwrap_or_default();
    let truncated = wire_alerts.len() > limit;
    let taken: Vec<Value> = wire_alerts.into_iter().take(limit).collect();

    // Normalise the wire shape (camelCase → snake_case fields).
    let alerts: Vec<Value> = taken
        .iter()
        .map(|a| {
            json!({
                "fingerprint": a.get("fingerprint"),
                "labels":      a.get("labels"),
                "annotations": a.get("annotations"),
                "state":       a.get("status").and_then(|s| s.get("state")),
                "silenced_by": a.get("status").and_then(|s| s.get("silencedBy")).unwrap_or(&json!([])),
                "inhibited_by": a.get("status").and_then(|s| s.get("inhibitedBy")).unwrap_or(&json!([])),
                "starts_at":   a.get("startsAt"),
                "ends_at":     a.get("endsAt"),
                "generator_url": a.get("generatorURL")
            })
        })
        .collect();

    contribute_alerts(host, &alerts);

    Ok(json!({
        "url": "alertmanager.endpoint",
        "alerts": alerts,
        "count": alerts.len(),
        "truncated": truncated
    }))
}

fn silence_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let wire = am_get(host, "/api/v2/silences")?;
    let state_filter = input
        .get("state")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);

    let silences: Vec<Value> = wire
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| {
            if let Some(ref filter) = state_filter {
                s.get("status")
                    .and_then(|st| st.get("state"))
                    .and_then(|v| v.as_str())
                    .map(|st| st == filter.as_str())
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .map(|s| normalise_silence(&s))
        .collect();

    Ok(json!({
        "url": "alertmanager.endpoint",
        "silences": silences,
        "count": silences.len()
    }))
}

fn silence_create(input: Value, host: &mut Host) -> Result<Value, String> {
    // Validate matchers.
    let raw_matchers = input
        .get("matchers")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`matchers` (non-empty array) required")?;

    let comment = input
        .get("comment")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("`comment` required — say why the silence exists")?;

    // Compute endsAt.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let ends_at: String = if let Some(ea) = input
        .get("ends_at")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Validate RFC3339.
        if !looks_like_rfc3339(ea) {
            return Err(format!("invalid ends_at {ea:?} — RFC3339 required"));
        }
        ea.to_string()
    } else {
        let duration_str = input
            .get("duration")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("1h");
        let secs = parse_go_duration_secs(duration_str)?;
        unix_secs_to_rfc3339(now_secs + secs)
    };

    let starts_at = unix_secs_to_rfc3339(now_secs);

    let created_by = input
        .get("created_by")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("flux-plugin");

    // Build wire matchers.
    let mut matchers: Vec<Value> = Vec::new();
    for (i, m) in raw_matchers.iter().enumerate() {
        let name = m
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("matchers[{i}]: name required"))?;
        let value = m.get("value").and_then(|v| v.as_str()).unwrap_or("");
        let is_regex = m.get("is_regex").and_then(|v| v.as_bool()).unwrap_or(false);
        let is_equal = m.get("is_equal").and_then(|v| v.as_bool()).unwrap_or(true);
        matchers.push(json!({
            "name": name,
            "value": value,
            "isRegex": is_regex,
            "isEqual": is_equal
        }));
    }

    let body = json!({
        "matchers":  matchers,
        "startsAt":  starts_at,
        "endsAt":    ends_at,
        "createdBy": created_by,
        "comment":   comment
    });

    let resp = am_post(host, "/api/v2/silences", &body)?;
    let silence_id = resp
        .get("silenceID")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(json!({
        "url": "alertmanager.endpoint",
        "silence_id": silence_id,
        "ends_at": ends_at,
        "created": !silence_id.is_empty()
    }))
}

fn silence_delete(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = input
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("`id` required")?;
    am_delete(host, &format!("/api/v2/silence/{}", percent_encode(id)))?;
    Ok(json!({
        "url": "alertmanager.endpoint",
        "id": id,
        "deleted": true
    }))
}

// ---------------------------------------------------------------------------
// Tiny helpers (no extra deps).
// ---------------------------------------------------------------------------

/// Percent-encode a string for URL path/query use (RFC 3986 unreserved chars pass through).
fn percent_encode(s: &str) -> String {
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

/// Format a Unix timestamp as a minimal RFC3339 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
fn unix_secs_to_rfc3339(secs: u64) -> String {
    // Hand-roll a minimal formatter to avoid pulling in chrono/time.
    // Leap seconds ignored; good enough for silence windows.
    let s = secs;
    let sec = s % 60;
    let min = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days = s / 86400;
    // Day → calendar date (proleptic Gregorian).
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(mut d: u64) -> (u64, u64, u64) {
    // 400-year cycle = 146097 days.
    let c400 = d / 146097;
    d %= 146097;
    let c100 = (d / 36524).min(3);
    d -= c100 * 36524;
    let c4 = d / 1461;
    d %= 1461;
    let c1 = (d / 365).min(3);
    d -= c1 * 365;
    let year = c400 * 400 + c100 * 100 + c4 * 4 + c1 + 1970;
    // d is now the 0-based day within the year.
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for md in &month_days {
        if d < *md {
            break;
        }
        d -= md;
        month += 1;
    }
    (year, month, d + 1)
}

/// Heuristic RFC3339 check: `YYYY-MM-DDTHH:MM:SS` prefix must be present.
fn looks_like_rfc3339(s: &str) -> bool {
    s.len() >= 19
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.as_bytes()[10] == b'T'
        && s.as_bytes()[13] == b':'
        && s.as_bytes()[16] == b':'
}

/// Normalise a silence wire object to the canonical output shape.
fn normalise_silence(s: &Value) -> Value {
    let matchers: Vec<Value> = s
        .get("matchers")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            json!({
                "name":     m.get("name"),
                "value":    m.get("value"),
                "is_regex": m.get("isRegex").unwrap_or(&json!(false)),
                "is_equal": m.get("isEqual").unwrap_or(&json!(true))
            })
        })
        .collect();
    json!({
        "id":         s.get("id"),
        "matchers":   matchers,
        "starts_at":  s.get("startsAt"),
        "ends_at":    s.get("endsAt"),
        "created_by": s.get("createdBy"),
        "comment":    s.get("comment"),
        "state":      s.get("status").and_then(|st| st.get("state"))
    })
}

fn main() {
    manifest_builder().serve();
}

// ---------------------------------------------------------------------------
// Tests — one MockHost test per op.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> Plugin {
        manifest_builder().build()
    }

    fn base_host() -> MockHost {
        MockHost::default().with_endpoint_ref("alertmanager.endpoint", "http://am.test:9093")
    }

    // -- alertmanager.test ---------------------------------------------------

    #[test]
    fn test_op_returns_version_and_cluster() {
        let mut host = base_host().with_http(
            "/api/v2/status",
            json!({
                "versionInfo": {"version": "0.26.0"},
                "cluster": {"status": "ready", "peers": [{"name": "peer1"}]}
            }),
        );
        let out = plugin()
            .call("alertmanager.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["ready"], true);
        assert_eq!(out["version"], "0.26.0");
        assert_eq!(out["cluster_status"], "ready");
        assert_eq!(out["cluster_peers"], 1);
    }

    #[test]
    fn test_op_reports_latency_ms() {
        let mut host = base_host().with_http(
            "/api/v2/status",
            json!({
                "versionInfo": {"version": "0.26.0"},
                "cluster": {"status": "ready", "peers": []}
            }),
        );
        let out = plugin()
            .call("alertmanager.test", json!({}), &mut host)
            .unwrap();
        let latency = out["latency_ms"]
            .as_i64()
            .expect("latency_ms should be a number");
        assert!(latency >= 0, "latency_ms should be non-negative");
    }

    // -- alertmanager.alerts -------------------------------------------------

    #[test]
    fn alerts_op_contributes_records_and_normalises_shape() {
        let mut host = base_host().with_http(
            "/api/v2/alerts",
            json!([
                {
                    "fingerprint": "abc123",
                    "labels": {"alertname": "HighErrorRate", "severity": "critical"},
                    "annotations": {"summary": "Error rate above threshold"},
                    "startsAt": "2024-01-01T12:00:00Z",
                    "endsAt": "2024-01-01T12:30:00Z",
                    "generatorURL": "http://prom.test/graph",
                    "status": {
                        "state": "active",
                        "silencedBy": [],
                        "inhibitedBy": []
                    }
                }
            ]),
        );
        let out = plugin()
            .call("alertmanager.alerts", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 1);
        let alert = &out["alerts"][0];
        assert_eq!(alert["fingerprint"], "abc123");
        assert_eq!(alert["state"], "active");
        // contribution check
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 1);
        assert_eq!(contributed[0].id, "abc123");
        assert_eq!(contributed[0].entity, "alertmanager.alert");
    }

    // -- alertmanager.silence.list ------------------------------------------

    #[test]
    fn silence_list_op_filters_by_state() {
        let mut host = base_host().with_http(
            "/api/v2/silences",
            json!([
                {
                    "id": "s1",
                    "matchers": [{"name": "alertname", "value": "X", "isRegex": false, "isEqual": true}],
                    "startsAt": "2024-01-01T00:00:00Z",
                    "endsAt": "2024-01-02T00:00:00Z",
                    "createdBy": "admin",
                    "comment": "test silence",
                    "status": {"state": "active"}
                },
                {
                    "id": "s2",
                    "matchers": [],
                    "startsAt": "2023-12-01T00:00:00Z",
                    "endsAt": "2023-12-02T00:00:00Z",
                    "createdBy": "admin",
                    "comment": "old silence",
                    "status": {"state": "expired"}
                }
            ]),
        );
        let out = plugin()
            .call(
                "alertmanager.silence.list",
                json!({"state": "active"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["silences"][0]["id"], "s1");
        assert_eq!(out["silences"][0]["state"], "active");
    }

    // -- alertmanager.silence.create ----------------------------------------

    #[test]
    fn silence_create_op_posts_body_and_returns_id() {
        let mut host =
            base_host().with_http("/api/v2/silences", json!({"silenceID": "deadbeef-1234"}));
        let out = plugin()
            .call(
                "alertmanager.silence.create",
                json!({
                    "matchers": [{"name": "alertname", "value": "HighErrorRate"}],
                    "duration": "2h",
                    "comment": "silenced during incident triage",
                    "created_by": "flux-plugin"
                }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["silence_id"], "deadbeef-1234");
        assert_eq!(out["created"], true);
        // ends_at should be a parseable RFC3339-like string
        let ends = out["ends_at"].as_str().unwrap();
        assert!(ends.contains('T'), "ends_at should be RFC3339: {ends}");
    }

    // -- alertmanager.silence.delete ----------------------------------------

    #[test]
    fn silence_delete_op_issues_delete_and_returns_id() {
        let mut host = base_host().with_http(
            "/api/v2/silence/deadbeef-1234",
            json!({}), // Alertmanager returns 200 empty body
        );
        let out = plugin()
            .call(
                "alertmanager.silence.delete",
                json!({"id": "deadbeef-1234"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["id"], "deadbeef-1234");
        assert_eq!(out["deleted"], true);
    }
}

// ===========================================================================
// Schema contract regression guard
// ===========================================================================
// The `read_op_typed::<T>` / `write_op_typed::<T>` migration changes the
// schema authorship from hand-written `json!({...})` literals to schemars
// derivation. This module encodes the *legacy* contract (fields, types,
// required set) and asserts the derived schemas are equivalent. Any drift is
// a contract change that needs explicit review.
#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    /// Normalized kind of one top-level input property.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        Array,
        Object,
        Enum(Vec<String>),
    }

    #[derive(Clone)]
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

    /// The authoritative pre-migration contract for all 5 alertmanager ops.
    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            (
                "alertmanager.test",
                OpContract {
                    props: vec![],
                    required: vec![],
                },
            ),
            (
                "alertmanager.alerts",
                OpContract {
                    props: vec![
                        p("filter", Kind::Array),
                        p("active", Kind::Bool),
                        p("silenced", Kind::Bool),
                        p("inhibited", Kind::Bool),
                        p("limit", Kind::Int),
                    ],
                    required: vec![],
                },
            ),
            (
                "alertmanager.silence.list",
                OpContract {
                    props: vec![p(
                        "state",
                        Kind::Enum(vec!["active".into(), "pending".into(), "expired".into()]),
                    )],
                    required: vec![],
                },
            ),
            (
                "alertmanager.silence.create",
                OpContract {
                    props: vec![
                        p("matchers", Kind::Array),
                        p("duration", Kind::Str),
                        p("ends_at", Kind::Str),
                        p("comment", Kind::Str),
                        p("created_by", Kind::Str),
                    ],
                    required: vec!["matchers", "comment"],
                },
            ),
            (
                "alertmanager.silence.delete",
                OpContract {
                    props: vec![p("id", Kind::Str)],
                    required: vec!["id"],
                },
            ),
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
                    return Kind::Enum(vals);
                }
                Kind::Str
            }
            "array" => Kind::Array,
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
                let resolved = resolve(v, &defs);
                got.insert(k.as_str(), kind_of(resolved));
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

        let req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut req_set: Vec<&str> = req.clone();
        req_set.sort();
        let mut want_req: Vec<&str> = contract.required.clone();
        want_req.sort();
        assert_eq!(req_set, want_req, "{op_name}: required set");
    }

    #[test]
    fn derived_schemas_match_legacy_contract() {
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

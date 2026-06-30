//! `opsgenie` — a flux integration plugin for the Opsgenie REST API v2: alert management, on-call
//! visibility, and schedule listing. Authenticates with a GenieKey header (not a Bearer token, so
//! auth is handled manually: the key is fetched via `host.secret("api_key")` and injected as
//! `Authorization: GenieKey <key>`). The base URL is `opsgenie.endpoint` (defaults to
//! `https://api.eu.opsgenie.com`). Alert list ops contribute datasource records (`opsgenie.alert`)
//! so the agent can search them.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("opsgenie", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec!["OPSGENIE_API_KEY".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "api_key".into(),
            env: vec!["OPSGENIE_API_KEY".into()],
            description: "Opsgenie API key (GenieKey). Create in Settings → API key management."
                .into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "opsgenie.endpoint".into(),
            env: vec!["OPSGENIE_API_URL".into()],
            description:
                "Opsgenie API base URL (default https://api.eu.opsgenie.com for the EU region)."
                    .into(),
        })
        .datasource(ds(
            "opsgenie.alerts",
            "opsgenie.alert",
            "Opsgenie alerts.",
        ))
        // ---- auth test ----
        .operation(
            read_op(
                "opsgenie.test",
                "Validate the Opsgenie API key and report account name, user count, and plan.",
                so(json!({}), json!([])),
            ),
            op_test,
        )
        // ---- alert reads ----
        .operation(
            read_op(
                "opsgenie.alert.list",
                "List Opsgenie alerts (newest first) using the Opsgenie query language. Contributes records.",
                so(
                    json!({
                        "query": {"type": "string", "description": "Opsgenie query language, e.g. 'status: open AND priority: P1'"},
                        "limit": {"type": "integer", "description": "Max alerts to return (1-100, default 20)"}
                    }),
                    json!([]),
                ),
            ),
            op_alert_list,
        )
        .operation(
            read_op(
                "opsgenie.alert.get",
                "Show one Opsgenie alert by id, alias, or tiny id — full details, status, owner, acknowledgement state.",
                so(
                    json!({
                        "id": {"type": "string", "description": "Alert id, alias, or tiny id (see identifier_type)"},
                        "identifier_type": {"type": "string", "description": "How to interpret id: id (default), alias, or tiny"}
                    }),
                    json!(["id"]),
                ),
            ),
            op_alert_get,
        )
        // ---- alert writes ----
        .operation(
            write_op(
                "opsgenie.alert.ack",
                "Acknowledge an Opsgenie alert (stops escalation). The API is async — returns Accepted + RequestId.",
                so(
                    json!({
                        "id": {"type": "string", "description": "Alert id, alias, or tiny id (see identifier_type)"},
                        "identifier_type": {"type": "string", "description": "How to interpret id: id (default), alias, or tiny"},
                        "note": {"type": "string", "description": "Optional note attached to the acknowledgement"},
                        "user": {"type": "string", "description": "Display name of the actor"}
                    }),
                    json!(["id"]),
                ),
            ),
            op_alert_ack,
        )
        .operation(
            write_op(
                "opsgenie.alert.close",
                "Close an Opsgenie alert, optionally with a note. The API is async — returns Accepted + RequestId.",
                so(
                    json!({
                        "id": {"type": "string", "description": "Alert id, alias, or tiny id (see identifier_type)"},
                        "identifier_type": {"type": "string", "description": "How to interpret id: id (default), alias, or tiny"},
                        "note": {"type": "string", "description": "Optional note attached to the close"},
                        "user": {"type": "string", "description": "Display name of the actor"}
                    }),
                    json!(["id"]),
                ),
            ),
            op_alert_close,
        )
        .operation(
            write_op(
                "opsgenie.alert.note",
                "Add a note to an Opsgenie alert. The API is async — returns Accepted + RequestId.",
                so(
                    json!({
                        "id": {"type": "string", "description": "Alert id, alias, or tiny id (see identifier_type)"},
                        "identifier_type": {"type": "string", "description": "How to interpret id: id (default), alias, or tiny"},
                        "note": {"type": "string", "description": "The note text (required)"},
                        "user": {"type": "string", "description": "Display name of the actor"}
                    }),
                    json!(["id", "note"]),
                ),
            ),
            op_alert_note,
        )
        // ---- schedules / on-call ----
        .operation(
            read_op(
                "opsgenie.oncall",
                "Who is on call right now: every enabled schedule with its current on-call participants. Optionally filter by schedule name.",
                so(
                    json!({
                        "schedule": {"type": "string", "description": "Case-insensitive substring filter on schedule name"}
                    }),
                    json!([]),
                ),
            ),
            op_oncall,
        )
        .operation(
            read_op(
                "opsgenie.schedule.list",
                "List Opsgenie schedules with id, name, timezone, and enabled state.",
                so(json!({}), json!([])),
            ),
            op_schedule_list,
        )
}

// ---------------------------------------------------------------------------
// Helper: datasource declaration.
// ---------------------------------------------------------------------------

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}

/// `{ "type": "object", "properties": <props>, "required": <required> }`.
fn so(props: Value, required: Value) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

// ---------------------------------------------------------------------------
// HTTP plumbing — Opsgenie uses `Authorization: GenieKey <key>` (prefixed, not
// bare Bearer), so we fetch the key ourselves and set the header manually.
// ---------------------------------------------------------------------------

/// Resolve the (base_url, api_key) pair for every request.
fn og_creds(host: &mut Host) -> Result<(String, String), String> {
    let base = host
        .endpoint("opsgenie.endpoint")
        .unwrap_or_else(|_| "https://api.eu.opsgenie.com".into());
    let key = host.secret("api_key")?;
    Ok((base.trim_end_matches('/').to_string(), key))
}

/// GET `{base}{path}` with the GenieKey header; parse JSON; error on non-2xx.
fn og_get(host: &mut Host, path: &str) -> Result<Value, String> {
    let (base, key) = og_creds(host)?;
    let url = format!("{base}{path}");
    let auth = format!("GenieKey {key}");
    let resp = host.http("GET", &url, None, &[("authorization", auth.as_str())], None)?;
    if !resp.is_success() {
        return Err(format!(
            "opsgenie GET {path} → {} {}",
            resp.status, resp.body
        ));
    }
    resp.json()
}

/// POST a JSON body to `{base}{path}` with the GenieKey header; parse JSON; error on non-2xx.
/// Opsgenie async write endpoints return 202 Accepted, which is still success.
fn og_post(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    let (base, key) = og_creds(host)?;
    let url = format!("{base}{path}");
    let auth = format!("GenieKey {key}");
    let body_str = serde_json::to_string(body).map_err(|e| e.to_string())?;
    let resp = host.http(
        "POST",
        &url,
        None,
        &[
            ("authorization", auth.as_str()),
            ("content-type", "application/json"),
        ],
        Some(body_str.as_str()),
    )?;
    if !resp.is_success() {
        return Err(format!(
            "opsgenie POST {path} → {} {}",
            resp.status, resp.body
        ));
    }
    resp.json()
}

// ---------------------------------------------------------------------------
// Input helpers.
// ---------------------------------------------------------------------------

/// Trimmed string for `key`; `None` when absent/null/empty.
fn flex_str(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// Clamp a limit to [1, max], falling back to `default` when unset/zero.
fn clamp_limit(v: i64, default: i64, max: i64) -> i64 {
    if v <= 0 {
        default
    } else if v > max {
        max
    } else {
        v
    }
}

/// Build `?k=v&...` for non-empty values; returns "" if nothing.
fn qs(pairs: &[(&str, &str)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// Percent-encode a query value (spaces → %20, etc.).
fn enc(s: &str) -> String {
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

/// Map `identifier_type` parameter to the Opsgenie `identifierType` query value.
fn identifier_type_param(input: &Value) -> &'static str {
    match flex_str(input, "identifier_type")
        .as_deref()
        .unwrap_or("id")
    {
        "alias" => "alias",
        "tiny" => "tiny",
        _ => "id",
    }
}

/// Build the standard write-action body (user + optional note + source).
fn action_body(input: &Value) -> Value {
    let user = flex_str(input, "user").unwrap_or_else(|| "flux-plugin".into());
    let mut body = json!({ "user": user, "source": "flux-plugin" });
    if let Some(note) = flex_str(input, "note") {
        body["note"] = json!(note);
    }
    body
}

// ---------------------------------------------------------------------------
// Op: opsgenie.test
// ---------------------------------------------------------------------------

fn op_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let v = og_get(host, "/v2/account")?;
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let name = data
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let user_count = data.get("userCount").and_then(|x| x.as_u64()).unwrap_or(0);
    let plan = data
        .get("plan")
        .and_then(|p| p.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok(json!({
        "ok": true,
        "account_name": name,
        "user_count": user_count,
        "plan": plan,
    }))
}

// ---------------------------------------------------------------------------
// Op: opsgenie.alert.list
// ---------------------------------------------------------------------------

fn op_alert_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let limit = clamp_limit(
        input.get("limit").and_then(|v| v.as_i64()).unwrap_or(0),
        20,
        100,
    );
    let query = flex_str(&input, "query").unwrap_or_default();
    let limit_s = limit.to_string();
    let path = format!(
        "/v2/alerts{}",
        qs(&[
            ("limit", limit_s.as_str()),
            ("sort", "createdAt"),
            ("order", "desc"),
            ("query", query.as_str()),
        ])
    );
    let v = og_get(host, &path)?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();

    // Normalise camelCase API fields to snake_case.
    let alerts: Vec<Value> = data.iter().map(alert_from_api).collect();

    // Contribute records.
    let records: Vec<Record> = alerts.iter().filter_map(alert_record).collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }

    Ok(json!({ "alerts": alerts, "count": alerts.len() }))
}

/// Convert one alert API object (camelCase) to our snake_case shape.
fn alert_from_api(w: &Value) -> Value {
    json!({
        "id":           w.get("id").cloned().unwrap_or(Value::Null),
        "tiny_id":      w.get("tinyId").cloned().unwrap_or(Value::Null),
        "alias":        w.get("alias").cloned().unwrap_or(Value::Null),
        "message":      w.get("message").cloned().unwrap_or(Value::Null),
        "status":       w.get("status").cloned().unwrap_or(Value::Null),
        "acknowledged": w.get("acknowledged").cloned().unwrap_or(json!(false)),
        "priority":     w.get("priority").cloned().unwrap_or(Value::Null),
        "owner":        w.get("owner").cloned().unwrap_or(Value::Null),
        "tags":         w.get("tags").cloned().unwrap_or(json!([])),
        "source":       w.get("source").cloned().unwrap_or(Value::Null),
        "count":        w.get("count").cloned().unwrap_or(json!(0)),
        "created_at":   w.get("createdAt").cloned().unwrap_or(Value::Null),
        "updated_at":   w.get("updatedAt").cloned().unwrap_or(Value::Null),
    })
}

/// Build a datasource Record for an alert (for contribute).
fn alert_record(alert: &Value) -> Option<Record> {
    let id = alert.get("id")?.as_str()?.to_string();
    let message = alert
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let status = alert
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(Record::new(
        Source::new("opsgenie"),
        "opsgenie.alert",
        id,
        message,
        format!("status={status}"),
    ))
}

// ---------------------------------------------------------------------------
// Op: opsgenie.alert.get
// ---------------------------------------------------------------------------

fn op_alert_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = flex_str(&input, "id").ok_or("`id` (string) required")?;
    let id_type = identifier_type_param(&input);
    let path = format!(
        "/v2/alerts/{}{}",
        enc(&id),
        if id_type == "id" {
            String::new()
        } else {
            format!("?identifierType={id_type}")
        }
    );
    let v = og_get(host, &path)?;
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let alert = alert_from_api(&data);
    let description = data
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let details = data.get("details").cloned().unwrap_or(json!({}));
    Ok(json!({ "alert": alert, "description": description, "details": details }))
}

// ---------------------------------------------------------------------------
// Op: opsgenie.alert.ack
// ---------------------------------------------------------------------------

fn op_alert_ack(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = flex_str(&input, "id").ok_or("`id` (string) required")?;
    let id_type = identifier_type_param(&input);
    let path = format!(
        "/v2/alerts/{}/acknowledge{}",
        enc(&id),
        if id_type == "id" {
            String::new()
        } else {
            format!("?identifierType={id_type}")
        }
    );
    og_alert_action(host, &path, &action_body(&input))
}

// ---------------------------------------------------------------------------
// Op: opsgenie.alert.close
// ---------------------------------------------------------------------------

fn op_alert_close(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = flex_str(&input, "id").ok_or("`id` (string) required")?;
    let id_type = identifier_type_param(&input);
    let path = format!(
        "/v2/alerts/{}/close{}",
        enc(&id),
        if id_type == "id" {
            String::new()
        } else {
            format!("?identifierType={id_type}")
        }
    );
    og_alert_action(host, &path, &action_body(&input))
}

// ---------------------------------------------------------------------------
// Op: opsgenie.alert.note
// ---------------------------------------------------------------------------

fn op_alert_note(input: Value, host: &mut Host) -> Result<Value, String> {
    let id = flex_str(&input, "id").ok_or("`id` (string) required")?;
    if flex_str(&input, "note").is_none() {
        return Err("`note` (string) required".into());
    }
    let id_type = identifier_type_param(&input);
    let path = format!(
        "/v2/alerts/{}/notes{}",
        enc(&id),
        if id_type == "id" {
            String::new()
        } else {
            format!("?identifierType={id_type}")
        }
    );
    og_alert_action(host, &path, &action_body(&input))
}

/// Shared POST for alert action endpoints — returns accepted + requestId.
fn og_alert_action(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    let v = og_post(host, path, body)?;
    let request_id = v
        .get("requestId")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let result = v
        .get("result")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok(json!({ "accepted": true, "request_id": request_id, "result": result }))
}

// ---------------------------------------------------------------------------
// Op: opsgenie.schedule.list  (also used internally by oncall)
// ---------------------------------------------------------------------------

fn op_schedule_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let schedules = fetch_schedules(host)?;
    let count = schedules.len();
    Ok(json!({ "schedules": schedules, "count": count }))
}

/// Shared schedule-fetch helper used by both schedule.list and oncall.
fn fetch_schedules(host: &mut Host) -> Result<Vec<Value>, String> {
    let v = og_get(host, "/v2/schedules")?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let schedules = data
        .iter()
        .map(|item| {
            let team = item
                .get("ownerTeam")
                .and_then(|t| t.get("name"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            json!({
                "id":       item.get("id").cloned().unwrap_or(Value::Null),
                "name":     item.get("name").cloned().unwrap_or(Value::Null),
                "timezone": item.get("timezone").cloned().unwrap_or(Value::Null),
                "enabled":  item.get("enabled").cloned().unwrap_or(json!(false)),
                "team":     team,
            })
        })
        .collect();
    Ok(schedules)
}

// ---------------------------------------------------------------------------
// Op: opsgenie.oncall
// ---------------------------------------------------------------------------

fn op_oncall(input: Value, host: &mut Host) -> Result<Value, String> {
    let schedules = fetch_schedules(host)?;
    let filter = flex_str(&input, "schedule")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    let mut entries: Vec<Value> = Vec::new();
    for sched in &schedules {
        let enabled = sched
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let name = sched
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
            continue;
        }
        let id = sched
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let path = format!("/v2/schedules/{}/on-calls?flat=true", enc(&id));
        let v = og_get(host, &path)?;
        let recipients = v
            .get("data")
            .and_then(|d| d.get("onCallRecipients"))
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let on_call: Vec<String> = recipients
            .iter()
            .filter_map(|r| r.as_str().map(String::from))
            .collect();
        entries.push(json!({
            "schedule": name,
            "schedule_id": id,
            "on_call": on_call,
        }));
    }

    let count = entries.len();
    Ok(json!({ "entries": entries, "count": count }))
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    manifest_builder().serve();
}

// ---------------------------------------------------------------------------
// Tests — one MockHost test per op.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use host_kit::MockHost;

    fn plugin() -> Plugin {
        manifest_builder().build()
    }

    fn mock() -> MockHost {
        MockHost::default().with_secret("api_key", "test-genie-key")
    }

    // ---- opsgenie.test ----

    #[test]
    fn test_op_test() {
        let mut host = mock().with_http(
            "/v2/account",
            json!({
                "data": {
                    "name": "Acme Corp",
                    "userCount": 42,
                    "plan": { "name": "Enterprise" }
                }
            }),
        );
        let result = plugin()
            .call("opsgenie.test", json!({}), &mut host)
            .unwrap();
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["account_name"], json!("Acme Corp"));
        assert_eq!(result["user_count"], json!(42));
        assert_eq!(result["plan"], json!("Enterprise"));
    }

    // ---- opsgenie.alert.list ----

    #[test]
    fn test_op_alert_list() {
        let mut host = mock().with_http(
            "/v2/alerts",
            json!({
                "data": [
                    {
                        "id": "abc-123",
                        "tinyId": "3",
                        "alias": "deploy-fail",
                        "message": "Deployment failed",
                        "status": "open",
                        "acknowledged": false,
                        "priority": "P1",
                        "owner": "bob",
                        "tags": ["prod"],
                        "source": "grafana",
                        "count": 1,
                        "createdAt": "2026-06-29T12:00:00Z",
                        "updatedAt": "2026-06-29T12:01:00Z"
                    }
                ]
            }),
        );
        let result = plugin()
            .call(
                "opsgenie.alert.list",
                json!({ "query": "status: open", "limit": 5 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(result["count"], json!(1));
        let alerts = result["alerts"].as_array().unwrap();
        assert_eq!(alerts[0]["id"], json!("abc-123"));
        assert_eq!(alerts[0]["message"], json!("Deployment failed"));
        assert_eq!(alerts[0]["status"], json!("open"));
        assert_eq!(alerts[0]["tiny_id"], json!("3"));
        // Contribution check.
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 1);
        assert_eq!(contributed[0].id, "abc-123");
        assert_eq!(contributed[0].entity, "opsgenie.alert");
    }

    // ---- opsgenie.alert.get ----

    #[test]
    fn test_op_alert_get() {
        let mut host = mock().with_http(
            "/v2/alerts/abc-123",
            json!({
                "data": {
                    "id": "abc-123",
                    "tinyId": "3",
                    "alias": "deploy-fail",
                    "message": "Deployment failed",
                    "status": "open",
                    "acknowledged": false,
                    "priority": "P1",
                    "owner": "alice",
                    "tags": ["prod"],
                    "source": "grafana",
                    "count": 1,
                    "createdAt": "2026-06-29T12:00:00Z",
                    "updatedAt": "2026-06-29T12:01:00Z",
                    "description": "Deploy to prod failed at 12:00",
                    "details": { "service": "api" }
                }
            }),
        );
        let result = plugin()
            .call("opsgenie.alert.get", json!({ "id": "abc-123" }), &mut host)
            .unwrap();
        assert_eq!(result["alert"]["id"], json!("abc-123"));
        assert_eq!(
            result["description"],
            json!("Deploy to prod failed at 12:00")
        );
        assert_eq!(result["details"]["service"], json!("api"));
    }

    // ---- opsgenie.alert.ack ----

    #[test]
    fn test_op_alert_ack() {
        let mut host = mock().with_http(
            "/acknowledge",
            json!({ "requestId": "req-001", "result": "Request will be processed" }),
        );
        let result = plugin()
            .call(
                "opsgenie.alert.ack",
                json!({ "id": "abc-123", "note": "looking into it" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(result["accepted"], json!(true));
        assert_eq!(result["request_id"], json!("req-001"));
    }

    // ---- opsgenie.alert.close ----

    #[test]
    fn test_op_alert_close() {
        let mut host = mock().with_http(
            "/close",
            json!({ "requestId": "req-002", "result": "Request will be processed" }),
        );
        let result = plugin()
            .call(
                "opsgenie.alert.close",
                json!({ "id": "abc-123", "note": "resolved" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(result["accepted"], json!(true));
        assert_eq!(result["request_id"], json!("req-002"));
    }

    // ---- opsgenie.alert.note ----

    #[test]
    fn test_op_alert_note() {
        let mut host = mock().with_http(
            "/notes",
            json!({ "requestId": "req-003", "result": "Request will be processed" }),
        );
        let result = plugin()
            .call(
                "opsgenie.alert.note",
                json!({ "id": "abc-123", "note": "root cause: OOM" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(result["accepted"], json!(true));
        assert_eq!(result["request_id"], json!("req-003"));
    }

    // ---- opsgenie.alert.note — missing note validation ----

    #[test]
    fn test_op_alert_note_missing_note() {
        let mut host = mock();
        let result = plugin().call("opsgenie.alert.note", json!({ "id": "abc-123" }), &mut host);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("note"));
    }

    // ---- opsgenie.schedule.list ----

    #[test]
    fn test_op_schedule_list() {
        let mut host = mock().with_http(
            "/v2/schedules",
            json!({
                "data": [
                    {
                        "id": "sched-1",
                        "name": "Primary On-Call",
                        "timezone": "Europe/Berlin",
                        "enabled": true,
                        "ownerTeam": { "name": "Backend" }
                    }
                ]
            }),
        );
        let result = plugin()
            .call("opsgenie.schedule.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(result["count"], json!(1));
        let schedules = result["schedules"].as_array().unwrap();
        assert_eq!(schedules[0]["id"], json!("sched-1"));
        assert_eq!(schedules[0]["name"], json!("Primary On-Call"));
        assert_eq!(schedules[0]["timezone"], json!("Europe/Berlin"));
        assert_eq!(schedules[0]["enabled"], json!(true));
        assert_eq!(schedules[0]["team"], json!("Backend"));
    }

    // ---- opsgenie.oncall ----

    #[test]
    fn test_op_oncall() {
        let mut host = mock()
            // on-calls must be matched first (longer/more specific) before the schedules list match
            .with_http(
                "/on-calls",
                json!({
                    "data": {
                        "onCallRecipients": ["alice@example.com", "bob@example.com"]
                    }
                }),
            )
            .with_http(
                "/v2/schedules",
                json!({
                    "data": [
                        {
                            "id": "sched-1",
                            "name": "Primary On-Call",
                            "timezone": "UTC",
                            "enabled": true,
                            "ownerTeam": { "name": "Backend" }
                        },
                        {
                            "id": "sched-2",
                            "name": "Secondary",
                            "timezone": "UTC",
                            "enabled": false,
                            "ownerTeam": { "name": "Frontend" }
                        }
                    ]
                }),
            );
        let result = plugin()
            .call("opsgenie.oncall", json!({}), &mut host)
            .unwrap();
        // Only the enabled schedule should appear.
        assert_eq!(result["count"], json!(1));
        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries[0]["schedule"], json!("Primary On-Call"));
        let on_call = entries[0]["on_call"].as_array().unwrap();
        assert_eq!(on_call.len(), 2);
        assert!(on_call.iter().any(|v| v == "alice@example.com"));
    }
}

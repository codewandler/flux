//! Slack Web API transport through host-owned endpoint and auth capabilities.

use super::*;

/// GET a Slack API `path` (joined onto the host-resolved `slack.endpoint` base) and parse the JSON.
/// The host holds the URL; the plugin only ever names the endpoint ref and the method path.
pub(super) fn sl_get(host: &mut Host, path: &str, auth: Option<&str>) -> Result<Value, String> {
    host.get_json_ref("slack.endpoint", path, auth)
}

/// Send a JSON body to a Slack API `path` (joined onto the host-resolved `slack.endpoint` base) and
/// parse the response. The ref-based mirror of `host.send_json` — the URL stays host-side.
pub(super) fn sl_send(
    host: &mut Host,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: &Value,
) -> Result<Value, String> {
    host.send_json_ref("slack.endpoint", method, path, auth, body)
}

/// Slack returns `{"ok": bool, …}`; treat a falsey `ok` as an error built from the `"error"` field.
pub(super) fn check_ok(v: Value) -> Result<Value, String> {
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        Ok(v)
    } else {
        let err = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error");
        Err(format!("slack error: {err}"))
    }
}

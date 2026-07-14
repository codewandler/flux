//! Jira endpoint selection, auth-mode resolution, and host-owned HTTP transport.

use super::*;

// ---------------------------------------------------------------------------------------------------
// Auth-mode + base-URL selection (ported from fluxplane `NewLiveClient` / `liveClient.do`).
// ---------------------------------------------------------------------------------------------------

/// The auth purpose + base for the current request, decided from gated config reads:
/// - cloud_id present → Bearer (`api_token`) against the host-composed gateway via the
///   `"jira.gateway"` ref (template `https://api.atlassian.com/ex/jira/{cloud_id}`);
/// - else email present → Basic (`basic`) against the site URL via the `"jira.endpoint"` ref;
/// - else → Bearer (`api_token`) against the site URL via the `"jira.endpoint"` ref.
///
/// Every base is a **named endpoint reference** the host resolves — the plugin never holds a URL.
pub(super) struct AuthMode {
    /// The `auth_purpose` to pass to the host (`"api_token"` → Bearer, `"basic"` → Basic).
    pub(super) purpose: &'static str,
    /// The named manifest endpoint ref the request resolves against, host-side.
    pub(super) base: &'static str,
}

/// Resolve the request auth mode + base ref from the gated non-secret config values.
pub(super) fn auth_mode(host: &mut Host) -> Result<AuthMode, String> {
    // cloud_id (config value) → gateway mode: the host composes the gateway base from the
    // `jira.gateway` template; the plugin only ever addresses it by name.
    let cloud_id_set = host
        .config("cloud_id")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if cloud_id_set {
        return Ok(AuthMode {
            purpose: "api_token",
            base: "jira.gateway",
        });
    }
    // No cloud_id: the site URL is the base, resolved host-side from the named `"jira.endpoint"` ref.
    // email (config value, no cloud_id) → Basic fallback; else Bearer against the site URL.
    let email_set = host
        .config("email")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    Ok(AuthMode {
        purpose: if email_set { "basic" } else { "api_token" },
        base: "jira.endpoint",
    })
}

// ---------------------------------------------------------------------------------------------------
// HTTP helpers — every call routes through the host with the selected `auth_purpose`, so the host
// injects Bearer or Basic (the plugin never sees the token or builds the header).
// ---------------------------------------------------------------------------------------------------

/// Build the full API path `/rest/api/3{path}` (the `/rest/api/3` prefix the v3 API expects).
pub(super) fn api_path(path: &str) -> String {
    format!("/rest/api/3{path}")
}

/// GET `/rest/api/3{path}` (against the current base ref) and parse the JSON body.
pub(super) fn jget(host: &mut Host, path: &str) -> Result<Value, String> {
    let mode = auth_mode(host)?;
    host.get_json_ref(mode.base, &api_path(path), Some(mode.purpose))
}

/// Send a JSON body with `method` and parse the (non-empty) JSON response.
pub(super) fn jsend(
    host: &mut Host,
    method: &str,
    path: &str,
    body: &Value,
) -> Result<Value, String> {
    let mode = auth_mode(host)?;
    host.send_json_ref(mode.base, method, &api_path(path), Some(mode.purpose), body)
}

/// Send a request whose response body is ignored (PUT/DELETE/POST that return 204 No Content).
pub(super) fn jsend_noresp(
    host: &mut Host,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(), String> {
    let mode = auth_mode(host)?;
    let full = api_path(path);
    match body {
        // For a JSON body, route through `send_json_ref` (it sets content-type and parses a
        // response we ignore — as the pre-ref path did too).
        Some(b) => {
            host.send_json_ref(mode.base, method, &full, Some(mode.purpose), b)?;
        }
        // For no body, use `http_ref` directly and check the status.
        None => {
            let resp = host.http_ref(mode.base, method, &full, Some(mode.purpose), &[], None)?;
            if !resp.is_success() {
                return Err(format!(
                    "jira {method} {path} → {} {}",
                    resp.status, resp.body
                ));
            }
        }
    }
    Ok(())
}

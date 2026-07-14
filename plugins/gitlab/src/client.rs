//! GitLab REST transport. Every request stays behind the host capability boundary.

use super::*;

// ---------------------------------------------------------------------------
// HTTP plumbing — every REST verb funnels through `gl_request` (PRIVATE-TOKEN
// header, `gitlab.endpoint` ref + /api/v4 + path, is_success check) so
// auth/encoding stay DRY. The base URL is resolved host-side only (env or the
// manifest's gitlab.com default) — the plugin never holds it (D-32). The
// manifest's `personal_token` auth method is not Header-scheme, so the token
// is still fetched via `host.secret` and sent explicitly as `PRIVATE-TOKEN`.
// ---------------------------------------------------------------------------

pub(super) fn gl_request(
    host: &mut Host,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<HttpResponse, String> {
    let token = host.secret("personal_token")?;
    let mut headers: Vec<(&str, &str)> = vec![("PRIVATE-TOKEN", token.as_str())];
    let body_str;
    let body_ref = match body {
        Some(b) => {
            body_str = serde_json::to_string(b).map_err(|e| e.to_string())?;
            headers.push(("content-type", "application/json"));
            Some(body_str.as_bytes())
        }
        None => None,
    };
    let resp = host.http_ref(
        "gitlab.endpoint",
        method,
        &format!("/api/v4{path}"),
        None,
        &headers,
        body_ref,
    )?;
    if !resp.is_success() {
        return Err(format!(
            "gitlab {method} {path} → {} {}",
            resp.status, resp.body
        ));
    }
    Ok(resp)
}

/// GET `/api/v4{path}` on the `gitlab.endpoint` ref and return the parsed JSON.
pub(super) fn gl_get(host: &mut Host, path: &str) -> Result<Value, String> {
    gl_request(host, "GET", path, None)?.json()
}

/// POST a JSON body and return the parsed JSON response.
pub(super) fn gl_post(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    gl_request(host, "POST", path, Some(body))?.json()
}

/// PUT a JSON body and return the parsed JSON response.
pub(super) fn gl_put(host: &mut Host, path: &str, body: &Value) -> Result<Value, String> {
    gl_request(host, "PUT", path, Some(body))?.json()
}

/// DELETE a path; GitLab replies 204 (no body), so nothing is parsed.
pub(super) fn gl_delete(host: &mut Host, path: &str) -> Result<(), String> {
    gl_request(host, "DELETE", path, None)?;
    Ok(())
}

/// GET raw bytes (for binary downloads like the repository archive) — byte-exact via
/// `http_bytes_ref`, so an archive never round-trips through a UTF-8 string body.
pub(super) fn gl_get_bytes(host: &mut Host, path: &str) -> Result<Vec<u8>, String> {
    let token = host.secret("personal_token")?;
    let resp = host.http_bytes_ref(
        "gitlab.endpoint",
        "GET",
        &format!("/api/v4{path}"),
        None,
        &[("PRIVATE-TOKEN", token.as_str())],
        None,
        true,
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("gitlab GET {path} → {}", resp.status));
    }
    Ok(resp.bytes)
}

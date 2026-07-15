//! Tier 1 — `http.request`: raw HTTP protocol access (any method/headers/body).
//!
//! The model gets the status, response headers, and a byte-capped body. Non-2xx responses are a
//! *result*, not an op failure — a 404 comes back with its status, the op succeeds. This is the
//! "APIs → tier 1" surface: for reading a page *as a document* the model should reach for
//! `web.fetch` (tier 2) instead.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};
use flux_system::net::PrivateNetAllow;

use crate::{egress, WebOptions};

/// Cap on the response body handed to the model (bytes, cut on a char boundary). Mirrors the
/// `web.fetch` `MAX_BYTES` precedent.
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Cap on the rendered response-header block.
const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Default request timeout when the caller doesn't set one.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Ceiling on the caller-supplied timeout.
const MAX_TIMEOUT_SECS: u64 = 300;

/// The `http.request` tool. Holds its own `reqwest::Client`, the resolved `web`-scope egress
/// allow-list, and an optional audit sink — the `WebFetchTool` shape, extended with the audit hook
/// tier 1 needs.
pub struct HttpRequestTool {
    http: reqwest::Client,
    private_net: PrivateNetAllow,
    audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    grant_source: String,
    /// Env-var names this tool may resolve via `{"$secret": "NAME"}`. Fail-closed: a name not on
    /// this list is refused before its value is read (C-76). Resolved once at construction from
    /// `WebOptions.allowed_secrets`, else the `FLUX_WEB_SECRET_ALLOW` env var.
    allowed_secrets: Vec<String>,
}

impl HttpRequestTool {
    pub fn new(opts: &WebOptions) -> Self {
        Self {
            http: egress::redirect_disabled_client(),
            private_net: opts.private_net.clone(),
            audit: opts.audit.clone(),
            grant_source: opts
                .grant_source
                .clone()
                .unwrap_or_else(|| "config:web".to_string()),
            allowed_secrets: opts
                .allowed_secrets
                .clone()
                .unwrap_or_else(secret_allowlist_from_env),
        }
    }

    /// Emit the `PrivateNetAdmit` audit event when the guard just let a request through to a
    /// private/internal host (i.e. the `web` grant admitted what the bare SSRF guard would refuse).
    /// Mirrors the plugin host's `audit_admit`: gated on `host_resolves_private` so only genuine
    /// private admits are recorded.
    fn audit_admit(&self, host: &str) {
        if let Some(audit) = &self.audit {
            if flux_system::net::host_resolves_private(host) {
                audit.record_private_admit("web:http.request", host, &self.grant_source);
            }
        }
    }
}

#[async_trait]
impl Tool for HttpRequestTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "http.request".into(),
            description: "Make an arbitrary HTTP(S) request (any method, headers, and body) and \
                return the status, response headers, and body. Use this for APIs and raw protocol \
                access; to read a web page as a readable document prefer `web.fetch`. \
                Private/loopback addresses are blocked unless the `web` egress scope grants them. A \
                header value may be a secret reference `{\"$secret\": \"ENV_NAME\"}`, resolved from \
                the environment and never shown — but only for env-var names the operator has \
                allowlisted; any other name is refused."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The absolute http(s) URL to request."},
                    "method": {"type": "string", "description": "HTTP method (default GET)."},
                    "headers": {
                        "type": "object",
                        "description": "Request headers. A value may be a string or a secret reference {\"$secret\": \"ENV_NAME\"}.",
                        "additionalProperties": true
                    },
                    "body": {"type": "string", "description": "Request body, for methods that take one (POST/PUT/PATCH/…)."},
                    "timeout": {"type": "number", "description": "Timeout in seconds (default 30, max 300)."}
                },
                "required": ["url"]
            }),
            output_schema: None,
            // Arbitrary HTTP can mutate remote state (POST/PUT/DELETE), so this is not the read-only
            // shape `web.fetch` uses: honest Network effect, Medium risk, non-idempotent.
            effects: vec![Effect::Network],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Network],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("url")
            .and_then(Value::as_str)
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(url) = params.get("url").and_then(Value::as_str) {
            set.push(Intent {
                behavior: IntentBehavior::NetworkFetch,
                target: IntentTarget::Url {
                    url: url.to_string(),
                },
                role: IntentRole::ReadTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let raw = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("http.request: `url` required".into()))?;
        let method_str = params
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let method = reqwest::Method::from_bytes(method_str.as_bytes())
            .map_err(|_| Error::Other(format!("http.request: invalid method {method_str:?}")))?;

        // Guard egress (SSRF): resolve the host + block private ranges unless the `web` scope grants
        // them, and capture the vetted addresses so the connection is pinned to them (no rebinding).
        // Runs before any bytes leave the process.
        let (url, pinned) = flux_system::net::guard_url_scoped_pinned(raw, &self.private_net)?;

        let timeout = params
            .get("timeout")
            .and_then(Value::as_f64)
            .map(|s| s.max(0.0) as u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        // Headers — resolving `{"$secret": "ENV"}` markers to their env values and seeding the
        // redactor so a token in a header never surfaces readable in output or persisted events.
        let mut request_headers = HeaderMap::new();
        if let Some(headers) = params.get("headers").and_then(Value::as_object) {
            for (name, val) in headers {
                let resolved = resolve_header_value(val, ctx, &self.allowed_secrets)?;
                let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                    Error::Other(format!("http.request: invalid header name `{name}`: {e}"))
                })?;
                let value = HeaderValue::from_str(&resolved).map_err(|e| {
                    Error::Other(format!(
                        "http.request: invalid value for header `{name}`: {e}"
                    ))
                })?;
                request_headers.insert(name, value);
            }
        }
        let body = params
            .get("body")
            .and_then(Value::as_str)
            .map(|body| body.as_bytes().to_vec());

        let response = egress::send_guarded(
            &self.http,
            egress::GuardedRequest {
                url,
                pinned,
                method,
                headers: request_headers,
                body,
                timeout: Duration::from_secs(timeout),
            },
            "http.request",
            |raw| flux_system::net::guard_url_scoped_pinned(raw, &self.private_net),
            |url| {
                if let Some(host) = url.host_str() {
                    self.audit_admit(host);
                }
            },
        )
        .await?;

        let status = response.status();
        let headers_text = render_headers(response.headers());
        let capped = egress::read_body_capped(response, MAX_BODY_BYTES, "http.request").await?;
        let mut body = cap_str(
            String::from_utf8_lossy(&capped.bytes).into_owned(),
            MAX_BODY_BYTES,
        );
        if capped.truncated && !body.ends_with("…[truncated]") {
            body.push_str("\n…[truncated]");
        }

        // A completed request is a successful op: the HTTP status (incl. 4xx/5xx) is *data*, carried
        // in the first line — never a tool-level error.
        Ok(ToolResult {
            content: format!("HTTP {status}\n{headers_text}\n{body}"),
            view: None,
            is_error: false,
        })
    }
}

/// A header value: a plain string, or the secret marker `{"$secret": "ENV_NAME"}`. A secret
/// reference is resolved from the environment (and seeded into the redactor) **only if `NAME` is on
/// the caller-configured allowlist** — otherwise it is refused before the value is read, so a
/// prompt-injected model cannot name an arbitrary env var (`AWS_SECRET_ACCESS_KEY`, …) and exfiltrate
/// it to an attacker host in one call (C-76). Any other shape is a caller error.
fn resolve_header_value(val: &Value, ctx: &ToolContext, allowed: &[String]) -> Result<String> {
    if let Some(name) = as_secret_ref(val) {
        if !allowed.iter().any(|a| a == name) {
            return Err(Error::Other(format!(
                "http.request: secret env var `{name}` is not on the allowlist and will not be \
                 resolved. Add it to `[web] allowed_secrets` (or the FLUX_WEB_SECRET_ALLOW env var) \
                 to permit `{{\"$secret\": \"{name}\"}}`."
            )));
        }
        let resolved = std::env::var(name).map_err(|_| {
            Error::Other(format!(
                "http.request: secret env var `{name}` is not set (referenced via {{\"$secret\": \"{name}\"}})"
            ))
        })?;
        ctx.redactor.add_secret(resolved.clone());
        return Ok(resolved);
    }
    match val {
        Value::String(s) => Ok(s.clone()),
        _ => Err(Error::Other(
            "http.request: header values must be strings or a secret reference {\"$secret\": \"ENV\"}"
                .into(),
        )),
    }
}

/// Parse the `FLUX_WEB_SECRET_ALLOW` env var into a list of permitted secret env-var names
/// (comma- or whitespace-separated). Unset/empty ⇒ deny-all — the correct fail-closed default so a
/// `$secret` header reference is inert until an operator opts specific names in (C-76).
fn secret_allowlist_from_env() -> Vec<String> {
    std::env::var("FLUX_WEB_SECRET_ALLOW")
        .unwrap_or_default()
        .split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// If `v` is exactly the secret marker `{"$secret": "NAME"}`, return `NAME` (mirrors
/// `flux_lang::program::as_secret_ref`, inlined to avoid an L0 language dep for one predicate).
fn as_secret_ref(v: &Value) -> Option<&str> {
    let obj = v.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    obj.get("$secret")?.as_str()
}

/// Render the response headers as `Name: value` lines, capped so a pathological header set can't
/// blow the budget.
fn render_headers(headers: &reqwest::header::HeaderMap) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        let v = value.to_str().unwrap_or("<binary>");
        let line = format!("{name}: {v}\n");
        if out.len() + line.len() > MAX_HEADER_BYTES {
            out.push_str("…[headers truncated]\n");
            break;
        }
        out.push_str(&line);
    }
    out
}

/// Cap a string to `max` bytes, cut on a char boundary (an arbitrary response body is not
/// guaranteed to split cleanly — `String::truncate` panics off a boundary).
fn cap_str(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n…[truncated]");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_runtime::ToolContext;
    use flux_system::System;
    use flux_system::Workspace;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!(
            "flux-web-http-test-{}-{}",
            std::process::id(),
            // a per-ctx suffix so parallel tests don't share a workspace dir
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// A one-shot loopback HTTP server: accepts one connection, reads the request, writes a canned
    /// response. Returns its `http://127.0.0.1:<port>` base URL.
    async fn one_shot(status_line: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    /// Two one-shot servers: the first redirects to the second, which returns `final_body` and
    /// reports the raw request headers it received. `initial_host` controls the spelling used for
    /// the first URL so tests can grant `localhost` without also granting the `127.0.0.1` target.
    async fn redirect_to_loopback(
        initial_host: &str,
        final_body: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = target.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let _ = seen_tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{final_body}",
                    final_body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });

        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_port = source.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = source.accept().await {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nlocation: http://{target_addr}/final\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (
            format!("http://{initial_host}:{source_port}/start"),
            seen_rx,
        )
    }

    fn tool(private_net: PrivateNetAllow) -> HttpRequestTool {
        HttpRequestTool::new(&WebOptions {
            private_net,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn not_found_is_a_result_not_a_failure() {
        // Loopback is private, so grant the `web` scope for the test.
        let base = one_shot("404 Not Found", "nope").await;
        let t = tool(PrivateNetAllow::Any);
        let r = t
            .execute(&ctx(), json!({ "url": base }))
            .await
            .expect("a 404 must be a successful op, not an Err");
        assert!(!r.is_error, "a 404 is a result, not a tool error");
        assert!(r.content.contains("404"), "status surfaced: {}", r.content);
    }

    #[tokio::test]
    async fn private_target_refused_without_grant() {
        let base = one_shot("200 OK", "hi").await;
        let t = tool(PrivateNetAllow::None);
        let err = t.execute(&ctx(), json!({ "url": base })).await;
        assert!(
            err.is_err(),
            "a loopback target must be refused without a `web` grant"
        );
    }

    #[tokio::test]
    async fn redirect_target_is_guarded_before_the_second_request() {
        let (url, _seen) = redirect_to_loopback("localhost", "must not arrive").await;
        let t = tool(PrivateNetAllow::from_hosts(["localhost".to_string()]));
        let err = t
            .execute(&ctx(), json!({ "url": url }))
            .await
            .expect_err("the ungranted loopback redirect target must be refused");
        assert!(
            err.to_string().contains("private/loopback"),
            "the shared SSRF guard names the denial: {err}"
        );
    }

    #[tokio::test]
    async fn cross_origin_redirect_drops_all_caller_headers() {
        let (url, seen) = redirect_to_loopback("127.0.0.1", "ok").await;
        let t = tool(PrivateNetAllow::Any);
        let result = t
            .execute(
                &ctx(),
                json!({
                    "url": url,
                    "headers": {
                        "Authorization": "Bearer caller-secret",
                        "Cookie": "session=caller-secret",
                        "Proxy-Authorization": "Basic caller-secret",
                        "X-Api-Key": "caller-secret",
                        "X-Custom": "also-sensitive"
                    }
                }),
            )
            .await
            .unwrap();
        assert!(result.content.ends_with("ok"));
        let request = seen.await.expect("redirect destination received a request");
        let lower = request.to_ascii_lowercase();
        for name in [
            "authorization:",
            "cookie:",
            "proxy-authorization:",
            "x-api-key:",
            "x-custom:",
        ] {
            assert!(
                !lower.contains(name),
                "cross-origin redirect forwarded {name}: {request}"
            );
        }
    }

    /// Records `record_private_admit` calls so the audit path can be asserted without an event store.
    #[derive(Default)]
    struct RecordingAudit {
        calls: Mutex<Vec<(String, String, String)>>,
    }
    impl flux_plugin::EgressAudit for RecordingAudit {
        fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str) {
            self.calls.lock().unwrap().push((
                caller.to_string(),
                host.to_string(),
                grant_source.to_string(),
            ));
        }
    }

    #[tokio::test]
    async fn private_admit_emits_audit_event() {
        let base = one_shot("200 OK", "ok").await;
        let audit = Arc::new(RecordingAudit::default());
        let t = HttpRequestTool::new(&WebOptions {
            private_net: PrivateNetAllow::Any,
            audit: Some(audit.clone()),
            grant_source: Some("cli:--allow-private-net".into()),
            ..Default::default()
        });
        t.execute(&ctx(), json!({ "url": base })).await.unwrap();
        let calls = audit.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "one private admit audited");
        assert_eq!(calls[0].0, "web:http.request", "caller label");
        assert_eq!(calls[0].1, "127.0.0.1", "the private host reached");
        assert_eq!(calls[0].2, "cli:--allow-private-net", "grant source label");
    }

    /// Build a tool with an explicit secret allowlist (bypasses the `FLUX_WEB_SECRET_ALLOW` env
    /// fallback so these tests never race on a process-global var).
    fn tool_allowing(private_net: PrivateNetAllow, secrets: &[&str]) -> HttpRequestTool {
        HttpRequestTool::new(&WebOptions {
            private_net,
            allowed_secrets: Some(secrets.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn secret_header_is_resolved_and_seeded_into_the_redactor() {
        std::env::set_var("FLUX_WEB_TEST_TOKEN", "super-secret-42");
        let base = one_shot("200 OK", "ok").await;
        let t = tool_allowing(PrivateNetAllow::Any, &["FLUX_WEB_TEST_TOKEN"]);
        let c = ctx();
        t.execute(
            &c,
            json!({
                "url": base,
                "headers": { "authorization": { "$secret": "FLUX_WEB_TEST_TOKEN" } }
            }),
        )
        .await
        .unwrap();
        // The resolved token was registered with the redactor, so it is scrubbed everywhere output
        // or logs might carry it.
        let scrubbed = c.redactor.redact("leak: super-secret-42 end");
        assert!(
            !scrubbed.contains("super-secret-42"),
            "the resolved secret must be redacted: {scrubbed}"
        );
    }

    #[tokio::test]
    async fn missing_secret_header_env_is_a_clean_error() {
        let base = one_shot("200 OK", "ok").await;
        // Allowlisted but unset: the request passes the allowlist gate and then fails cleanly on
        // the missing value (not the allowlist refusal).
        let t = tool_allowing(PrivateNetAllow::Any, &["FLUX_WEB_DEFINITELY_UNSET"]);
        let err = t
            .execute(
                &ctx(),
                json!({
                    "url": base,
                    "headers": { "authorization": { "$secret": "FLUX_WEB_DEFINITELY_UNSET" } }
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("FLUX_WEB_DEFINITELY_UNSET") && err.contains("not set"),
            "names the missing var: {err}"
        );
    }

    /// C-76: a `$secret` naming an env var that is NOT on the allowlist must be refused — even when
    /// the var is set in the environment — and its value must never be read or sent. This is the
    /// single-call exfiltration primitive the story closes.
    #[tokio::test]
    async fn secret_ref_to_non_allowlisted_env_var_is_refused() {
        std::env::set_var("FLUX_WEB_STOLEN_TOKEN", "exfiltrate-me");
        let base = one_shot("200 OK", "ok").await;
        // Deny-all allowlist, though the classic exfil target is present in the environment.
        let t = tool_allowing(PrivateNetAllow::Any, &[]);
        let err = t
            .execute(
                &ctx(),
                json!({
                    "url": base,
                    "headers": { "x-api-key": { "$secret": "FLUX_WEB_STOLEN_TOKEN" } }
                }),
            )
            .await
            .expect_err("a non-allowlisted secret ref must be refused, not sent");
        let msg = err.to_string();
        assert!(
            msg.contains("allowlist") && !msg.contains("exfiltrate-me"),
            "refusal must name the allowlist and never leak the value: {msg}"
        );
    }

    #[test]
    fn guard_allows_public_refuses_private() {
        // The scope semantics the whole family shares: a public host passes even offline (the guard
        // only errors when a name *resolves* to a private range); a loopback literal is refused
        // without a grant and admitted with one.
        assert!(
            flux_system::net::guard_url_scoped("https://example.com/", &PrivateNetAllow::None)
                .is_ok()
        );
        assert!(
            flux_system::net::guard_url_scoped("http://127.0.0.1:9/", &PrivateNetAllow::None)
                .is_err()
        );
        assert!(
            flux_system::net::guard_url_scoped("http://127.0.0.1:9/", &PrivateNetAllow::Any)
                .is_ok()
        );
    }
}

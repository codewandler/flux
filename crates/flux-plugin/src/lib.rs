//! `flux-plugin` — the subprocess plugin protocol, host, and SDK.
//!
//! Plugins are native binaries in any language that speak a line-delimited JSON protocol over
//! stdio: the host writes one [`Frame`] (request) per line to the plugin's stdin and reads one
//! [`Frame`] (response) per line from its stdout. Plugins never do their own privileged IO — in
//! the full design every side effect is a host-capability call back over the same channel
//! (HTTP/process/blob/…); v1 implements `manifest` + `operation.call`. WASM can later be an
//! alternate transport behind the same protocol/manifest.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{Effect, Idempotency, Risk, ToolSpec};

/// JavaScript pre-tool hooks (QuickJS via `rquickjs`) — the other half of L4 extensibility, folded in
/// from the former `flux-hooks` crate. Re-exported at the crate root as [`JsHookEngine`].
pub mod hooks;
pub use hooks::JsHookEngine;

pub const PROTOCOL: &str = "flux.plugin.v1";

/// Whether a frame is a request (host→plugin) or a response (plugin→host).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    Request,
    Response,
}

/// One protocol message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub protocol: String,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FrameKind,
    pub command: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Frame {
    pub fn request(id: impl Into<String>, command: impl Into<String>, payload: Value) -> Self {
        Self {
            protocol: PROTOCOL.into(),
            id: id.into(),
            kind: FrameKind::Request,
            command: command.into(),
            payload,
            ok: false,
            result: Value::Null,
            error: None,
        }
    }

    pub fn ok_response(id: &str, result: Value) -> Self {
        Self {
            protocol: PROTOCOL.into(),
            id: id.into(),
            kind: FrameKind::Response,
            command: String::new(),
            payload: Value::Null,
            ok: true,
            result,
            error: None,
        }
    }

    pub fn err_response(id: &str, error: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL.into(),
            id: id.into(),
            kind: FrameKind::Response,
            command: String::new(),
            payload: Value::Null,
            ok: false,
            result: Value::Null,
            error: Some(error.into()),
        }
    }
}

/// A plugin-declared operation (becomes a tool projected to the agent, after the policy gate). The
/// `effects`/`risk`/`idempotency` an operation declares feed the authorization floor; when omitted, the
/// projection assumes a conservative default (see [`PluginTool::new`]) so an undeclared op can't slip the
/// gate. flux reuses its own [`Effect`]/[`Risk`]/[`Idempotency`] vocabulary — there is no separate
/// fluxplane-style "access" enum.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OperationSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
    /// IO effects this operation may produce (drives the policy floor + approval).
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Declared risk; `None` → `Risk::Medium`.
    #[serde(default)]
    pub risk: Option<Risk>,
    /// Declared idempotency; `None` → `Idempotency::NonIdempotent`.
    #[serde(default)]
    pub idempotency: Option<Idempotency>,
    /// Secret purposes (auth-method names) this op needs the host to resolve (e.g. `"api_token"`).
    #[serde(default)]
    pub secret_purposes: Vec<String>,
}

/// How the host injects a resolved secret into an `http.do` request for an auth method. Default
/// `Bearer`, so manifests written before this field — and the legacy `bearer_purpose` call path —
/// behave unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    /// `Authorization: Bearer <secret>`.
    #[default]
    Bearer,
    /// `Authorization: Basic base64(<user>:<secret>)` — `<user>` resolved from the method's `user_env`.
    Basic,
    /// A custom header `<name>: <secret>` (e.g. `PRIVATE-TOKEN`, `GenieKey`).
    Header { name: String },
    /// A query parameter `?<name>=<secret>`.
    Query { name: String },
}

/// An authentication method the plugin needs, resolved **by purpose**: the host maps `purpose` (e.g.
/// `"bot_token"`) to a secret value by trying `env` keys in order (each must also be a granted secret).
/// A plugin asks `secret { "purpose": "bot_token" }` or `http.do { "auth_purpose": "api_token" }`; the
/// host injects the resolved secret per the method's [`AuthScheme`] (the plugin never sees the token on
/// the injection path).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthMethod {
    /// The purpose name the plugin references (e.g. `"bot_token"`, `"api_token"`).
    pub purpose: String,
    /// Env-var keys to resolve the secret from, tried in order.
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// How the host injects the resolved secret into an HTTP request (default `Bearer`).
    #[serde(default)]
    pub scheme: AuthScheme,
    /// For `AuthScheme::Basic`: env-var keys holding the username/email half, tried in order. These are
    /// config (not a gated secret), so they resolve directly from declared env like an endpoint.
    #[serde(default)]
    pub user_env: Vec<String>,
}

impl AuthMethod {
    /// A Bearer-token method: `Authorization: Bearer <env>`.
    pub fn bearer(purpose: impl Into<String>, env: Vec<String>) -> Self {
        Self {
            purpose: purpose.into(),
            env,
            scheme: AuthScheme::Bearer,
            ..Self::default()
        }
    }

    /// A Basic-auth method: `Authorization: Basic base64(<user_env>:<env>)`.
    pub fn basic(purpose: impl Into<String>, user_env: Vec<String>, env: Vec<String>) -> Self {
        Self {
            purpose: purpose.into(),
            env,
            user_env,
            scheme: AuthScheme::Basic,
            ..Self::default()
        }
    }

    /// A custom-header method: `<header>: <env>`.
    pub fn header(purpose: impl Into<String>, header: impl Into<String>, env: Vec<String>) -> Self {
        Self {
            purpose: purpose.into(),
            env,
            scheme: AuthScheme::Header {
                name: header.into(),
            },
            ..Self::default()
        }
    }
}

/// A configurable API endpoint (base URL) the plugin resolves by name from env. A plugin asks
/// `endpoint { "name": "gitlab.endpoint" }` and the host returns `{ "url": … }`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointSpec {
    /// The endpoint name the plugin references.
    pub name: String,
    /// Env-var keys holding the base URL, tried in order.
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// The host capabilities a plugin requests. The host grants ONLY what is declared here and checks
/// each callback against it, so a plugin can never run an arbitrary binary, read an arbitrary env
/// var, or reach the network unless its manifest said so. Empty/false = that capability is denied.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// Allowed `argv[0]` programs for `process.run` (matched exactly; empty = `process.run` denied).
    #[serde(default)]
    pub process: Vec<String>,
    /// Allowed env-var keys for the `secret` capability (empty = `secret` denied).
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Whether `http.do` is permitted at all (host-side SSRF guard still applies).
    #[serde(default)]
    pub http: bool,
    /// Allowed `conn.dial` targets (`tcp:host:port` / `unix:/path`; a single `*` wildcards one
    /// segment, e.g. `tcp:*:5432`). Empty = the `conn.*` capability is denied.
    #[serde(default)]
    pub conn: Vec<String>,
    /// Whether the `blob.*` capability (content-addressed scratch store) is permitted.
    #[serde(default)]
    pub blob: bool,
}

/// What a plugin advertises about itself — the single source of truth the host introspects (ops,
/// auth methods, datasources, endpoints, and the capabilities it requests). No separate `*.list`
/// round-trips: the host reads it once.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub operations: Vec<OperationSpec>,
    /// Auth methods (by purpose) the host resolves to secrets for this plugin.
    #[serde(default)]
    pub auth: Vec<AuthMethod>,
    /// Datasources this plugin contributes/serves (records feed the D-07 knowledge index via the
    /// host's datasource capability). Uses the shared `flux-datasource` schema.
    #[serde(default)]
    pub datasources: Vec<flux_datasource::Declaration>,
    /// Configurable API endpoints (base URLs) the host resolves from env.
    #[serde(default)]
    pub endpoints: Vec<EndpointSpec>,
    /// Host capabilities the plugin requests (default: none — the plugin gets no privileged IO).
    #[serde(default)]
    pub capabilities: PluginCapabilities,
}

// ---------------------------------------------------------------------------
// Plugin SDK (guest side) — synchronous stdio loop
// ---------------------------------------------------------------------------

/// A handle a plugin uses to call back into the host (host capabilities) during an operation.
/// Each call writes a request frame to stdout and blocks for the host's response on stdin.
pub trait GuestHost {
    fn host_call(&mut self, command: &str, payload: Value) -> std::result::Result<Value, String>;
}

/// Implemented by a plugin: advertise a manifest, handle operation calls. The `host` handle lets
/// an operation call back into the host for privileged IO (HTTP/process/secret) — plugins do no
/// privileged IO of their own.
pub trait PluginHandler {
    fn manifest(&self) -> PluginManifest;
    fn call(
        &self,
        operation: &str,
        input: Value,
        host: &mut dyn GuestHost,
    ) -> std::result::Result<Value, String>;
}

/// The concrete [`GuestHost`] used by [`serve`]: writes plugin→host request frames and reads the
/// host's response, sharing the same stdio the serve loop uses (sequentially, never concurrently).
struct StdioGuestHost<'a, R: std::io::BufRead, W: std::io::Write> {
    reader: &'a mut R,
    writer: &'a mut W,
    next: u64,
}

impl<R: std::io::BufRead, W: std::io::Write> GuestHost for StdioGuestHost<'_, R, W> {
    fn host_call(&mut self, command: &str, payload: Value) -> std::result::Result<Value, String> {
        self.next += 1;
        let frame = Frame::request(format!("h{}", self.next), command, payload);
        let mut line = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;

        let mut resp = String::new();
        match self.reader.read_line(&mut resp) {
            Ok(0) => return Err("host closed the connection".into()),
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
        let frame: Frame = serde_json::from_str(resp.trim()).map_err(|e| e.to_string())?;
        if frame.ok {
            Ok(frame.result)
        } else {
            Err(frame.error.unwrap_or_default())
        }
    }
}

fn write_line<W: std::io::Write>(writer: &mut W, frame: &Frame) {
    if let Ok(mut out) = serde_json::to_string(frame) {
        out.push('\n');
        let _ = writer.write_all(out.as_bytes());
        let _ = writer.flush();
    }
}

/// Run the plugin: read request frames from stdin, dispatch, write response frames to stdout.
/// Operation calls may issue host-capability callbacks via the provided [`GuestHost`]. Blocks
/// until stdin closes. Call this from a plugin binary's `main`.
pub fn serve(handler: impl PluginHandler) {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or read error
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Frame>(line.trim()) else {
            continue;
        };
        let resp = match req.command.as_str() {
            "manifest" => match serde_json::to_value(handler.manifest()) {
                Ok(v) => Frame::ok_response(&req.id, v),
                Err(e) => Frame::err_response(&req.id, e.to_string()),
            },
            "operation.call" => {
                let op = req
                    .payload
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let input = req.payload.get("input").cloned().unwrap_or(Value::Null);
                let mut host = StdioGuestHost {
                    reader: &mut reader,
                    writer: &mut writer,
                    next: 0,
                };
                match handler.call(op, input, &mut host) {
                    Ok(v) => Frame::ok_response(&req.id, v),
                    Err(e) => Frame::err_response(&req.id, e),
                }
            }
            other => Frame::err_response(&req.id, format!("unknown command: {other}")),
        };
        write_line(&mut writer, &resp);
    }
}

// ---------------------------------------------------------------------------
// Host capabilities (the only IO a plugin gets, all routed through the guarded host)
// ---------------------------------------------------------------------------

/// The privileged operations a plugin may request of the host during an operation call. Every
/// capability is policy-relevant IO the plugin cannot do itself; the host services it through the
/// guarded [`System`](flux_system::System) and returns a result frame.
#[async_trait]
pub trait HostCapabilities: Send + Sync {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String>;
}

/// Denies every host-capability callback (the default for `call`). A plugin that needs callbacks
/// must be driven via [`PluginHost::call_with_host`] with a real [`HostCapabilities`].
pub struct DenyHostCaps;

#[async_trait]
impl HostCapabilities for DenyHostCaps {
    async fn handle(&self, command: &str, _p: &Value) -> std::result::Result<Value, String> {
        Err(format!("host capability `{command}` is not available"))
    }
}

/// Host capabilities backed by the guarded [`System`](flux_system::System): `process.run` (argv
/// only), `http.do` (GET, loopback/private blocked unless allowed), and `secret` (env refs). This
/// is the bridge that keeps plugin IO inside the same safety boundary as the agent's own tools.
///
/// Every callback is additionally gated by the per-plugin [`PluginCapabilities`] grants (built from
/// the plugin's manifest): `process.run` only for allow-listed programs, `secret` only for
/// allow-listed keys, `http.do` only if the plugin declared it. A fresh `SystemHostCaps` grants
/// nothing — call [`with_grants`](Self::with_grants).
pub struct SystemHostCaps {
    system: Arc<flux_system::System>,
    allow_private_net: bool,
    grants: PluginCapabilities,
    auth: Vec<AuthMethod>,
    endpoints: Vec<EndpointSpec>,
    /// Open `conn.dial` connections for this call scope, keyed by an opaque id. A tokio mutex so a
    /// `conn.read`/`write` can hold the stream across its await without making the guard non-Send.
    conns: tokio::sync::Mutex<std::collections::HashMap<u64, flux_system::net::DialStream>>,
    next_conn: std::sync::atomic::AtomicU64,
    /// `blob.*` content-addressed scratch store for this call scope: `sha256-hex -> (name, bytes)`.
    blobs: tokio::sync::Mutex<std::collections::HashMap<String, (String, Vec<u8>)>>,
}

impl SystemHostCaps {
    pub fn new(system: Arc<flux_system::System>) -> Self {
        Self {
            system,
            allow_private_net: false,
            grants: PluginCapabilities::default(),
            auth: Vec::new(),
            endpoints: Vec::new(),
            conns: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            blobs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn allow_private_net(mut self, yes: bool) -> Self {
        self.allow_private_net = yes;
        self
    }

    /// Restrict this host's callbacks to the capabilities the plugin declared in its manifest.
    pub fn with_grants(mut self, grants: PluginCapabilities) -> Self {
        self.grants = grants;
        self
    }

    /// Pin this host to a plugin's whole manifest: its capability grants, auth methods (for
    /// secret-by-purpose resolution), and endpoints. The one-call setup for [`load_plugin_tools`].
    pub fn with_manifest(mut self, m: &PluginManifest) -> Self {
        self.grants = m.capabilities.clone();
        self.auth = m.auth.clone();
        self.endpoints = m.endpoints.clone();
        self
    }

    /// Resolve a secret **by purpose**: find the auth method, try its env keys in order; each key must
    /// also be in the plugin's granted `secrets`. Returns the first value set, else an error.
    fn resolve_purpose(&self, purpose: &str) -> std::result::Result<String, String> {
        let method = self
            .auth
            .iter()
            .find(|a| a.purpose == purpose)
            .ok_or_else(|| format!("no auth method declared for purpose `{purpose}`"))?;
        for key in &method.env {
            if !self.grants.secrets.iter().any(|k| k == key) {
                continue; // not a granted secret — skip
            }
            if let Some(v) = self.system.env(key) {
                return Ok(v);
            }
        }
        Err(format!(
            "no granted env value for purpose `{purpose}` (tried {:?})",
            method.env
        ))
    }

    /// Resolve a named endpoint base URL from its declared env keys (config, not a secret).
    fn resolve_endpoint(&self, name: &str) -> std::result::Result<String, String> {
        let ep = self
            .endpoints
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("no endpoint declared named `{name}`"))?;
        for key in &ep.env {
            if let Some(v) = self.system.env(key) {
                return Ok(v);
            }
        }
        Err(format!(
            "no env value for endpoint `{name}` (tried {:?})",
            ep.env
        ))
    }

    /// Resolve the username half of Basic auth from a method's `user_env` (config, not a gated secret —
    /// resolved directly from declared env, like an endpoint).
    fn resolve_user(&self, user_env: &[String]) -> std::result::Result<String, String> {
        for key in user_env {
            if let Some(v) = self.system.env(key) {
                return Ok(v);
            }
        }
        Err(format!(
            "no env value for basic-auth username (tried {user_env:?})"
        ))
    }

    /// Decide what auth the host injects into an `http.do` request: the legacy `bearer_purpose` (always
    /// Bearer) or `auth_purpose` (respects the declared [`AuthScheme`]). Pure given the resolved env, so
    /// it is unit-testable without a network round-trip.
    fn resolve_auth(&self, payload: &Value) -> std::result::Result<AuthInjection, String> {
        if let Some(p) = payload.get("bearer_purpose").and_then(|v| v.as_str()) {
            return Ok(AuthInjection::Bearer(self.resolve_purpose(p)?));
        }
        let Some(p) = payload.get("auth_purpose").and_then(|v| v.as_str()) else {
            return Ok(AuthInjection::None);
        };
        let method = self
            .auth
            .iter()
            .find(|a| a.purpose == p)
            .ok_or_else(|| format!("no auth method declared for purpose `{p}`"))?;
        let scheme = method.scheme.clone();
        let user_env = method.user_env.clone();
        let secret = self.resolve_purpose(p)?;
        Ok(match scheme {
            AuthScheme::Bearer => AuthInjection::Bearer(secret),
            AuthScheme::Basic => AuthInjection::Basic {
                user: self.resolve_user(&user_env)?,
                secret,
            },
            AuthScheme::Header { name } => AuthInjection::Header {
                name,
                value: secret,
            },
            AuthScheme::Query { name } => AuthInjection::Query {
                name,
                value: secret,
            },
        })
    }
}

/// The auth the host injects into an `http.do` request, resolved from the payload + manifest. The
/// secret never crosses back to the plugin on this path.
#[derive(Debug, PartialEq)]
enum AuthInjection {
    None,
    Bearer(String),
    Basic { user: String, secret: String },
    Header { name: String, value: String },
    Query { name: String, value: String },
}

#[async_trait]
impl HostCapabilities for SystemHostCaps {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String> {
        match command {
            "process.run" => {
                let argv: Vec<String> = payload
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if argv.is_empty() {
                    return Err("process.run: `argv` (non-empty array) required".into());
                }
                // The plugin may only run programs it declared in its manifest's capabilities.
                if !self.grants.process.iter().any(|p| p == &argv[0]) {
                    return Err(format!(
                        "process.run: program `{}` not in this plugin's granted capabilities",
                        argv[0]
                    ));
                }
                let secs = payload
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(60);
                let out = self
                    .system
                    .run(&argv, std::time::Duration::from_secs(secs))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({ "stdout": out.stdout, "stderr": out.stderr, "exit_code": out.exit_code }),
                )
            }
            "secret" => {
                // Resolve by `purpose` (auth-method indirection) or a direct `key`. Either way only
                // granted env keys are read — never arbitrary host secrets.
                if let Some(purpose) = payload.get("purpose").and_then(|v| v.as_str()) {
                    return self.resolve_purpose(purpose).map(|v| json!({ "value": v }));
                }
                let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
                if !self.grants.secrets.iter().any(|k| k == key) {
                    return Err(format!(
                        "secret `{key}` not in this plugin's granted capabilities"
                    ));
                }
                match self.system.env(key) {
                    Some(v) => Ok(json!({ "value": v })),
                    None => Err(format!("secret `{key}` not set")),
                }
            }
            "endpoint" => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.resolve_endpoint(name).map(|url| json!({ "url": url }))
            }
            "http.do" => {
                if !self.grants.http {
                    return Err("http.do not granted to this plugin".into());
                }
                let raw = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let mut url = guard_http_url(raw, self.allow_private_net)?;
                let method = payload
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET")
                    .to_uppercase();
                let m = reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|e| format!("http.do: bad method `{method}`: {e}"))?;
                // Auth injection by purpose: the host resolves the secret and injects it per the
                // method's declared scheme — the plugin never sees raw tokens on this path. `Query`
                // mutates the URL, so resolve before building the request.
                let inject = self.resolve_auth(payload)?;
                if let AuthInjection::Query { name, value } = &inject {
                    url.query_pairs_mut().append_pair(name, value);
                }
                let mut req = reqwest::Client::new().request(m, url);
                if let Some(headers) = payload.get("headers").and_then(|v| v.as_object()) {
                    for (k, v) in headers {
                        if let Some(s) = v.as_str() {
                            req = req.header(k.as_str(), s);
                        }
                    }
                }
                match inject {
                    AuthInjection::None | AuthInjection::Query { .. } => {}
                    AuthInjection::Bearer(t) => req = req.bearer_auth(t),
                    AuthInjection::Basic { user, secret } => {
                        req = req.basic_auth(user, Some(secret))
                    }
                    AuthInjection::Header { name, value } => req = req.header(name.as_str(), value),
                }
                if let Some(body) = payload.get("body").and_then(|v| v.as_str()) {
                    req = req.body(body.to_string());
                }
                let resp = req.send().await.map_err(|e| e.to_string())?;
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                let body = truncate_on_char_boundary(body, 256 * 1024);
                Ok(json!({ "status": status, "body": body }))
            }
            "conn.dial" => {
                let kind = payload
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tcp");
                let target = match kind {
                    "tcp" => {
                        let host = payload
                            .get("host")
                            .and_then(|v| v.as_str())
                            .ok_or("conn.dial: `host` required for tcp")?
                            .to_string();
                        let port = payload
                            .get("port")
                            .and_then(|v| v.as_u64())
                            .ok_or("conn.dial: `port` required for tcp")?
                            as u16;
                        flux_system::net::DialTarget::Tcp { host, port }
                    }
                    "unix" => {
                        let path = payload
                            .get("path")
                            .and_then(|v| v.as_str())
                            .ok_or("conn.dial: `path` required for unix")?
                            .to_string();
                        flux_system::net::DialTarget::Unix { path }
                    }
                    other => return Err(format!("conn.dial: unknown kind `{other}`")),
                };
                let tstr = conn_target_str(&target);
                if !conn_granted(&self.grants.conn, &tstr) {
                    return Err(format!(
                        "conn.dial: target `{tstr}` not in this plugin's granted conn capabilities"
                    ));
                }
                let stream = flux_system::net::dial(&target, self.allow_private_net)
                    .await
                    .map_err(|e| e.to_string())?;
                let id = self
                    .next_conn
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.conns.lock().await.insert(id, stream);
                Ok(json!({ "conn_id": id }))
            }
            "conn.read" => {
                let id = payload
                    .get("conn_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("conn.read: `conn_id` required")?;
                let max = payload
                    .get("max")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(64 * 1024)
                    .min(1024 * 1024) as usize;
                let mut guard = self.conns.lock().await;
                let stream = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("conn.read: no open connection {id}"))?;
                let data = stream.read(max).await.map_err(|e| e.to_string())?;
                let eof = data.is_empty();
                Ok(json!({
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(&data),
                    "eof": eof
                }))
            }
            "conn.write" => {
                let id = payload
                    .get("conn_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("conn.write: `conn_id` required")?;
                let data_b64 = payload
                    .get("data_b64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let data = base64::engine::general_purpose::STANDARD
                    .decode(data_b64)
                    .map_err(|e| format!("conn.write: bad base64: {e}"))?;
                let mut guard = self.conns.lock().await;
                let stream = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("conn.write: no open connection {id}"))?;
                stream.write_all(&data).await.map_err(|e| e.to_string())?;
                Ok(json!({ "written": data.len() }))
            }
            "conn.close" => {
                let id = payload
                    .get("conn_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("conn.close: `conn_id` required")?;
                if let Some(mut stream) = self.conns.lock().await.remove(&id) {
                    let _ = stream.shutdown().await;
                }
                Ok(json!({ "ok": true }))
            }
            "blob.put" => {
                if !self.grants.blob {
                    return Err("blob.put not granted to this plugin".into());
                }
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let b64 = payload
                    .get("data_b64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let data = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| format!("blob.put: bad base64: {e}"))?;
                let mut h = Sha256::new();
                h.update(&data);
                let blob_ref = h
                    .finalize()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                self.blobs
                    .lock()
                    .await
                    .insert(blob_ref.clone(), (name, data));
                Ok(json!({ "blob_ref": blob_ref }))
            }
            "blob.get" => {
                if !self.grants.blob {
                    return Err("blob.get not granted to this plugin".into());
                }
                let r = payload
                    .get("blob_ref")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let guard = self.blobs.lock().await;
                let (_, data) = guard
                    .get(r)
                    .ok_or_else(|| format!("blob.get: no blob {r}"))?;
                Ok(json!({ "data_b64": base64::engine::general_purpose::STANDARD.encode(data) }))
            }
            "blob.info" => {
                if !self.grants.blob {
                    return Err("blob.info not granted to this plugin".into());
                }
                let r = payload
                    .get("blob_ref")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let guard = self.blobs.lock().await;
                let (name, data) = guard
                    .get(r)
                    .ok_or_else(|| format!("blob.info: no blob {r}"))?;
                Ok(json!({ "name": name, "size": data.len(), "sha256": r }))
            }
            other => Err(format!("unknown host capability: {other}")),
        }
    }
}

/// The canonical grant string for a dial target (`tcp:host:port` / `unix:/path`).
fn conn_target_str(t: &flux_system::net::DialTarget) -> String {
    match t {
        flux_system::net::DialTarget::Tcp { host, port } => format!("tcp:{host}:{port}"),
        flux_system::net::DialTarget::Unix { path } => format!("unix:{path}"),
    }
}

/// Whether a plugin's `conn` grant list permits `target`. Entries match exactly or with a single `*`
/// wildcard segment (e.g. `tcp:*:5432`, `tcp:db.internal:*`, `unix:/var/run/*.sock`).
fn conn_granted(grants: &[String], target: &str) -> bool {
    grants.iter().any(|g| conn_glob(g, target))
}

/// Match a pattern with at most one `*` wildcard against a string.
fn conn_glob(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        Some((pre, suf)) => {
            s.len() >= pre.len() + suf.len() && s.starts_with(pre) && s.ends_with(suf)
        }
        None => pat == s,
    }
}

/// Truncate a `String` to at most `max` bytes without splitting a UTF-8 codepoint (`String::truncate`
/// panics off a char boundary on attacker-controlled bodies).
fn truncate_on_char_boundary(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}

/// Reject non-HTTP(S) schemes and (unless `allow_private`) private/loopback/link-local hosts —
/// delegating to the shared egress guard in `flux-system` (host→IP resolution, IPv6/IPv4-mapped
/// coverage), the same SSRF policy the agent's own `web_fetch` uses.
fn guard_http_url(raw: &str, allow_private: bool) -> std::result::Result<url::Url, String> {
    flux_system::net::guard_url(raw, allow_private).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Plugin host (host side) — async, spawns the subprocess
// ---------------------------------------------------------------------------

/// A running plugin subprocess the host talks to over framed stdio.
pub struct PluginHost {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl PluginHost {
    /// Spawn a plugin binary.
    pub async fn spawn(program: &str, args: &[String]) -> Result<Self> {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::Other(format!("spawn plugin {program}: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Other("plugin stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Other("plugin stdout unavailable".into()))?;
        Ok(Self {
            child,
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            next_id: 0,
        })
    }

    async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(frame)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(Error::Io)?;
        self.stdin.flush().await.map_err(Error::Io)?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Frame> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        // Bound a single framed message so a malicious/buggy plugin can't OOM the host by emitting
        // a gigantic line with no newline. `Take` caps the bytes `read_until` will consume.
        const MAX_FRAME: usize = 8 * 1024 * 1024;
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_FRAME as u64)
            .read_until(b'\n', &mut buf)
            .await
            .map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::Provider("plugin closed the connection".into()));
        }
        if buf.last() != Some(&b'\n') {
            return Err(Error::Provider(
                "plugin frame exceeded the size limit (no newline within bound)".into(),
            ));
        }
        let line = std::str::from_utf8(&buf)
            .map_err(|e| Error::Provider(format!("plugin frame not valid UTF-8: {e}")))?;
        Ok(serde_json::from_str(line.trim())?)
    }

    async fn request(&mut self, command: &str, payload: Value) -> Result<Frame> {
        self.next_id += 1;
        let frame = Frame::request(format!("r{}", self.next_id), command, payload);
        self.write_frame(&frame).await?;
        self.read_frame().await
    }

    /// Fetch the plugin's manifest.
    pub async fn manifest(&mut self) -> Result<PluginManifest> {
        let f = self.request("manifest", Value::Null).await?;
        if !f.ok {
            return Err(Error::Provider(f.error.unwrap_or_default()));
        }
        Ok(serde_json::from_value(f.result)?)
    }

    /// Call an operation with no host capabilities (callbacks are denied).
    pub async fn call(&mut self, operation: &str, input: Value) -> Result<Value> {
        self.call_with_host(operation, input, &DenyHostCaps).await
    }

    /// Call an operation, servicing any plugin→host capability callbacks via `host` until the
    /// operation's own response arrives. Callbacks and the final response are multiplexed on the
    /// same channel and demultiplexed by frame kind + id.
    pub async fn call_with_host(
        &mut self,
        operation: &str,
        input: Value,
        host: &dyn HostCapabilities,
    ) -> Result<Value> {
        self.next_id += 1;
        let call_id = format!("r{}", self.next_id);
        let frame = Frame::request(
            &call_id,
            "operation.call",
            json!({ "operation": operation, "input": input }),
        );
        self.write_frame(&frame).await?;

        loop {
            let f = self.read_frame().await?;
            match f.kind {
                FrameKind::Request => {
                    // A host-capability callback from the plugin — service it and reply.
                    let reply = match host.handle(&f.command, &f.payload).await {
                        Ok(v) => Frame::ok_response(&f.id, v),
                        Err(e) => Frame::err_response(&f.id, e),
                    };
                    self.write_frame(&reply).await?;
                }
                FrameKind::Response => {
                    if f.id == call_id {
                        return if f.ok {
                            Ok(f.result)
                        } else {
                            Err(Error::Provider(f.error.unwrap_or_default()))
                        };
                    }
                    // A stray/duplicate response — ignore and keep reading.
                }
            }
        }
    }

    /// Terminate the plugin.
    pub async fn shutdown(mut self) -> Result<()> {
        drop(self.stdin); // closing stdin lets a well-behaved plugin exit
        let _ = self.child.kill().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PluginTool — project a plugin operation as an agent tool
// ---------------------------------------------------------------------------

/// Projects a single plugin [`OperationSpec`] as an agent [`Tool`]. All of a plugin's operations
/// share one [`PluginHost`] subprocess behind a mutex; each call goes through the same safety
/// envelope as built-in tools and may issue host-capability callbacks via [`HostCapabilities`].
pub struct PluginTool {
    host: Arc<tokio::sync::Mutex<PluginHost>>,
    caps: Arc<dyn HostCapabilities>,
    plugin: String,
    operation: String,
    spec: ToolSpec,
}

impl PluginTool {
    pub fn new(
        host: Arc<tokio::sync::Mutex<PluginHost>>,
        caps: Arc<dyn HostCapabilities>,
        plugin: &str,
        op: &OperationSpec,
    ) -> Self {
        // Project the operation's declared effects so the authorization floor gates it like any
        // built-in tool. An operation that declares none could still touch the network or run a
        // process via host capabilities, so default to those — under the default grants that forces
        // approval rather than letting the op slip the envelope.
        let effects = if op.effects.is_empty() {
            vec![Effect::Process, Effect::Network]
        } else {
            op.effects.clone()
        };
        let spec = ToolSpec {
            name: format!("{plugin}.{}", op.name),
            description: op.description.clone(),
            input_schema: op.input_schema.clone(),
            output_schema: None,
            effects,
            risk: op.risk.unwrap_or(Risk::Medium),
            idempotency: op.idempotency.unwrap_or(Idempotency::NonIdempotent),
            access: Vec::new(),
            group: None,
        };
        Self {
            host,
            caps,
            plugin: plugin.to_string(),
            operation: op.name.clone(),
            spec,
        }
    }
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![format!("{}.{}", self.plugin, self.operation)]
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut host = self.host.lock().await;
        match host
            .call_with_host(&self.operation, params, self.caps.as_ref())
            .await
        {
            Ok(v) => Ok(ToolResult::ok(
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
            )),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}

/// Spawn a plugin, fetch its manifest, and project every operation as a [`PluginTool`] sharing one
/// host connection. Returns the tools plus the shared host handle (keep it alive for the session).
///
/// `make_caps` builds the host capabilities *from the fetched manifest*, so the caps can be scoped
/// to exactly what the plugin declared (see [`SystemHostCaps::with_grants`]) — the binding point
/// where a plugin's requested privileges are pinned to its manifest.
pub async fn load_plugin_tools(
    program: &str,
    args: &[String],
    make_caps: impl FnOnce(&PluginManifest) -> Arc<dyn HostCapabilities>,
) -> Result<(Vec<Arc<dyn Tool>>, Arc<tokio::sync::Mutex<PluginHost>>)> {
    let mut host = PluginHost::spawn(program, args).await?;
    let manifest = host.manifest().await?;
    let caps = make_caps(&manifest);
    let host = Arc::new(tokio::sync::Mutex::new(host));
    let tools: Vec<Arc<dyn Tool>> = manifest
        .operations
        .iter()
        .map(|op| {
            Arc::new(PluginTool::new(
                host.clone(),
                caps.clone(),
                &manifest.name,
                op,
            )) as Arc<dyn Tool>
        })
        .collect();
    Ok((tools, host))
}

// ---------------------------------------------------------------------------
// Discovery & lifecycle — descriptors under ~/.flux/plugins/<name>.toml
// ---------------------------------------------------------------------------

/// A persisted plugin descriptor (`~/.flux/plugins/<name>.toml`): how to launch the plugin plus an
/// optional pinned version. `flux plugin add|ls|pin|rollback` manage these; discovery loads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// The plugin executable (absolute path or a name on `PATH`).
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The pinned version, if any (advisory; surfaced by `flux plugin ls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

/// A discovered plugin: its name (the descriptor file stem) and how to launch it.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub name: String,
    pub descriptor: PluginDescriptor,
}

fn descriptor_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    dir.join(format!("{name}.toml"))
}

/// Read every `<dir>/*.toml` plugin descriptor (sorted by name). Missing dir → empty; malformed
/// descriptors are skipped (never fail discovery for one bad file).
pub fn discover(dir: &std::path::Path) -> Vec<DiscoveredPlugin> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(descriptor) = toml::from_str::<PluginDescriptor>(&text) {
                out.push(DiscoveredPlugin { name, descriptor });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Write a plugin descriptor to `<dir>/<name>.toml`, creating `dir` if needed.
pub fn add_descriptor(
    dir: &std::path::Path,
    name: &str,
    descriptor: &PluginDescriptor,
) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    let body = toml::to_string_pretty(descriptor)
        .map_err(|e| Error::Other(format!("serialize descriptor: {e}")))?;
    std::fs::write(descriptor_path(dir, name), body).map_err(Error::Io)?;
    Ok(())
}

/// Load a single named descriptor, if present.
pub fn load_descriptor(dir: &std::path::Path, name: &str) -> Result<Option<PluginDescriptor>> {
    match std::fs::read_to_string(descriptor_path(dir, name)) {
        Ok(text) => {
            Ok(Some(toml::from_str(&text).map_err(|e| {
                Error::Other(format!("parse descriptor: {e}"))
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Set or clear the pinned version of a plugin (`flux plugin pin` / `rollback`).
pub fn set_pinned(dir: &std::path::Path, name: &str, version: Option<String>) -> Result<()> {
    let mut d = load_descriptor(dir, name)?
        .ok_or_else(|| Error::Other(format!("no such plugin: {name}")))?;
    d.pinned = version;
    add_descriptor(dir, name, &d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn host_caps_deny_ungranted_and_allow_granted() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-caps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A fresh SystemHostCaps grants nothing.
        let none = SystemHostCaps::new(sys.clone());
        assert!(
            none.handle("process.run", &json!({"argv": ["echo", "hi"]}))
                .await
                .is_err(),
            "ungranted process.run must be denied"
        );
        assert!(
            none.handle("secret", &json!({"key": "PATH"}))
                .await
                .is_err(),
            "ungranted secret must be denied (no arbitrary env reads)"
        );
        assert!(
            none.handle("http.do", &json!({"url": "http://example.com"}))
                .await
                .is_err(),
            "ungranted http.do must be denied"
        );

        // Granting only `echo` lets echo run but nothing else; secret stays denied.
        let limited = SystemHostCaps::new(sys.clone()).with_grants(PluginCapabilities {
            process: vec!["echo".into()],
            secrets: vec![],
            http: false,
            ..Default::default()
        });
        assert!(
            limited
                .handle("process.run", &json!({"argv": ["echo", "hi"]}))
                .await
                .is_ok(),
            "a granted program should run"
        );
        assert!(
            limited
                .handle("process.run", &json!({"argv": ["cat", "/etc/passwd"]}))
                .await
                .is_err(),
            "a non-granted program must be denied"
        );
        assert!(
            limited
                .handle("secret", &json!({"key": "PATH"}))
                .await
                .is_err(),
            "secret not in the grant list must be denied"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn secret_by_purpose_and_endpoint_resolution() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-purpose-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        // Unique env keys so the process-global set_var doesn't collide with other tests.
        std::env::set_var("FLUX_TEST_API_TOKEN_XZ", "s3cr3t");
        std::env::set_var("FLUX_TEST_GITLAB_URL_XZ", "https://gl.example.com");

        let manifest = PluginManifest {
            name: "gl".into(),
            auth: vec![AuthMethod {
                purpose: "api_token".into(),
                env: vec!["FLUX_TEST_API_TOKEN_XZ".into()],
                description: String::new(),
                ..Default::default()
            }],
            endpoints: vec![EndpointSpec {
                name: "gitlab.endpoint".into(),
                env: vec!["FLUX_TEST_GITLAB_URL_XZ".into()],
                description: String::new(),
            }],
            capabilities: PluginCapabilities {
                secrets: vec!["FLUX_TEST_API_TOKEN_XZ".into()],
                http: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys).with_manifest(&manifest);

        // secret-by-purpose resolves the granted env key
        let got = caps
            .handle("secret", &json!({"purpose": "api_token"}))
            .await
            .unwrap();
        assert_eq!(got["value"], "s3cr3t");
        // endpoint resolves from its declared env
        let ep = caps
            .handle("endpoint", &json!({"name": "gitlab.endpoint"}))
            .await
            .unwrap();
        assert_eq!(ep["url"], "https://gl.example.com");
        // an undeclared purpose is denied
        assert!(caps
            .handle("secret", &json!({"purpose": "nope"}))
            .await
            .is_err());

        std::env::remove_var("FLUX_TEST_API_TOKEN_XZ");
        std::env::remove_var("FLUX_TEST_GITLAB_URL_XZ");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn auth_injection_resolves_per_scheme() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-authinj-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var("FLUX_TEST_BEARER_AJ", "bear-tok");
        std::env::set_var("FLUX_TEST_BASIC_TOK_AJ", "basic-tok");
        std::env::set_var("FLUX_TEST_BASIC_USER_AJ", "user@example.com");
        std::env::set_var("FLUX_TEST_HDR_AJ", "hdr-tok");
        std::env::set_var("FLUX_TEST_QRY_AJ", "qry-tok");

        let manifest = PluginManifest {
            name: "multi".into(),
            auth: vec![
                AuthMethod::bearer("bear", vec!["FLUX_TEST_BEARER_AJ".into()]),
                AuthMethod::basic(
                    "basic",
                    vec!["FLUX_TEST_BASIC_USER_AJ".into()],
                    vec!["FLUX_TEST_BASIC_TOK_AJ".into()],
                ),
                AuthMethod::header("genie", "GenieKey", vec!["FLUX_TEST_HDR_AJ".into()]),
                AuthMethod {
                    purpose: "qry".into(),
                    env: vec!["FLUX_TEST_QRY_AJ".into()],
                    scheme: AuthScheme::Query {
                        name: "apikey".into(),
                    },
                    ..Default::default()
                },
            ],
            capabilities: PluginCapabilities {
                secrets: vec![
                    "FLUX_TEST_BEARER_AJ".into(),
                    "FLUX_TEST_BASIC_TOK_AJ".into(),
                    "FLUX_TEST_HDR_AJ".into(),
                    "FLUX_TEST_QRY_AJ".into(),
                ],
                http: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys).with_manifest(&manifest);

        // legacy bearer_purpose → Bearer (unchanged behaviour)
        assert_eq!(
            caps.resolve_auth(&json!({"bearer_purpose": "bear"}))
                .unwrap(),
            AuthInjection::Bearer("bear-tok".into())
        );
        // auth_purpose respects each declared scheme
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "bear"})).unwrap(),
            AuthInjection::Bearer("bear-tok".into())
        );
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "basic"}))
                .unwrap(),
            AuthInjection::Basic {
                user: "user@example.com".into(),
                secret: "basic-tok".into()
            }
        );
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "genie"}))
                .unwrap(),
            AuthInjection::Header {
                name: "GenieKey".into(),
                value: "hdr-tok".into()
            }
        );
        assert_eq!(
            caps.resolve_auth(&json!({"auth_purpose": "qry"})).unwrap(),
            AuthInjection::Query {
                name: "apikey".into(),
                value: "qry-tok".into()
            }
        );
        // no auth requested → None; undeclared purpose → error
        assert_eq!(caps.resolve_auth(&json!({})).unwrap(), AuthInjection::None);
        assert!(caps.resolve_auth(&json!({"auth_purpose": "nope"})).is_err());

        for k in [
            "FLUX_TEST_BEARER_AJ",
            "FLUX_TEST_BASIC_TOK_AJ",
            "FLUX_TEST_BASIC_USER_AJ",
            "FLUX_TEST_HDR_AJ",
            "FLUX_TEST_QRY_AJ",
        ] {
            std::env::remove_var(k);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn conn_dial_round_trips_and_is_gated() {
        use flux_system::{System, Workspace};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = std::env::temp_dir().join(format!("flux-conn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A loopback echo server (hermetic).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 64];
                if let Ok(n) = sock.read(&mut buf).await {
                    let _ = sock.write_all(&buf[..n]).await;
                }
            }
        });
        let dial = json!({"kind": "tcp", "host": "127.0.0.1", "port": port});

        // Ungranted conn.dial is denied even with private-net allowed (the grant is the gate).
        let none = SystemHostCaps::new(sys.clone()).allow_private_net(true);
        assert!(none.handle("conn.dial", &dial).await.is_err());

        // Granted (loopback wildcard) → dial/write/read/close round-trips.
        let caps = SystemHostCaps::new(sys)
            .allow_private_net(true)
            .with_grants(PluginCapabilities {
                conn: vec!["tcp:127.0.0.1:*".into()],
                ..Default::default()
            });
        let id = caps.handle("conn.dial", &dial).await.unwrap()["conn_id"]
            .as_u64()
            .unwrap();
        let ping = base64::engine::general_purpose::STANDARD.encode(b"ping");
        let wrote = caps
            .handle("conn.write", &json!({"conn_id": id, "data_b64": ping}))
            .await
            .unwrap();
        assert_eq!(wrote["written"], 4);
        let read = caps
            .handle("conn.read", &json!({"conn_id": id, "max": 64}))
            .await
            .unwrap();
        let got = base64::engine::general_purpose::STANDARD
            .decode(read["data_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(&got, b"ping");
        caps.handle("conn.close", &json!({"conn_id": id}))
            .await
            .unwrap();
        // reading a closed/unknown connection errors
        assert!(caps
            .handle("conn.read", &json!({"conn_id": id, "max": 8}))
            .await
            .is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn blob_put_get_info_round_trips_and_is_gated() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-blob-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let payload = base64::engine::general_purpose::STANDARD.encode(b"hello blob");

        // Ungranted blob.* is denied.
        let none = SystemHostCaps::new(sys.clone());
        assert!(none
            .handle("blob.put", &json!({"name": "x", "data_b64": payload}))
            .await
            .is_err());

        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            blob: true,
            ..Default::default()
        });
        let put = caps
            .handle(
                "blob.put",
                &json!({"name": "greeting.txt", "data_b64": payload}),
            )
            .await
            .unwrap();
        let r = put["blob_ref"].as_str().unwrap().to_string();
        // content-addressed: same content → same ref
        let put2 = caps
            .handle(
                "blob.put",
                &json!({"name": "again.txt", "data_b64": payload}),
            )
            .await
            .unwrap();
        assert_eq!(put2["blob_ref"].as_str().unwrap(), r);

        let info = caps
            .handle("blob.info", &json!({"blob_ref": r}))
            .await
            .unwrap();
        assert_eq!(info["size"], 10);
        assert_eq!(info["sha256"], r);

        let got = caps
            .handle("blob.get", &json!({"blob_ref": r}))
            .await
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(got["data_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(&bytes, b"hello blob");

        // unknown ref errors
        assert!(caps
            .handle("blob.get", &json!({"blob_ref": "deadbeef"}))
            .await
            .is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncate_on_char_boundary_never_panics() {
        let s = format!("{}é", "a".repeat(100)); // multibyte char near the cut
                                                 // Cut at a byte that lands inside the 'é' — must not panic and stays valid UTF-8.
        let out = truncate_on_char_boundary(s.clone(), 101);
        assert!(out.len() <= 101);
        assert!(out.is_char_boundary(out.len()));
        assert_eq!(truncate_on_char_boundary("short".into(), 1024), "short");
    }

    #[test]
    fn frame_roundtrips_as_ndjson() {
        let f = Frame::request("r1", "manifest", Value::Null);
        let line = serde_json::to_string(&f).unwrap();
        assert!(!line.contains('\n'));
        let back: Frame = serde_json::from_str(&line).unwrap();
        assert_eq!(back.command, "manifest");
        assert_eq!(back.kind, FrameKind::Request);
    }

    #[test]
    fn responses_carry_ok_and_error() {
        let ok = Frame::ok_response("r1", serde_json::json!({"x": 1}));
        assert!(ok.ok);
        assert_eq!(ok.result["x"], 1);
        let err = Frame::err_response("r1", "boom");
        assert!(!err.ok);
        assert_eq!(err.error.as_deref(), Some("boom"));
    }

    #[test]
    fn descriptors_add_discover_pin_rollback() {
        let dir = std::env::temp_dir().join(format!("flux-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // missing dir → empty discovery
        assert!(discover(&dir).is_empty());

        add_descriptor(
            &dir,
            "gitlab",
            &PluginDescriptor {
                program: "/usr/bin/gitlab-plugin".into(),
                args: vec!["--v2".into()],
                pinned: None,
            },
        )
        .unwrap();
        add_descriptor(
            &dir,
            "slack",
            &PluginDescriptor {
                program: "slack-plugin".into(),
                args: vec![],
                pinned: None,
            },
        )
        .unwrap();

        let found = discover(&dir);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "gitlab"); // sorted
        assert_eq!(found[0].descriptor.args, vec!["--v2"]);

        set_pinned(&dir, "gitlab", Some("1.2.3".into())).unwrap();
        assert_eq!(
            load_descriptor(&dir, "gitlab")
                .unwrap()
                .unwrap()
                .pinned
                .as_deref(),
            Some("1.2.3")
        );
        set_pinned(&dir, "gitlab", None).unwrap(); // rollback clears the pin
        assert!(load_descriptor(&dir, "gitlab")
            .unwrap()
            .unwrap()
            .pinned
            .is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}

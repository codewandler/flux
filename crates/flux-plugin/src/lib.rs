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
use flux_system::net::PrivateNetAllow;

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
    /// **Host-only op** (C-09a): when `true` this op is NOT advertised to the LLM as a callable
    /// tool — it is an internal host-dispatched channel. The canonical case is the `aws-bedrock`
    /// plugin's `auth` op, which returns raw AWS credentials: the model must never call it, or the
    /// keys would appear in the tool result (a leak). The op stays callable by the host via the
    /// shared `PluginHost` handle (exactly how the endpoint broker calls `endpoint.discover`);
    /// only the *projection* as an agent tool is suppressed (see [`visible_ops`]). Defaults
    /// `false`, so every existing manifest that says nothing about `internal` projects all its
    /// ops unchanged.
    #[serde(default)]
    pub internal: bool,
}

/// The manifest-declared, deny-by-default operations that are projected as agent tools: every
/// op whose [`OperationSpec::internal`] flag is `false`. Host-only (`internal: true`) ops are
/// excluded — they are still dispatchable by the host via the shared `PluginHost` handle, just
/// not advertised to the model. This is the single filter [`load_plugin_tools`] applies.
pub fn visible_ops(manifest: &PluginManifest) -> impl Iterator<Item = &OperationSpec> {
    manifest.operations.iter().filter(|op| !op.internal)
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
    /// Allowed public/fallback hosts for this endpoint. Env-resolved endpoint hosts are allowed too.
    #[serde(default)]
    pub http_hosts: Vec<String>,
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
    /// Allowed public HTTP hosts for `http.do` in addition to declared endpoint hosts.
    #[serde(default)]
    pub http_hosts: Vec<String>,
    /// Declared hosts this plugin may reach at private/loopback addresses when the operator grants them.
    #[serde(default)]
    pub private_hosts: Vec<String>,
    /// Allowed `conn.dial` targets (`tcp:host:port` / `unix:/path`; a single `*` wildcards one
    /// segment, e.g. `tcp:*:5432`). Empty = the `conn.*` capability is denied.
    #[serde(default)]
    pub conn: Vec<String>,
    /// Whether the `blob.*` capability (content-addressed scratch store) is permitted.
    #[serde(default)]
    pub blob: bool,
    /// Whether the `endpoint.discover` host capability (cross-plugin endpoint discovery, D-26) is
    /// permitted. Deny-by-default like every other capability: a consumer plugin can only ask the
    /// host "what endpoints exist for product X?" if its manifest set this.
    #[serde(default)]
    pub discover: bool,
    /// Whether the `credential` host capability (D-27) is permitted: materializing a credential
    /// *reference* into the raw secret value, delivered to the trusted plugin binary for in-band-auth
    /// raw-socket protocols (e.g. Postgres SCRAM needs the password). Deny-by-default — the host
    /// refuses `credential` unless this plugin's manifest set it. The value is registered with the
    /// [`Redactor`](flux_secret::Redactor) so it never leaks into model-visible output, and is NEVER
    /// returned through any discovery/endpoint path — only this explicit, audited capability.
    #[serde(default)]
    pub credential: bool,
    /// **Path-scoped host-file reads** (C-09a): a deny-by-default `fs.read` capability for reading
    /// HOST files outside the workspace jail (which `System::read_file` cannot reach) — e.g. the
    /// `aws-bedrock` plugin reading `~/.aws/config` + `~/.aws/sso/cache` (the SSO refresh-token
    /// cache) to resolve the credential chain without an `aws` CLI. The host reads ONLY paths that
    /// match a declared [`FsReadScope`]; anything out of scope is refused; `..` traversal is
    /// rejected; and a scope marked `secret: true` has its content registered with the
    /// [`Redactor`](flux_secret::Redactor) so refresh tokens can never leak into model-visible
    /// output. Empty = `fs.read` denied (the default).
    #[serde(default)]
    pub fs: Vec<FsReadScope>,
}

/// One path scope the host may read on a plugin's behalf via the `fs.read` capability (C-09a).
/// `path` is a glob: an exact path, or a directory prefix with `/**` (matches the dir itself +
/// everything under it, incl. nested subdirs) or `/*` (direct children only). `~` expands to
/// `$HOME`. `secret: true` registers the read content with the [`Redactor`](flux_secret::Redactor)
/// — for `~/.aws/sso/cache` refresh tokens and `~/.aws/credentials` static keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FsReadScope {
    /// The path/glob this scope permits (e.g. `"~/.aws/config"`, `"~/.aws/sso/cache/**"`).
    pub path: String,
    /// Whether read content is registered with the Redactor (scrubbed from model-visible output).
    #[serde(default)]
    pub secret: bool,
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
    /// Products this plugin can **discover** endpoints for as a provider (D-26): e.g. the kubernetes
    /// plugin declares `["prometheus", "loki", "postgres", …]`. The fan-out broker matches a
    /// consumer's discovery query for product X against every provider whose `discovers` contains X.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovers: Vec<String>,
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

/// Resolves endpoint/credential **references** to their runtime form — host-side only. This is the
/// seam the L5 endpoint broker implements; [`SystemHostCaps`] consults it (when present, see
/// [`SystemHostCaps::with_resolver`]) so a plugin op can pass an `endpoint_ref` instead of a URL,
/// and the host alone turns it into a connection + injected credentials. The plugin and the model
/// never see a resolved URL-with-credentials.
///
/// A *reference* is either a **named** config/manifest endpoint (`"sql.endpoint"`) or a
/// **discovered** `@endpoint/<id>`; the resolver handles both.
#[async_trait]
pub trait ReferenceResolver: Send + Sync {
    /// Resolve an endpoint reference to its runtime form (absolute URL + any injected auth headers).
    /// Host-only — the result has no model-visible serializer.
    ///
    /// This is the consumer-agnostic form. On the IO path prefer
    /// [`resolve_endpoint_for`](Self::resolve_endpoint_for): when a discovered endpoint's credential is
    /// owned by a *different* plugin, injecting it into the request on the caller's behalf is exactly a
    /// cross-plugin credential *use*, which must be gated against the consuming plugin.
    async fn resolve_endpoint(
        &self,
        reference: &str,
    ) -> std::result::Result<flux_secret::endpoint::ResolvedEndpoint, String>;

    /// Resolve an endpoint reference **on behalf of `consumer`** (the plugin doing the IO). Identical
    /// to [`resolve_endpoint`](Self::resolve_endpoint), except a discovered endpoint's `credential_ref`
    /// is materialized as `consumer` — so when that credential is owned by another plugin, the
    /// deny-by-default cross-plugin gate (grant + first-use approval + audit) fires before the host
    /// injects it. The default ignores the consumer and delegates to `resolve_endpoint`; the L5 broker
    /// overrides it.
    async fn resolve_endpoint_for(
        &self,
        _consumer: &str,
        reference: &str,
    ) -> std::result::Result<flux_secret::endpoint::ResolvedEndpoint, String> {
        self.resolve_endpoint(reference).await
    }

    /// Materialize a credential reference to secret material — for raw-socket in-band-auth protocols
    /// (e.g. Postgres SCRAM) that must speak the handshake themselves. Host-side; the value is
    /// delivered only to the trusted plugin binary, never surfaced to the model.
    ///
    /// This is the consumer-agnostic form (no cross-plugin gate). Prefer
    /// [`resolve_credential_for`](Self::resolve_credential_for) on the IO path so the broker can
    /// enforce the deny-by-default cross-plugin grant against the *consuming* plugin.
    async fn resolve_credential(
        &self,
        reference: &flux_secret::Ref,
    ) -> std::result::Result<flux_secret::Material, String>;

    /// Materialize a credential reference **on behalf of `consumer`** (the plugin requesting it). When
    /// the credential is owned by a *different* plugin (a cross-plugin `Kubernetes`/`Plugin` scheme
    /// ref), the resolver gates the resolution against the operator's cross-plugin grant for the
    /// `(consumer, provider)` pair, an optional first-use approval, and an audit record. The default
    /// implementation ignores the consumer and delegates to [`resolve_credential`](Self::resolve_credential)
    /// — overridden by the L5 broker, which alone knows the provider graph and the grants.
    async fn resolve_credential_for(
        &self,
        _consumer: &str,
        reference: &flux_secret::Ref,
    ) -> std::result::Result<flux_secret::Material, String> {
        self.resolve_credential(reference).await
    }

    /// The credential *reference* (a location, never a value) attached to an endpoint reference, for
    /// the `credential`-by-`endpoint_ref` path. The default has no endpoint registry and errors;
    /// the L5 broker overrides it (looking the record up in the [`EndpointRegistry`]).
    async fn credential_ref_for_endpoint(
        &self,
        reference: &str,
    ) -> std::result::Result<flux_secret::Ref, String> {
        Err(format!(
            "this resolver cannot map endpoint `{reference}` to a credential reference"
        ))
    }
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

/// An audit seam: the host calls [`record_private_admit`](Self::record_private_admit) whenever it
/// admits an egress request to a **private/internal** address under a scoped grant — the auditable
/// security event. This crate (L4) only defines the trait; the concrete, `flux-events`-backed
/// implementation that appends a `PrivateNetAdmit` event lives at a surface (L6), so flux-plugin
/// stays free of an event-store dependency. A host with no audit installed simply admits silently.
pub trait EgressAudit: Send + Sync {
    /// Record that `caller` (a plugin name, or `"web_fetch"`) reached the private `host`, admitted by
    /// `grant_source` (e.g. `"config:plugin/<name>"` or `"config:endpoint/<plugin>:<ep>"`).
    fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str);
}

/// A sink for secret values the host materializes at runtime (the `credential` capability path).
/// Registering a value here ensures it is scrubbed from any model-visible output. The concrete
/// implementation lives at a surface (L6), backed by the executor's [`Redactor`](flux_secret::Redactor);
/// a host with no sink installed simply hands the value to the trusted plugin without registration.
pub trait SecretSink: Send + Sync {
    /// Register `value` as a known secret so it is redacted from captured tool output and logs.
    fn register_secret(&self, value: &str);
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
    private_net_grants: Vec<String>,
    grants: PluginCapabilities,
    auth: Vec<AuthMethod>,
    endpoints: Vec<EndpointSpec>,
    /// The caller name recorded in egress-admit audit events (the plugin's manifest name, set by
    /// [`with_manifest`](Self::with_manifest)). Defaults to `"plugin"` until a manifest is pinned.
    caller: String,
    /// How this plugin's private-net grants were sourced, recorded in audit events (defaults to a
    /// generic plugin-scope label; the surface can override via [`with_grant_source`](Self::with_grant_source)).
    grant_source: String,
    /// Optional egress-audit hook: fires when a private host is admitted under a scoped grant.
    audit: Option<Arc<dyn EgressAudit>>,
    /// Optional reference resolver (the L5 endpoint broker, injected as a trait object). When present,
    /// a plugin op may pass an `endpoint_ref` to `http.do`/`conn.dial` instead of a URL/host:port, and
    /// the host alone turns it into a connection + injected credentials — the plugin and the model
    /// never see a resolved URL-with-credentials. Also backs the gated `credential` capability.
    ///
    /// LIFETIME: the resolver is the broker, which holds the `PluginRegistry`, whose entries' caps
    /// transitively hold *this* `SystemHostCaps` → a strong `Arc` cycle. This is intentional and kept
    /// simple: the broker/registry/caps form a **session-lived** object graph torn down at process
    /// exit. It is not a per-request leak (the graph is built once at startup), so a strong `Arc` is
    /// fine; engineering a `Weak` back-edge here would add complexity for no practical benefit.
    resolver: Option<Arc<dyn ReferenceResolver>>,
    /// The consumer plugin's name passed to the resolver on the cross-plugin credential path, so the
    /// broker can gate a `(consumer, provider)` resolution. Defaults to [`caller`](Self::caller).
    consumer: String,
    /// Optional sink for credentials materialized on the `credential` capability path: the host hands
    /// the raw value to the trusted plugin binary, and registers it here so it is scrubbed from any
    /// model-visible output. Backed at the surface by the same [`Redactor`](flux_secret::Redactor) the
    /// executor redacts with.
    secret_sink: Option<Arc<dyn SecretSink>>,
    /// Open `conn.dial` connections for this call scope, keyed by an opaque id. A tokio mutex so a
    /// `conn.read`/`write` can hold the stream across its await without making the guard non-Send.
    conns: tokio::sync::Mutex<std::collections::HashMap<u64, flux_system::net::DialStream>>,
    next_conn: std::sync::atomic::AtomicU64,
    /// `blob.*` content-addressed scratch store for this call scope: `sha256-hex -> (name, bytes)`.
    blobs: tokio::sync::Mutex<std::collections::HashMap<String, (String, Vec<u8>)>>,
    /// Host-managed background processes (`process.spawn`/`read`/`status`/`kill`), keyed by an opaque
    /// id. Persists across op calls (one `SystemHostCaps` is shared for a plugin's whole session), so
    /// a `kubectl port-forward` started in one call is stopped in a later one. A tokio mutex so a
    /// handler can hold the map across the `try_wait`/drain (neither awaits, but the guard stays Send).
    procs: tokio::sync::Mutex<std::collections::HashMap<u64, ManagedProc>>,
    next_proc: std::sync::atomic::AtomicU64,
}

/// A long-lived host-managed process registered in [`SystemHostCaps::procs`].
type ManagedProc = flux_system::ManagedChild;

impl SystemHostCaps {
    pub fn new(system: Arc<flux_system::System>) -> Self {
        Self {
            system,
            private_net_grants: Vec::new(),
            grants: PluginCapabilities::default(),
            auth: Vec::new(),
            endpoints: Vec::new(),
            caller: "plugin".to_string(),
            grant_source: "config:plugin".to_string(),
            audit: None,
            resolver: None,
            consumer: "plugin".to_string(),
            secret_sink: None,
            conns: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            blobs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            procs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            next_proc: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn allow_private_net(mut self, yes: bool) -> Self {
        self.private_net_grants = if yes {
            vec!["*".to_string()]
        } else {
            Vec::new()
        };
        self
    }

    /// Operator grants for private-network egress for this plugin. These are intersected with the
    /// plugin's manifest-declared `private_hosts`.
    pub fn with_private_net_grants(mut self, hosts: Vec<String>) -> Self {
        self.private_net_grants = hosts;
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
        if !m.name.is_empty() {
            self.caller = m.name.clone();
            self.grant_source = format!("config:plugin/{}", m.name);
            self.consumer = m.name.clone();
        }
        self
    }

    /// Install an [`EgressAudit`] hook. When set, the host records a private-network-admit event the
    /// moment it lets a request to a private/internal host through under a scoped grant.
    pub fn with_egress_audit(mut self, audit: Arc<dyn EgressAudit>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Inject the [`ReferenceResolver`] (the L5 endpoint broker). With a resolver installed, a plugin
    /// op may pass an `endpoint_ref` (to `http.do`/`conn.dial`) and use the gated `credential`
    /// capability; without one, those paths return a clear "no resolver" error and the legacy
    /// URL-based paths are unaffected. See the field doc for the (intentional) session-lived Arc cycle.
    pub fn with_resolver(mut self, resolver: Arc<dyn ReferenceResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// Install a [`SecretSink`] so credentials materialized on the `credential` capability path are
    /// registered with the executor's redactor (scrubbed from any model-visible output).
    pub fn with_secret_sink(mut self, sink: Arc<dyn SecretSink>) -> Self {
        self.secret_sink = Some(sink);
        self
    }

    /// Override the `grant_source` label recorded in egress-admit audit events (e.g. an
    /// endpoint-scoped `"config:endpoint/<plugin>:<ep>"` when grants were resolved per endpoint).
    pub fn with_grant_source(mut self, grant_source: impl Into<String>) -> Self {
        self.grant_source = grant_source.into();
        self
    }

    /// Fire the egress-audit hook (if installed) when `host` is a private/internal address — i.e. the
    /// scoped grant just admitted a request the bare SSRF guard would have refused.
    fn audit_admit(&self, host: &str) {
        if let Some(audit) = &self.audit {
            if flux_system::net::host_resolves_private(host) {
                audit.record_private_admit(&self.caller, host, &self.grant_source);
            }
        }
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

    /// Find the manifest-declared [`FsReadScope`] matching an expanded absolute path, returning
    /// `(scope.path, scope.secret)`. Deny-by-default: `None` if no scope matches (the `fs.read`
    /// handler refuses the read). The scope's `path` is home-expanded before matching, so manifest
    /// authors write `~/.aws/sso/cache/**`.
    fn fs_scope_for(&self, abs_path: &str) -> Option<(&String, bool)> {
        self.grants
            .fs
            .iter()
            .find(|s| fs_path_matches(&expand_home(&s.path), abs_path))
            .map(|s| (&s.path, s.secret))
    }

    fn private_net_allow(&self) -> PrivateNetAllow {
        let declared = normalize_patterns(&self.grants.private_hosts);
        let grants = normalize_patterns(&self.private_net_grants);
        if declared.is_empty() || grants.is_empty() {
            return PrivateNetAllow::None;
        }
        if grants.iter().any(|g| g == "*") {
            return PrivateNetAllow::from_hosts(declared);
        }
        if declared.iter().any(|d| d == "*") {
            return PrivateNetAllow::from_hosts(grants);
        }
        PrivateNetAllow::from_hosts(
            grants
                .into_iter()
                .filter(|grant| host_matches(&declared, grant))
                .collect::<Vec<_>>(),
        )
    }

    fn ensure_http_host_allowed(&self, url: &url::Url) -> std::result::Result<(), String> {
        let host = url
            .host_str()
            .ok_or_else(|| "http.do: url has no host".to_string())?;
        if host_matches(&self.grants.http_hosts, host) || self.endpoint_allows_host(host) {
            Ok(())
        } else {
            Err(format!(
                "http.do: host `{host}` not in this plugin's declared HTTP capabilities"
            ))
        }
    }

    fn endpoint_allows_host(&self, host: &str) -> bool {
        self.endpoints.iter().any(|ep| {
            host_matches(&ep.http_hosts, host)
                || ep.env.iter().any(|key| {
                    self.system
                        .env(key)
                        .and_then(|raw| url::Url::parse(&raw).ok())
                        .and_then(|url| url.host_str().map(|h| h.eq_ignore_ascii_case(host)))
                        .unwrap_or(false)
                })
        })
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
            "process.spawn" => {
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
                    return Err("process.spawn: `argv` (non-empty array) required".into());
                }
                // Same deny-by-default gate as `process.run`: only allow-listed `argv[0]` programs.
                if !self.grants.process.iter().any(|p| p == &argv[0]) {
                    return Err(format!(
                        "process.spawn: program `{}` not in this plugin's granted capabilities",
                        argv[0]
                    ));
                }
                // Optional caller env overrides (applied on top of the cleared+allow-listed env by
                // `spawn_background`); only string values are taken.
                let env: Vec<(String, String)> = payload
                    .get("env")
                    .and_then(|v| v.as_object())
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();
                let child = self
                    .system
                    .spawn_background(&argv, &env)
                    .map_err(|e| e.to_string())?;
                let id = self
                    .next_proc
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.procs.lock().await.insert(id, child);
                Ok(json!({ "proc_id": id }))
            }
            "process.read" => {
                let id = payload
                    .get("proc_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("process.read: `proc_id` required")?;
                let mut guard = self.procs.lock().await;
                let child = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("process.read: no managed process {id}"))?;
                let (stdout, stderr) = child.read_output();
                let st = child.status();
                let mut out = json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "running": st.running,
                });
                if let Some(code) = st.exit_code {
                    out["exit_code"] = json!(code);
                }
                Ok(out)
            }
            "process.status" => {
                let id = payload
                    .get("proc_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("process.status: `proc_id` required")?;
                let mut guard = self.procs.lock().await;
                let child = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("process.status: no managed process {id}"))?;
                let st = child.status();
                let mut out = json!({ "running": st.running });
                if let Some(code) = st.exit_code {
                    out["exit_code"] = json!(code);
                }
                Ok(out)
            }
            "process.kill" => {
                let id = payload
                    .get("proc_id")
                    .and_then(|v| v.as_u64())
                    .ok_or("process.kill: `proc_id` required")?;
                if let Some(mut child) = self.procs.lock().await.remove(&id) {
                    child.kill();
                }
                Ok(json!({ "ok": true }))
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
            "credential" => {
                // The in-band-auth path for raw-socket protocols (e.g. Postgres SCRAM needs the
                // password value). DENY-BY-DEFAULT: only available if the plugin's manifest granted
                // `credential`. The materialized value is delivered to the (trusted) plugin binary,
                // registered with the redactor so it never leaks into model-visible output, and is
                // NEVER returned through any discovery/endpoint path — only this explicit capability.
                if !self.grants.credential {
                    return Err("credential not granted to this plugin".into());
                }
                let resolver = self
                    .resolver
                    .as_ref()
                    .ok_or("credential requires a reference resolver (none installed)")?;
                // Either a direct `credential_ref` (string or object), or an `endpoint_ref` whose
                // record carries a `credential_ref` to materialize.
                let reference = if let Some(cr) = payload.get("credential_ref") {
                    parse_credential_ref(cr)?
                } else if let Some(endpoint_ref) =
                    payload.get("endpoint_ref").and_then(|v| v.as_str())
                {
                    resolver.credential_ref_for_endpoint(endpoint_ref).await?
                } else {
                    return Err(
                        "credential: `credential_ref` or `endpoint_ref` required".to_string()
                    );
                };
                let material = resolver
                    .resolve_credential_for(&self.consumer, &reference)
                    .await?;
                // Register the value with the redactor (if a sink is installed) so it is scrubbed
                // from any captured/model-visible output even though the trusted plugin receives it.
                if let Some(sink) = &self.secret_sink {
                    sink.register_secret(&material.value);
                }
                Ok(json!({ "value": material.value }))
            }
            "fs.read" => {
                // Path-scoped HOST-file read (C-09a). For the `aws-bedrock` plugin to read
                // `~/.aws/config` + `~/.aws/sso/cache` (the SSO refresh-token cache) without an
                // `aws` CLI. These are HOST paths OUTSIDE the workspace jail (which `System::read_file`
                // cannot reach), so the capability has its own manifest-declared scope: the host reads
                // ONLY paths matching a declared [`FsReadScope`], denies anything out of scope, rejects
                // `..` traversal, caps the size, and registers `secret: true` reads with the
                // [`Redactor`](flux_secret::Redactor) so refresh tokens never leak into model-visible
                // output. Deny-by-default: an empty `fs` grant refuses every read.
                let raw_path = payload
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or("fs.read: `path` (string) required")?;
                let expanded = expand_home(raw_path);
                // Reject `..` traversal before matching (defense-in-depth; the scope match is the
                // primary gate, but a naive glob join could otherwise reach outside the scope dir).
                if path_has_traversal(&expanded) {
                    return Err(format!(
                        "fs.read: path `{raw_path}` contains a `..` traversal; denied"
                    ));
                }
                let (scope, secret) = match self.fs_scope_for(&expanded) {
                    Some(s) => (s.0.clone(), s.1),
                    None => {
                        return Err(format!(
                            "fs.read: path `{raw_path}` not in this plugin's fs.read scope"
                        ))
                    }
                };
                let bytes = match tokio::fs::read(&expanded).await {
                    Ok(b) => b,
                    Err(e) => return Err(format!("fs.read: {raw_path}: {e} (scope: {scope})")),
                };
                let size = bytes.len();
                // Binary (NUL-bearing or invalid UTF-8) -> base64; else UTF-8 text. Same shape as
                // `http.do`'s body/body_b64 split, and byte-capped on a char boundary.
                let is_binary = bytes.contains(&0) || std::str::from_utf8(&bytes).is_err();
                if is_binary {
                    let capped = if size > 256 * 1024 {
                        bytes[..256 * 1024].to_vec()
                    } else {
                        bytes
                    };
                    let body_b64 = base64::engine::general_purpose::STANDARD.encode(&capped);
                    if secret {
                        if let Some(sink) = &self.secret_sink {
                            sink.register_secret(&String::from_utf8_lossy(&capped));
                        }
                    }
                    Ok(json!({ "path": raw_path, "size": size, "body_b64": body_b64 }))
                } else {
                    let text = String::from_utf8(bytes).expect("checked UTF-8 above");
                    let text = truncate_on_char_boundary(text, 256 * 1024);
                    if secret {
                        if let Some(sink) = &self.secret_sink {
                            sink.register_secret(&text);
                        }
                    }
                    Ok(json!({ "path": raw_path, "size": size, "body": text }))
                }
            }
            "http.do" => {
                if !self.grants.http {
                    return Err("http.do not granted to this plugin".into());
                }
                // Ref-based IO (D-27): when the plugin passes an `endpoint_ref`, the host resolves it
                // to an absolute URL + injected auth headers — the plugin (and the model) never see the
                // URL or the credential. The composed URL still runs through the SAME egress guard +
                // host allow-list as the legacy `url` path, so SSRF/private-net rules still apply.
                //
                // NAMED vs DISCOVERED split: a *discovered* `@endpoint/<id>` ref goes to the injected
                // resolver (the L5 broker, which owns the discovery registry + the cross-plugin gate). A
                // *named* manifest endpoint resolves LOCALLY here from the plugin's own `EndpointSpec`
                // env binding + the declared `auth_purpose` injection — so a static plugin needs NO host
                // config beyond "set the documented env var and go" and works with no resolver installed.
                let mut ref_injected: Vec<(String, String)> = Vec::new();
                let mut url = if let Some(endpoint_ref) =
                    payload.get("endpoint_ref").and_then(|v| v.as_str())
                {
                    let path = payload.get("path").and_then(|v| v.as_str());
                    let base =
                        if flux_secret::endpoint::EndpointRef::is_discovered_ref(endpoint_ref) {
                            let resolver = self.resolver.as_ref().ok_or(
                            "http.do: endpoint_ref requires a reference resolver (none installed)",
                        )?;
                            // Resolve on behalf of THIS plugin (the real consumer): if the endpoint's
                            // credential is owned by another plugin, host-injecting it is a cross-plugin use
                            // and the broker's deny-by-default gate fires against `self.consumer`.
                            let resolved = resolver
                                .resolve_endpoint_for(&self.consumer, endpoint_ref)
                                .await?;
                            ref_injected = resolved.injected_headers;
                            resolved.url
                        } else {
                            // Named manifest endpoint → resolve its base URL locally from the declared env.
                            self.resolve_endpoint(endpoint_ref)?
                        };
                    let composed = compose_url(&base, path)?;
                    let url = guard_http_url(&composed, &self.private_net_allow())?;
                    self.ensure_http_host_allowed(&url)?;
                    url
                } else {
                    let raw = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let url = guard_http_url(raw, &self.private_net_allow())?;
                    self.ensure_http_host_allowed(&url)?;
                    url
                };
                // The request is admitted. If the (now-allowed) host is private/internal, the scoped
                // grant just let through what the bare SSRF guard would refuse — audit it.
                if let Some(host) = url.host_str() {
                    self.audit_admit(host);
                }
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
                // Host-injected auth from the resolved endpoint (the `endpoint_ref` path): applied
                // host-side BEFORE the legacy `auth_purpose` injection, so a ref-resolved credential
                // reaches the wire without the plugin ever holding the value.
                for (name, value) in ref_injected {
                    req = req.header(name.as_str(), value);
                }
                match inject {
                    AuthInjection::None | AuthInjection::Query { .. } => {}
                    AuthInjection::Bearer(t) => req = req.bearer_auth(t),
                    AuthInjection::Basic { user, secret } => {
                        req = req.basic_auth(user, Some(secret))
                    }
                    AuthInjection::Header { name, value } => req = req.header(name.as_str(), value),
                }
                // Request body: a base64 `body_b64` (byte-exact upload) wins over the text `body`;
                // either one (never both) becomes the request body.
                if let Some(b64) = payload.get("body_b64").and_then(|v| v.as_str()) {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .map_err(|e| format!("http.do: bad body_b64: {e}"))?;
                    req = req.body(bytes);
                } else if let Some(body) = payload.get("body").and_then(|v| v.as_str()) {
                    req = req.body(body.to_string());
                }
                let resp = req.send().await.map_err(|e| e.to_string())?;
                let status = resp.status().as_u16();
                // Binary download path (`response_binary: true`): return the raw bytes base64-encoded,
                // capped (NOT char-truncated) so a byte-exact download survives. Default keeps the
                // text path unchanged.
                if payload
                    .get("response_binary")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    const MAX_BIN_BODY: usize = 16 * 1024 * 1024;
                    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
                    let capped = &bytes[..bytes.len().min(MAX_BIN_BODY)];
                    let body_b64 = base64::engine::general_purpose::STANDARD.encode(capped);
                    return Ok(json!({ "status": status, "body_b64": body_b64 }));
                }
                let body = resp.text().await.unwrap_or_default();
                let body = truncate_on_char_boundary(body, 256 * 1024);
                Ok(json!({ "status": status, "body": body }))
            }
            "conn.dial" => {
                // Ref-based dial (D-27): when the plugin passes an `endpoint_ref`, the host resolves
                // it and takes host:port from the resolved URL — the plugin passes the ref, not the
                // host:port. The resolved target still runs through the same `dial_scoped` guard +
                // grant check below.
                //
                // NAMED vs DISCOVERED split (mirrors `http.do`): a *discovered* `@endpoint/<id>` ref
                // resolves through the injected resolver; a *named* manifest endpoint resolves its
                // host:port LOCALLY from the plugin's own `EndpointSpec` env binding (no host config,
                // no resolver needed).
                let target = if let Some(endpoint_ref) =
                    payload.get("endpoint_ref").and_then(|v| v.as_str())
                {
                    let base = if flux_secret::endpoint::EndpointRef::is_discovered_ref(
                        endpoint_ref,
                    ) {
                        let resolver = self.resolver.as_ref().ok_or(
                            "conn.dial: endpoint_ref requires a reference resolver (none installed)",
                        )?;
                        // Resolve on behalf of THIS plugin (the real consumer) — same cross-plugin
                        // gating rationale as the `http.do` path (the resolved URL's host:port is what
                        // we dial; any cross-plugin credential on the record is gated against
                        // `self.consumer`).
                        let resolved = resolver
                            .resolve_endpoint_for(&self.consumer, endpoint_ref)
                            .await?;
                        resolved.url
                    } else {
                        // Named manifest endpoint → resolve its base URL locally from the declared env.
                        self.resolve_endpoint(endpoint_ref)?
                    };
                    dial_target_from_url(&base)?
                } else {
                    let kind = payload
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tcp");
                    match kind {
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
                    }
                };
                let tstr = conn_target_str(&target);
                if !conn_granted(&self.grants.conn, &tstr) {
                    return Err(format!(
                        "conn.dial: target `{tstr}` not in this plugin's granted conn capabilities"
                    ));
                }
                let stream = flux_system::net::dial_scoped(&target, &self.private_net_allow())
                    .await
                    .map_err(|e| e.to_string())?;
                // The dial was admitted. A TCP target that resolves private was let through by the
                // scoped grant (Unix sockets aren't IP egress) — audit it.
                if let flux_system::net::DialTarget::Tcp { host, .. } = &target {
                    self.audit_admit(host);
                }
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
                // Optional per-call read deadline (D-45: sql/asterisk `timeout` parity). When set,
                // `stream.read` is raced against the deadline; on elapsed the connection stays open
                // (the plugin decides to retry or close) and a `timed_out` flag is returned so the
                // plugin's wire-protocol loop can surface a timeout error rather than a silent hang.
                let timeout_ms = payload.get("timeout_ms").and_then(|v| v.as_u64());
                let mut guard = self.conns.lock().await;
                let stream = guard
                    .get_mut(&id)
                    .ok_or_else(|| format!("conn.read: no open connection {id}"))?;
                let read_fut = stream.read(max);
                let (data, timed_out) = match timeout_ms {
                    Some(ms) => {
                        let dur = std::time::Duration::from_millis(ms);
                        match tokio::time::timeout(dur, read_fut).await {
                            Ok(Ok(data)) => (data, false),
                            Ok(Err(e)) => return Err(format!("conn.read: {e}")),
                            Err(_) => (Vec::new(), true),
                        }
                    }
                    None => (read_fut.await.map_err(|e| e.to_string())?, false),
                };
                let eof = data.is_empty() && !timed_out;
                Ok(json!({
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(&data),
                    "eof": eof,
                    "timed_out": timed_out
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
fn guard_http_url(raw: &str, allow: &PrivateNetAllow) -> std::result::Result<url::Url, String> {
    flux_system::net::guard_url_scoped(raw, allow).map_err(|e| e.to_string())
}

/// Compose an absolute request URL from a resolved base and an optional plugin-supplied `path`.
/// The base is the host-resolved endpoint URL (already credential-free); `path` joins onto it
/// (relative-resolved against the base, so a base `…/v1/` + path `query` → `…/v1/query`). A `None`
/// or empty path returns the base unchanged.
fn compose_url(base: &str, path: Option<&str>) -> std::result::Result<String, String> {
    match path {
        None | Some("") => Ok(base.to_string()),
        Some(p) => {
            let base = url::Url::parse(base).map_err(|e| format!("http.do: bad base url: {e}"))?;
            let joined = base
                .join(p)
                .map_err(|e| format!("http.do: bad path `{p}`: {e}"))?;
            Ok(joined.to_string())
        }
    }
}

/// Build a TCP [`DialTarget`](flux_system::net::DialTarget) from a resolved endpoint URL's host+port
/// (defaulting the port to the URL scheme's known default). For the ref-based `conn.dial` path.
fn dial_target_from_url(raw: &str) -> std::result::Result<flux_system::net::DialTarget, String> {
    let url = url::Url::parse(raw).map_err(|e| format!("conn.dial: bad endpoint url: {e}"))?;
    let host = url
        .host_str()
        .ok_or("conn.dial: resolved endpoint url has no host")?
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or("conn.dial: resolved endpoint url has no port (and scheme has no default)")?;
    Ok(flux_system::net::DialTarget::Tcp { host, port })
}

/// Parse a credential reference from the `credential` capability payload: either a `Ref`-shaped
/// object (`{scheme, plugin, instance, slot}`) or a `scheme/...` string.
fn parse_credential_ref(v: &Value) -> std::result::Result<flux_secret::Ref, String> {
    match v {
        Value::String(s) => flux_secret::Ref::parse(s),
        Value::Object(_) => serde_json::from_value(v.clone())
            .map_err(|e| format!("credential: bad credential_ref object: {e}")),
        _ => Err("credential: `credential_ref` must be a string or object".to_string()),
    }
}

fn normalize_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

// --- C-09a `fs.read` path helpers -------------------------------------------------------------

/// Expand a leading `~` to `$HOME` (matching `Workspace::resolve`). `~` alone or `~/...` expands;
/// `~user/...` is left as-is.
fn expand_home(input: &str) -> String {
    if let Some(rest) = input.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            let home = std::env::var("HOME").unwrap_or_default();
            return format!("{home}{rest}");
        }
    }
    input.to_string()
}

/// Whether a path contains a `..` path component (traversal). Rejects `..` as any segment, not
/// just a leading one, so `a/../b` and `/x/..` both trip it — defense-in-depth before the scope
/// match (a naive glob join could otherwise reach outside the scope dir).
fn path_has_traversal(path: &str) -> bool {
    std::path::Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Match an expanded absolute path against a `/**` / `/*` / exact glob. `/**` matches the dir
/// itself + everything under it (incl. nested subdirs); `/*` matches direct children only; an
/// exact path matches itself. Trailing-slash-insensitive.
fn fs_path_matches(pattern: &str, abs_path: &str) -> bool {
    let pat = pattern.trim_end_matches('/');
    let p = abs_path.trim_end_matches('/');
    if pat == p {
        return true;
    }
    if let Some(dir) = pat.strip_suffix("/**") {
        let dir = dir.trim_end_matches('/');
        p == dir || p.starts_with(&format!("{dir}/"))
    } else if let Some(dir) = pat.strip_suffix("/*") {
        let dir = dir.trim_end_matches('/');
        if !p.starts_with(&format!("{dir}/")) || p.len() <= dir.len() + 1 {
            return false;
        }
        let rest = &p[dir.len() + 1..];
        !rest.contains('/')
    } else {
        false
    }
}

fn host_matches(patterns: &[String], host: &str) -> bool {
    let host = host
        .trim()
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase();
    patterns.iter().any(|pattern| {
        let p = pattern
            .trim()
            .trim_matches('[')
            .trim_matches(']')
            .to_ascii_lowercase();
        p == "*"
            || p == host
            || p.strip_prefix("*.").is_some_and(|suffix| {
                host.ends_with(suffix)
                    && host.len() > suffix.len()
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            })
    })
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
    /// Spawn a plugin binary through flux's **single guarded process path**
    /// ([`flux_system::System::spawn_interactive`]): the plugin runs argv-only, in the workspace root,
    /// with a **cleared environment** (the minimal non-secret allow-list only) — so it cannot read the
    /// host's secrets directly and must request them back through the gated host capabilities. The
    /// framed `flux.plugin.v1` protocol then runs over the piped stdin/stdout.
    pub async fn spawn(
        system: &flux_system::System,
        program: &str,
        args: &[String],
    ) -> Result<Self> {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(program.to_string());
        argv.extend_from_slice(args);
        let flux_system::InteractiveChild {
            child,
            stdin,
            stdout,
        } = system
            .spawn_interactive(&argv)
            .map_err(|e| Error::Other(format!("spawn plugin {program}: {e}")))?;
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
        // The model-facing tool name is the operation's fully-qualified name. flux plugin ops are
        // authored already qualified (e.g. `slack.message.send`), and the plugin's own dispatch,
        // `flux plugin call`, and the generated skill docs all use that name — so adopt it verbatim when
        // it is already prefixed, and only add the `{plugin}.` prefix for an un-qualified op name.
        // (Unconditionally prefixing double-qualified the common case to `slack.slack.message.send`, so
        // an agent's `tools` grant — `slack.message.send` — never matched and every plugin op was
        // silently dropped from the agent surface.)
        let qualified = if op.name == plugin || op.name.starts_with(&format!("{plugin}.")) {
            op.name.clone()
        } else {
            format!("{plugin}.{}", op.name)
        };
        let spec = ToolSpec {
            name: qualified,
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

/// A loaded plugin: its projected tools plus the live handles the L5 endpoint broker (D-26) needs to
/// fan out a discovery query back to it. Returned by [`load_plugin_tools`].
///
/// The broker keeps the `manifest` (to match its `discovers` products), the shared `host` (to issue
/// an `endpoint.discover` op call), and the `caps` (the same guarded host-capability set the plugin's
/// tools were built with, so a broker-driven call is gated identically).
pub struct LoadedPlugin {
    /// Each plugin operation projected as an agent [`Tool`]; registering them keeps the host alive.
    pub tools: Vec<Arc<dyn Tool>>,
    /// The shared subprocess connection every tool (and the broker) drives behind a mutex.
    pub host: Arc<tokio::sync::Mutex<PluginHost>>,
    /// The plugin's fetched manifest (carries `discovers` + the requested capabilities).
    pub manifest: PluginManifest,
    /// The guarded host capabilities the plugin's ops run under (the output of `make_caps`).
    pub caps: Arc<dyn HostCapabilities>,
}

/// Spawn a plugin, fetch its manifest, and project every operation as a [`PluginTool`] sharing one
/// host connection. Returns a [`LoadedPlugin`] (the tools plus the shared host/manifest/caps the
/// endpoint broker fans out to) — keep the host handle alive for the session.
///
/// `make_caps` builds the host capabilities *from the fetched manifest*, so the caps can be scoped
/// to exactly what the plugin declared (see [`SystemHostCaps::with_grants`]) — the binding point
/// where a plugin's requested privileges are pinned to its manifest.
pub async fn load_plugin_tools(
    system: &flux_system::System,
    program: &str,
    args: &[String],
    make_caps: impl FnOnce(&PluginManifest) -> Arc<dyn HostCapabilities>,
) -> Result<LoadedPlugin> {
    let mut host = PluginHost::spawn(system, program, args).await?;
    let manifest = host.manifest().await?;
    let caps = make_caps(&manifest);
    let host = Arc::new(tokio::sync::Mutex::new(host));
    // Project only the non-`internal` ops as agent tools (C-09a). A host-only op (`internal: true`,
    // e.g. the aws-bedrock plugin's `auth` op returning raw AWS keys) stays dispatchable by the host
    // via the shared `PluginHost` handle, but is NOT advertised to the LLM — the model must never
    // call it, or the keys would appear in the tool result. The filter is a single free function
    // ([`visible_ops`]) so the projection rule is unit-testable without spawning a subprocess.
    let tools: Vec<Arc<dyn Tool>> = visible_ops(&manifest)
        .map(|op| {
            Arc::new(PluginTool::new(
                host.clone(),
                caps.clone(),
                &manifest.name,
                op,
            )) as Arc<dyn Tool>
        })
        .collect();
    Ok(LoadedPlugin {
        tools,
        host,
        manifest,
        caps,
    })
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

/// The path a plugin descriptor for `name` would live at, after rejecting names that could
/// escape `dir` when joined. `Path::join` treats `..` and absolute components literally, so an
/// unsanitized name like `../../config` or `/etc/passwd` would resolve outside the plugins
/// directory — a destructive traversal for `remove_descriptor`. The single guard here covers
/// every caller (`add`/`load`/`set_pinned`/`remove`): a valid plugin name is a bare file name with
/// no path separators, no `..`/`.` component, and no absolute/prefix component.
fn descriptor_path(dir: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    invalid_plugin_name(name)?;
    Ok(dir.join(format!("{name}.toml")))
}

/// Reject a plugin name that is not a bare file name. Empty, a path separator (`/` / `\`),
/// `..`, `.`, or an absolute / Windows-prefix component all fail — `Path::join` would otherwise
/// carry them literally out of the plugins directory.
fn invalid_plugin_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(Error::Other(format!(
            "invalid plugin name `{name}`: must be a bare file name (no path separators, `..`, or absolute components)"
        )));
    }
    use std::path::Component;
    for comp in std::path::Path::new(name).components() {
        if !matches!(comp, Component::Normal(_)) {
            return Err(Error::Other(format!(
                "invalid plugin name `{name}`: must be a bare file name (no path separators, `..`, or absolute components)"
            )));
        }
    }
    Ok(())
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
    std::fs::write(descriptor_path(dir, name)?, body).map_err(Error::Io)?;
    Ok(())
}

/// Load a single named descriptor, if present.
pub fn load_descriptor(dir: &std::path::Path, name: &str) -> Result<Option<PluginDescriptor>> {
    match std::fs::read_to_string(descriptor_path(dir, name)?) {
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

/// Remove a plugin descriptor (`flux plugin uninstall`); returns whether a descriptor existed
/// (a missing name is `Ok(false)` — a clean "nothing to uninstall", not an error). Other IO
/// failures (permissions, etc.) propagate as `Err`.
pub fn remove_descriptor(dir: &std::path::Path, name: &str) -> Result<bool> {
    match std::fs::remove_file(descriptor_path(dir, name)?) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_descriptor_deletes_file_and_reports_missing_as_false() {
        let dir = std::env::temp_dir().join(format!("flux-rm-desc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No descriptor yet → `remove_descriptor` reports `false`, not an error.
        assert!(!remove_descriptor(&dir, "ghost").unwrap());

        // Add one, then remove → reports `true`, and discovery no longer lists it.
        add_descriptor(
            &dir,
            "p",
            &PluginDescriptor {
                program: "/bin/true".into(),
                args: vec![],
                pinned: None,
            },
        )
        .unwrap();
        assert!(remove_descriptor(&dir, "p").unwrap());
        assert!(
            discover(&dir).is_empty(),
            "the descriptor is gone after uninstall"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A plugin name with `..`, a path separator, or an absolute component must be rejected before
    /// any filesystem op — `remove_descriptor` is a destructive `remove_file`, so an escaped name
    /// would delete a file outside the plugins dir (D-35). One guard in `descriptor_path` covers
    /// `add` / `load` / `set_pinned` / `remove`.
    #[test]
    fn descriptor_path_rejects_traversal_names() {
        let dir = std::env::temp_dir().join(format!("flux-desc-traversal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A sentinel file *outside* `dir` (a sibling, reachable via `..`). Every traversal name
        // below would, if joined literally, point at or past it. The guard must refuse before any
        // filesystem op touches it.
        let outside = dir.parent().unwrap().join("flux-desc-traversal-sentinel");
        std::fs::write(&outside, b"keep me").unwrap();

        let desc = PluginDescriptor {
            program: "/bin/true".into(),
            args: vec![],
            pinned: None,
        };
        let bad_names = [
            "../sentinel",
            "../../flux-desc-traversal-sentinel",
            "/etc/passwd",
            "a/b",
            "..",
            ".",
            "",
        ];
        for name in bad_names {
            assert!(
                remove_descriptor(&dir, name).is_err(),
                "remove_descriptor(`{name}`) must be rejected"
            );
            assert!(
                add_descriptor(&dir, name, &desc).is_err(),
                "add_descriptor(`{name}`) must be rejected"
            );
            assert!(
                load_descriptor(&dir, name).is_err(),
                "load_descriptor(`{name}`) must be rejected"
            );
            assert!(
                set_pinned(&dir, name, None).is_err(),
                "set_pinned(`{name}`) must be rejected"
            );
        }

        // The sentinel outside `dir` is untouched — no traversal reached it.
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "keep me",
            "no traversal name reached a file outside the plugins dir"
        );
        // And nothing was written *inside* `dir` either.
        assert!(
            discover(&dir).is_empty(),
            "no descriptor was created for a rejected name"
        );

        // Legitimate names still work (the guard must not over-reach).
        add_descriptor(&dir, "my-plugin_v2.0", &desc).unwrap();
        assert!(load_descriptor(&dir, "my-plugin_v2.0").unwrap().is_some());
        assert!(remove_descriptor(&dir, "my-plugin_v2.0").unwrap());

        std::fs::remove_file(&outside).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

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

    // --- C-09a piece 1: the `internal`/host-only op flag ------------------------------------------
    // An op marked `internal: true` is NOT advertised to the LLM as a callable tool — it is a
    // host-only channel (the aws-bedrock plugin's `auth` op returning raw AWS keys is the canonical
    // case: the model must never call it, or the keys would appear in the tool result). The op stays
    // dispatchable by the host (via the shared `PluginHost` handle, like the broker calls
    // `endpoint.discover`); only the *projection* as an agent tool is suppressed.

    #[test]
    fn internal_op_is_not_projected_as_a_tool() {
        // Failing-first for C-09a piece 1: before the `internal` flag existed every manifest op
        // became an LLM-callable tool, so an `auth` op returning raw keys would be model-callable.
        let manifest = PluginManifest {
            name: "aws-bedrock".into(),
            operations: vec![
                OperationSpec {
                    name: "aws-bedrock.chat".into(),
                    description: "run a bedrock turn".into(),
                    ..Default::default()
                },
                OperationSpec {
                    name: "aws-bedrock.auth".into(),
                    description: "resolve AWS creds (host-only)".into(),
                    internal: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let visible: Vec<&str> = visible_ops(&manifest).map(|o| o.name.as_str()).collect();
        assert_eq!(
            visible,
            vec!["aws-bedrock.chat"],
            "only the non-internal op projects"
        );
        assert!(
            !visible.contains(&"aws-bedrock.auth"),
            "the internal `auth` op must NOT be advertised to the LLM"
        );
    }

    #[test]
    fn internal_flag_defaults_false_so_existing_plugins_unchanged() {
        // Backwards compat: a manifest that says nothing about `internal` (every existing plugin)
        // projects all its ops — the flag is opt-in, not a behavior change for current manifests.
        let op = serde_json::from_value::<OperationSpec>(serde_json::json!({
            "name": "kubernetes.pod.list",
            "description": "list pods"
        }))
        .unwrap();
        assert!(!op.internal);
    }

    // --- C-09a piece 2: the path-scoped deny-by-default `fs.read` capability ----------------------
    // For the aws-bedrock plugin to read `~/.aws/config` + `~/.aws/sso/cache` (the SSO refresh-token
    // cache) without an `aws` CLI. These are HOST paths outside the workspace jail, so they can't go
    // through `System::read_file`; the capability has its own manifest-declared scope, denies anything
    // out of scope, rejects `..` traversal, and registers secret-bearing reads with the Redactor.

    #[tokio::test]
    async fn fs_read_denies_out_of_scope_paths() {
        let dir = std::env::temp_dir().join(format!("flux-fs-deny-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("aws/config");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"[default]").unwrap();
        let outside = dir.join("secret.txt");
        std::fs::write(&outside, b"TOPSECRET").unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/aws/config", dir.display()),
                secret: false,
            }],
            ..Default::default()
        });

        // In-scope: allowed.
        assert!(
            caps.handle(
                "fs.read",
                &serde_json::json!({"path": target.to_str().unwrap()})
            )
            .await
            .is_ok(),
            "in-scope fs.read must be allowed"
        );
        // Out-of-scope: denied (deny-by-default — not a silent empty read).
        let err = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": outside.to_str().unwrap()}),
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("not in this plugin's fs.read scope"),
            "out-of-scope read must be denied with a clear error, got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fs_read_recursive_glob_matches_nested_files() {
        let dir = std::env::temp_dir().join(format!("flux-fs-glob-{}", std::process::id()));
        let cache = dir.join("aws/sso/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let token_file = cache.join("abc.json");
        std::fs::write(&token_file, b"{\"refreshToken\":\"rt\"}").unwrap();
        let nested = cache.join("sub/deep.json");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, b"{}").unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                // `/**` matches the dir itself + everything under it (incl. nested subdirs).
                path: format!("{}/aws/sso/cache/**", dir.display()),
                secret: true,
            }],
            ..Default::default()
        });

        let got = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": token_file.to_str().unwrap()}),
            )
            .await
            .unwrap();
        assert_eq!(got["body"], "{\"refreshToken\":\"rt\"}");
        // A nested file under the cache dir also matches.
        assert!(
            caps.handle(
                "fs.read",
                &serde_json::json!({"path": nested.to_str().unwrap()})
            )
            .await
            .is_ok(),
            "`/**` must match nested files"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fs_read_secret_scope_registers_content_with_redactor() {
        // A `secret: true` scope's content is registered with the SecretSink (the executor's
        // Redactor) so that if it ever flows into model-visible output it is scrubbed. This is the
        // `~/.aws/sso/cache` refresh-token privilege boundary.
        let dir = std::env::temp_dir().join(format!("flux-fs-secret-{}", std::process::id()));
        let cache = dir.join("aws/sso/cache");
        std::fs::create_dir_all(&cache).unwrap();
        let token_file = cache.join("token.json");
        // The full file content is what `fs.read` registers with the Redactor (the capability
        // registers what it READ; the consumer — the aws-bedrock plugin in C-09b — registers the
        // specific secrets it EXTRACTS). So the redaction guarantee is: if the raw file content is
        // ever echoed into model-visible output, it is scrubbed.
        let file_content = "{\"accessToken\":\"super-secret-refresh-token-xyz\"}";
        std::fs::write(&token_file, file_content).unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let redactor = flux_secret::Redactor::new();
        let sink = Arc::new(RedactorSink {
            redactor: redactor.clone(),
        });
        let caps = SystemHostCaps::new(sys)
            .with_grants(PluginCapabilities {
                fs: vec![FsReadScope {
                    path: format!("{}/aws/sso/cache/**", dir.display()),
                    secret: true,
                }],
                ..Default::default()
            })
            .with_secret_sink(sink);

        let _ = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": token_file.to_str().unwrap()}),
            )
            .await
            .unwrap();

        // The refresh-token value the host just read must be registered with the Redactor — so a
        // later capture that echoes it back is scrubbed, not leaked.
        let leaked = format!("the cache file reads: {file_content}");
        let scrubbed = redactor.redact(&leaked);
        assert_ne!(
            &scrubbed, &leaked,
            "secret fs.read content must be redactor-registered"
        );
        assert!(!scrubbed.contains(file_content));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn fs_read_rejects_path_traversal_even_when_pattern_could_match() {
        let dir = std::env::temp_dir().join(format!("flux-fs-trav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let outside = dir.parent().unwrap().join("flux-fs-trav-sentinel");
        std::fs::write(&outside, b"keep me").unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        // A wildcard scope that, naively joined, could reach outside via `..`.
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/aws/**", dir.display()),
                secret: false,
            }],
            ..Default::default()
        });

        let traversal = format!("{}/aws/../../flux-fs-trav-sentinel", dir.display());
        let err = caps
            .handle("fs.read", &serde_json::json!({"path": &traversal}))
            .await
            .unwrap_err();
        assert!(
            err.contains("traversal") || err.contains("not in this plugin's fs.read scope"),
            "`..` traversal must be rejected, got: {err}"
        );
        // The sentinel outside the scope is untouched.
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep me");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_file(&outside).ok();
    }

    #[tokio::test]
    async fn fs_read_returns_binary_as_base64() {
        let dir = std::env::temp_dir().join(format!("flux-fs-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin_file = dir.join("aws/blob");
        std::fs::create_dir_all(bin_file.parent().unwrap()).unwrap();
        std::fs::write(&bin_file, [0u8, 255, 0, 128]).unwrap();

        let sys = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            fs: vec![FsReadScope {
                path: format!("{}/aws/**", dir.display()),
                secret: false,
            }],
            ..Default::default()
        });

        let got = caps
            .handle(
                "fs.read",
                &serde_json::json!({"path": bin_file.to_str().unwrap()}),
            )
            .await
            .unwrap();
        // Binary (NUL-bearing) content comes back base64-encoded, not as a UTF-8-mangled `body`.
        assert!(
            got.get("body_b64").is_some(),
            "binary read must return body_b64"
        );
        assert!(got.get("body").is_none());
        assert_eq!(got["size"], 4);
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
                http_hosts: vec!["gl.example.com".into()],
                description: String::new(),
            }],
            capabilities: PluginCapabilities {
                secrets: vec!["FLUX_TEST_API_TOKEN_XZ".into()],
                http: true,
                http_hosts: vec!["gl.example.com".into()],
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
    async fn http_do_denies_undeclared_hosts_before_network() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-http-host-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            http: true,
            http_hosts: vec!["api.example.com".into()],
            ..Default::default()
        });

        let err = caps
            .handle("http.do", &json!({"url": "https://evil.example.com/"}))
            .await
            .unwrap_err();
        assert!(err.contains("not in this plugin's declared HTTP capabilities"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn endpoint_env_hosts_are_http_allow_listed() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-http-endpoint-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        std::env::set_var(
            "FLUX_TEST_ENDPOINT_HOST_XZ",
            "https://selfhosted.example.com/base",
        );
        let caps = SystemHostCaps::new(sys)
            .with_grants(PluginCapabilities {
                http: true,
                ..Default::default()
            })
            .with_manifest(&PluginManifest {
                endpoints: vec![EndpointSpec {
                    name: "service.endpoint".into(),
                    env: vec!["FLUX_TEST_ENDPOINT_HOST_XZ".into()],
                    ..Default::default()
                }],
                capabilities: PluginCapabilities {
                    http: true,
                    ..Default::default()
                },
                ..Default::default()
            });
        let url = url::Url::parse("https://selfhosted.example.com/path").unwrap();

        assert!(caps.ensure_http_host_allowed(&url).is_ok());

        std::env::remove_var("FLUX_TEST_ENDPOINT_HOST_XZ");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn private_net_requires_manifest_declaration_and_operator_grant() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-private-host-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let loopback = "http://127.0.0.1:8123/";

        let operator_only =
            SystemHostCaps::new(sys.clone()).with_private_net_grants(vec!["127.0.0.1".into()]);
        assert!(guard_http_url(loopback, &operator_only.private_net_allow()).is_err());

        let manifest_only = SystemHostCaps::new(sys.clone()).with_grants(PluginCapabilities {
            private_hosts: vec!["127.0.0.1".into()],
            ..Default::default()
        });
        assert!(guard_http_url(loopback, &manifest_only.private_net_allow()).is_err());

        let both = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        assert!(guard_http_url(loopback, &both.private_net_allow()).is_ok());

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
        let none =
            SystemHostCaps::new(sys.clone()).with_private_net_grants(vec!["127.0.0.1".into()]);
        assert!(none.handle("conn.dial", &dial).await.is_err());

        // Granted (loopback wildcard) → dial/write/read/close round-trips.
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                conn: vec!["tcp:127.0.0.1:*".into()],
                private_hosts: vec!["127.0.0.1".into()],
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
    async fn conn_read_timeout_returns_timed_out_without_closing() {
        // D-45: a `conn.read` with `timeout_ms` that elapses before data arrives returns
        // `timed_out: true` (and an empty body) while leaving the connection open — the plugin
        // can retry or close. A server that accepts but never writes exercises the deadline path.
        use flux_system::{System, Workspace};
        use tokio::io::AsyncReadExt;
        let dir = std::env::temp_dir().join(format!("flux-conn-timeout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A loopback server that accepts but never writes (so the client's read blocks until the
        // deadline). It holds the socket open for the whole test so the read doesn't get a clean
        // EOF — only the timeout fires.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept_task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Keep the socket open (no write) so the client's read blocks; drain until closed.
            let mut buf = [0u8; 64];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                conn: vec!["tcp:127.0.0.1:*".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let dial = json!({"kind": "tcp", "host": "127.0.0.1", "port": port});
        let id = caps.handle("conn.dial", &dial).await.unwrap()["conn_id"]
            .as_u64()
            .unwrap();

        // A 10ms deadline on a read against a server that never writes → timed_out, empty body.
        let read = caps
            .handle(
                "conn.read",
                &json!({"conn_id": id, "max": 64, "timeout_ms": 10}),
            )
            .await
            .unwrap();
        assert_eq!(read["timed_out"], true, "the read should time out: {read}");
        let body = base64::engine::general_purpose::STANDARD
            .decode(read["data_b64"].as_str().unwrap())
            .unwrap();
        assert!(body.is_empty(), "no data should arrive before the deadline");
        // The connection stays open (not closed by the timeout): a write still succeeds.
        let ping = base64::engine::general_purpose::STANDARD.encode(b"ping");
        let wrote = caps
            .handle("conn.write", &json!({"conn_id": id, "data_b64": ping}))
            .await
            .unwrap();
        assert_eq!(
            wrote["written"], 4,
            "the connection is still usable after a timeout"
        );

        caps.handle("conn.close", &json!({"conn_id": id}))
            .await
            .unwrap();
        accept_task.await.unwrap();
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

    #[tokio::test]
    async fn process_spawn_denies_ungranted_program() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-procdeny-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        // A fresh caps grants no programs → process.spawn is denied.
        let none = SystemHostCaps::new(sys.clone());
        assert!(
            none.handle("process.spawn", &json!({"argv": ["echo", "hi"]}))
                .await
                .is_err(),
            "ungranted process.spawn must be denied"
        );
        // Granting only `printf` still denies `sleep` (same allow-list as process.run).
        let limited = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            process: vec!["printf".into()],
            ..Default::default()
        });
        assert!(
            limited
                .handle("process.spawn", &json!({"argv": ["sleep", "30"]}))
                .await
                .is_err(),
            "a non-granted program must be denied on process.spawn"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn process_spawn_read_status_kill_lifecycle() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-proclife-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            process: vec!["sleep".into()],
            ..Default::default()
        });

        // spawn a long-lived child
        let spawned = caps
            .handle("process.spawn", &json!({"argv": ["sleep", "30"]}))
            .await
            .unwrap();
        let id = spawned["proc_id"].as_u64().unwrap();

        // read + status both report it running (and no exit_code yet)
        let read = caps
            .handle("process.read", &json!({"proc_id": id}))
            .await
            .unwrap();
        assert_eq!(read["running"], true);
        assert!(read.get("exit_code").is_none());
        let st = caps
            .handle("process.status", &json!({"proc_id": id}))
            .await
            .unwrap();
        assert_eq!(st["running"], true);

        // kill removes it from the registry
        let killed = caps
            .handle("process.kill", &json!({"proc_id": id}))
            .await
            .unwrap();
        assert_eq!(killed["ok"], true);
        assert!(
            caps.handle("process.status", &json!({"proc_id": id}))
                .await
                .is_err(),
            "a killed process is no longer in the registry"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn process_read_captures_output_and_exit_code() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-procout-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let caps = SystemHostCaps::new(sys).with_grants(PluginCapabilities {
            process: vec!["printf".into()],
            ..Default::default()
        });
        let id = caps
            .handle("process.spawn", &json!({"argv": ["printf", "out-bg"]}))
            .await
            .unwrap()["proc_id"]
            .as_u64()
            .unwrap();

        // Poll read (it drains) accumulating stdout. The drain task copies the pipe asynchronously,
        // so the final bytes can arrive a tick *after* the child is observed exited — keep reading
        // until the expected output shows up, not just until exit.
        let mut combined = String::new();
        let mut exit_code: Option<i64> = None;
        let mut saw_exit = false;
        for _ in 0..200 {
            let r = caps
                .handle("process.read", &json!({"proc_id": id}))
                .await
                .unwrap();
            combined.push_str(r["stdout"].as_str().unwrap());
            if r["running"] == false {
                saw_exit = true;
                exit_code = r.get("exit_code").and_then(|v| v.as_i64());
            }
            if saw_exit && combined.contains("out-bg") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(saw_exit, "child should have exited");
        assert_eq!(exit_code, Some(0));
        assert_eq!(combined, "out-bg");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A throwaway HTTP/1.1 server that echoes each request body back as the response body. Lets the
    /// `http.do` binary-body paths round-trip without a network dependency. Returns the bound port.
    async fn spawn_echo_http_server() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 2048];
                    // Read until we have the full header block, then parse Content-Length.
                    let (headers_end, content_length) = loop {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header_str = String::from_utf8_lossy(&buf[..pos]);
                            let cl = header_str
                                .lines()
                                .find_map(|l| {
                                    let lower = l.to_ascii_lowercase();
                                    lower
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            break (pos + 4, cl);
                        }
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break (buf.len(), 0),
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    };
                    // Read the remaining body bytes.
                    while buf.len() < headers_end + content_length {
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let end = (headers_end + content_length).min(buf.len());
                    let body = buf[headers_end..end].to_vec();
                    let mut resp =
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                            .into_bytes();
                    resp.extend_from_slice(&body);
                    let _ = sock.write_all(&resp).await;
                    let _ = sock.flush().await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn http_body_b64_round_trips_and_response_binary_is_byte_exact() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-httpbin-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        // http granted + loopback declared and operator-granted so the test server is reachable.
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            });
        let port = spawn_echo_http_server().await;
        let url = format!("http://127.0.0.1:{port}/echo");

        // Raw, non-UTF-8 bytes: body_b64 upload + response_binary download must round-trip exactly.
        let raw: Vec<u8> = vec![0u8, 159, 146, 150, 255, 0, 1, 2];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let resp = caps
            .handle(
                "http.do",
                &json!({
                    "method": "POST",
                    "url": url,
                    "body_b64": b64,
                    "response_binary": true,
                }),
            )
            .await
            .unwrap();
        assert_eq!(resp["status"], 200);
        assert!(
            resp.get("body").is_none(),
            "binary response must not carry a text body"
        );
        let got = base64::engine::general_purpose::STANDARD
            .decode(resp["body_b64"].as_str().unwrap())
            .unwrap();
        assert_eq!(got, raw, "binary body must be byte-exact");

        // Default (no response_binary): body_b64 still uploads, response comes back as text.
        let text_b64 = base64::engine::general_purpose::STANDARD.encode(b"hello-text");
        let resp2 = caps
            .handle(
                "http.do",
                &json!({"method": "POST", "url": url, "body_b64": text_b64}),
            )
            .await
            .unwrap();
        assert_eq!(resp2["status"], 200);
        assert_eq!(resp2["body"], "hello-text");
        assert!(resp2.get("body_b64").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A test [`EgressAudit`] double that records every admit as `(caller, host, grant_source)`.
    #[derive(Default)]
    struct RecordingAudit {
        admits: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl EgressAudit for RecordingAudit {
        fn record_private_admit(&self, caller: &str, host: &str, grant_source: &str) {
            self.admits.lock().unwrap().push((
                caller.to_string(),
                host.to_string(),
                grant_source.to_string(),
            ));
        }
    }

    #[tokio::test]
    async fn egress_audit_fires_on_private_admit_only() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-audit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let audit = Arc::new(RecordingAudit::default());

        // A loopback echo server so a *private* host can actually be reached (admitted).
        let port = spawn_echo_http_server().await;

        // Manifest names the plugin (→ caller + grant_source), grants http + a loopback private host
        // (declared + operator-granted), plus a *public* host allow so a public request isn't blocked.
        let manifest = PluginManifest {
            name: "auditplug".into(),
            capabilities: PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into(), "example.com".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_egress_audit(audit.clone());

        // Admitting a PRIVATE host fires the audit with the plugin name + plugin grant_source.
        caps.handle(
            "http.do",
            &json!({"url": format!("http://127.0.0.1:{port}/echo")}),
        )
        .await
        .unwrap();
        {
            let admits = audit.admits.lock().unwrap();
            assert_eq!(
                admits.len(),
                1,
                "private admit must record exactly one event"
            );
            assert_eq!(admits[0].0, "auditplug");
            assert_eq!(admits[0].1, "127.0.0.1");
            assert_eq!(admits[0].2, "config:plugin/auditplug");
        }

        // A PUBLIC host does NOT fire the audit (the request fails at connect/DNS, but the host is
        // allow-listed so it passes the host gate and reaches the audit check — which must not fire).
        let _ = caps
            .handle("http.do", &json!({"url": "http://example.com/"}))
            .await;
        assert_eq!(
            audit.admits.lock().unwrap().len(),
            1,
            "a public host must not record a private-admit event"
        );

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

    /// A mock [`ReferenceResolver`] for the ref-based IO tests: a fixed endpoint resolution (URL +
    /// one injected header) and a fixed credential materialization, recording whether each was
    /// consulted.
    struct MockResolver {
        endpoint_url: String,
        inject: (String, String),
        credential_value: String,
        endpoint_consulted: std::sync::atomic::AtomicBool,
        credential_consulted: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl ReferenceResolver for MockResolver {
        async fn resolve_endpoint(
            &self,
            reference: &str,
        ) -> std::result::Result<flux_secret::endpoint::ResolvedEndpoint, String> {
            self.endpoint_consulted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(
                flux_secret::endpoint::ResolvedEndpoint::new(reference, &self.endpoint_url)
                    .with_header(&self.inject.0, &self.inject.1),
            )
        }

        async fn resolve_credential(
            &self,
            reference: &flux_secret::Ref,
        ) -> std::result::Result<flux_secret::Material, String> {
            self.credential_consulted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(flux_secret::Material {
                reference: reference.clone(),
                kind: flux_secret::Kind::ApiKey,
                value: self.credential_value.clone(),
                media_type: None,
            })
        }
    }

    /// A [`SecretSink`] double backed by a [`Redactor`](flux_secret::Redactor), so a test can assert a
    /// materialized credential is registered (and thus redacted from any captured output).
    struct RedactorSink {
        redactor: flux_secret::Redactor,
    }

    impl SecretSink for RedactorSink {
        fn register_secret(&self, value: &str) {
            self.redactor.add_secret(value);
        }
    }

    /// A throwaway HTTP/1.1 server that echoes the request's `Authorization` header value back as the
    /// response body (or `none`). Lets a test prove the host-injected header reached the wire.
    async fn spawn_header_echo_server() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 2048];
                    loop {
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        }
                    }
                    let headers = String::from_utf8_lossy(&buf);
                    let auth = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("authorization:")
                                .map(|_| {
                                    l.split_once(':')
                                        .map(|(_, v)| v.trim().to_string())
                                        .unwrap_or_default()
                                })
                        })
                        .unwrap_or_else(|| "none".to_string());
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                        auth.len(),
                        auth
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn http_by_ref_injects_host_side() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-httpref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let port = spawn_header_echo_server().await;

        // The resolver returns the loopback test server's URL + an Authorization: Bearer header. The
        // plugin passes only an `endpoint_ref` — never the URL or the token.
        let token = "sk-super-secret-ref-token";
        let resolver = Arc::new(MockResolver {
            endpoint_url: format!("http://127.0.0.1:{port}/"),
            inject: ("Authorization".into(), format!("Bearer {token}")),
            credential_value: String::new(),
            endpoint_consulted: std::sync::atomic::AtomicBool::new(false),
            credential_consulted: std::sync::atomic::AtomicBool::new(false),
        });
        let caps = SystemHostCaps::new(sys)
            .with_private_net_grants(vec!["127.0.0.1".into()])
            .with_grants(PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            })
            .with_resolver(resolver.clone());

        let result = caps
            .handle(
                "http.do",
                &json!({ "endpoint_ref": "@endpoint/svc-1", "path": "v1/ping" }),
            )
            .await
            .unwrap();

        // The resolver was consulted, and the outbound request carried the host-injected header.
        assert!(resolver
            .endpoint_consulted
            .load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(result["status"], 200);
        assert_eq!(
            result["body"],
            format!("Bearer {token}"),
            "the server saw the host-injected Authorization header"
        );
        // The frame the plugin gets back contains neither the resolved URL nor… the token would have
        // been in `body` only because our echo server reflects it; in production the plugin gets only
        // the real response. Assert the *frame fields* never carry the URL or the credential ref.
        let frame = result.to_string();
        assert!(
            !frame.contains("127.0.0.1"),
            "frame must not carry the URL: {frame}"
        );
        assert!(
            result.get("url").is_none() && result.get("endpoint_ref").is_none(),
            "frame must not echo the URL/ref back to the plugin"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn http_by_named_ref_resolves_from_manifest() {
        use flux_system::{System, Workspace};
        let dir =
            std::env::temp_dir().join(format!("flux-httpnamedref-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let port = spawn_header_echo_server().await;

        // A NAMED manifest endpoint bound to the loopback test server via env, plus a declared
        // Bearer auth method. The plugin passes only the named `endpoint_ref` + `path`; the host
        // resolves the base URL locally from the manifest (no resolver installed), composes the URL,
        // and injects the declared `auth_purpose` host-side. A resolver is deliberately NOT installed
        // to prove a named ref resolves entirely from the manifest.
        std::env::set_var(
            "FLUX_TEST_NAMEDREF_URL",
            format!("http://127.0.0.1:{port}/"),
        );
        std::env::set_var("FLUX_TEST_NAMEDREF_TOK", "named-bear-tok");
        let manifest = PluginManifest {
            name: "svc".into(),
            auth: vec![AuthMethod::bearer(
                "api_token",
                vec!["FLUX_TEST_NAMEDREF_TOK".into()],
            )],
            endpoints: vec![EndpointSpec {
                name: "svc.endpoint".into(),
                env: vec!["FLUX_TEST_NAMEDREF_URL".into()],
                http_hosts: vec!["127.0.0.1".into()],
                description: String::new(),
            }],
            capabilities: PluginCapabilities {
                http: true,
                http_hosts: vec!["127.0.0.1".into()],
                private_hosts: vec!["127.0.0.1".into()],
                secrets: vec!["FLUX_TEST_NAMEDREF_TOK".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let caps = SystemHostCaps::new(sys)
            .with_manifest(&manifest)
            .with_private_net_grants(vec!["127.0.0.1".into()]);

        let result = caps
            .handle(
                "http.do",
                &json!({
                    "endpoint_ref": "svc.endpoint",
                    "path": "/api/x",
                    "auth_purpose": "api_token",
                }),
            )
            .await
            .unwrap();

        // The host composed `{base}/api/x` and injected the declared Bearer token; the echo server
        // reflects the Authorization header it saw.
        assert_eq!(result["status"], 200);
        assert_eq!(
            result["body"], "Bearer named-bear-tok",
            "the host injected the declared auth_purpose for a named ref"
        );
        // The frame carries neither the resolved URL nor the token — only the ref + path went in.
        let frame = result.to_string();
        assert!(
            !frame.contains("127.0.0.1") && !frame.contains("FLUX_TEST_NAMEDREF"),
            "frame must not carry the URL/env: {frame}"
        );

        std::env::remove_var("FLUX_TEST_NAMEDREF_URL");
        std::env::remove_var("FLUX_TEST_NAMEDREF_TOK");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn raw_socket_credential_gated_to_plugin_not_model() {
        use flux_system::{System, Workspace};
        let dir = std::env::temp_dir().join(format!("flux-credgate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sys = Arc::new(System::new(Workspace::new(&dir).unwrap()));

        let secret = "pg-scram-password-value";
        let resolver = Arc::new(MockResolver {
            endpoint_url: "postgres://db.internal:5432/app".into(),
            inject: (String::new(), String::new()),
            credential_value: secret.into(),
            endpoint_consulted: std::sync::atomic::AtomicBool::new(false),
            credential_consulted: std::sync::atomic::AtomicBool::new(false),
        });
        let redactor = flux_secret::Redactor::new();
        let sink = Arc::new(RedactorSink {
            redactor: redactor.clone(),
        });
        let cred_payload = json!({ "credential_ref": "kubernetes/monitoring/pg-creds/password" });

        // WITHOUT the `credential` grant → refused (deny-by-default).
        let ungranted = SystemHostCaps::new(sys.clone())
            .with_resolver(resolver.clone())
            .with_secret_sink(sink.clone());
        let err = ungranted
            .handle("credential", &cred_payload)
            .await
            .unwrap_err();
        assert!(
            err.contains("not granted"),
            "ungranted credential must be refused: {err}"
        );
        assert!(
            !resolver
                .credential_consulted
                .load(std::sync::atomic::Ordering::SeqCst),
            "the resolver must not even be consulted without the grant"
        );

        // WITH the grant → the (trusted) plugin receives the materialized value.
        let granted = SystemHostCaps::new(sys)
            .with_grants(PluginCapabilities {
                credential: true,
                ..Default::default()
            })
            .with_resolver(resolver.clone())
            .with_secret_sink(sink.clone());
        let got = granted.handle("credential", &cred_payload).await.unwrap();
        assert_eq!(
            got["value"], secret,
            "the trusted plugin receives the value"
        );
        assert!(resolver
            .credential_consulted
            .load(std::sync::atomic::Ordering::SeqCst));

        // The value is registered with the redactor → it would be scrubbed from any captured output.
        assert_eq!(
            redactor.redact(&format!("connecting with {secret} now")),
            "connecting with [redacted] now",
            "the materialized credential is redacted from model-visible output"
        );
        std::fs::remove_dir_all(&dir).ok();
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

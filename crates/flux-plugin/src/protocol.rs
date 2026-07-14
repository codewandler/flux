//! Wire contracts and the synchronous guest-side stdio SDK.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use flux_evidence::{SignalMatch, ToolGroup, KIND_TURN_INTENT};
use flux_lang::ast::FlowEffect;
use flux_spec::{Effect, Idempotency, Risk, StagingDisposition};

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
    /// Optional model-facing tool name. The subprocess dispatch identity remains [`Self::name`],
    /// which lets a plugin preserve its stable CLI/wire operation while exposing a compatibility
    /// name in the agent catalog (for example `websearch.search` as `web.search`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_name: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
    /// JSON Schema for a successful operation result. Optional for wire compatibility with
    /// existing plugins; schema-complete catalogs may require it for their own visible ops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// IO effects this operation may produce (drives the policy floor + approval).
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Declared risk; `None` → `Risk::Medium`.
    #[serde(default)]
    pub risk: Option<Risk>,
    /// Declared idempotency; `None` → `Idempotency::NonIdempotent`.
    #[serde(default)]
    pub idempotency: Option<Idempotency>,
    /// Whether the adaptive loop should gather this operation during exploration or capture it in
    /// the later action batch. `gather` remains subject to the ordinary risk/effect/intent checks;
    /// it can never grant execution authority.
    #[serde(default)]
    pub staging: StagingDisposition,
    /// Secret purposes (auth-method names) this op needs the host to resolve (e.g. `"api_token"`).
    #[serde(default)]
    pub secret_purposes: Vec<String>,
    /// Optional evidence/catalog group this operation belongs to. Plugin-authored groups are declared
    /// on the manifest and merged into the runtime group list when the plugin loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The op's declared SEMANTIC effects (`money`, `delete`, `send_external`, …) — the
    /// `flux_lang::ast::FlowEffect` tag vocabulary — carried alongside `effects` above instead of
    /// being erased the way lowering a Flux-Lang `OpSpec` to a host [`ToolSpec`] necessarily erases
    /// them (D-138). [`PluginTool`] projects these onto its [`Tool::semantic_effects`] hook, and
    /// `flux-flow`'s catalog adapter folds them into the op's `OpSignature` and, from there, into
    /// `annotate_effects`'s per-call annotation — with no authored `effect:` tag required at the
    /// call site. Empty means "no declared semantic tier beyond `effects`," matching every existing
    /// manifest that says nothing about it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub semantic_effects: Vec<FlowEffect>,
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
    /// **Secret-like fields** (GL-031 / D-93): property NAMES whose values the host must MASK
    /// wherever it echoes this op's input or result — the `flux plugin call` dry-run input preview,
    /// the live result echo, the stringified [`Tool::execute`](PluginTool) result the model sees,
    /// and audit. Declarative: an op marks which of its fields carry secrets (e.g. a CI/pipeline
    /// variable `value`) and the host applies [`redact_secret_fields`] uniformly — the plugin never
    /// redacts by hand, and secret values never reach terminal scrollback, logs, or saved
    /// transcripts. Matched by property name at *any depth*, so a flat `value` field and an array of
    /// `{key, value}` variable objects are both masked. Empty (the default) means "this op echoes
    /// nothing secret," matching every existing manifest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redact_fields: Vec<String>,
}

impl OperationSpec {
    /// Return the model-facing tool identity for this operation.
    ///
    /// Explicit public names are already fully-qualified product identities and are returned
    /// verbatim. Otherwise the legacy plugin-name qualification rule remains unchanged.
    pub fn projected_name(&self, plugin: &str) -> String {
        if let Some(name) = &self.public_name {
            return name.clone();
        }
        if self.name == plugin || self.name.starts_with(&format!("{plugin}.")) {
            self.name.clone()
        } else {
            format!("{plugin}.{}", self.name)
        }
    }
}

/// The marker a secret-like field's value is replaced with when the host echoes it (GL-031).
pub const REDACTED_MARKER: &str = "***";

/// Mask secret-like fields in a JSON value for host-side echo/audit (GL-031 / D-93). Every object
/// property whose key matches one of `fields` (by name, at any depth) has its value replaced with
/// [`REDACTED_MARKER`], recursing through nested objects and arrays. This is the single masking
/// applied to a plugin op's declared [`OperationSpec::redact_fields`] wherever secret-carrying
/// input or output is printed: `flux plugin call`'s dry-run input preview and live result echo, and
/// [`PluginTool::execute`]'s stringified result. Matching by key name (not a fixed JSON pointer)
/// means an array of `{key, value}` variable objects is masked element-wise. A no-op when `fields`
/// is empty (the common case), so it is safe to call unconditionally.
pub fn redact_secret_fields(value: &mut Value, fields: &[String]) {
    if fields.is_empty() {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if fields.iter().any(|f| f == key) {
                    *val = Value::String(REDACTED_MARKER.to_string());
                } else {
                    redact_secret_fields(val, fields);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                redact_secret_fields(item, fields);
            }
        }
        _ => {}
    }
}

/// The manifest-declared, deny-by-default operations that are projected as agent tools: every
/// op whose [`OperationSpec::internal`] flag is `false`. Host-only (`internal: true`) ops are
/// excluded — they are still dispatchable by the host via the shared `PluginHost` handle, just
/// not advertised to the model. This is the single filter [`load_plugin_tools`] applies.
pub fn visible_ops(manifest: &PluginManifest) -> impl Iterator<Item = &OperationSpec> {
    manifest.operations.iter().filter(|op| !op.internal)
}

/// Validate the operation identity portion of a plugin manifest before any handler is projected.
///
/// A plugin may spell an operation as either `get` or `{plugin}.get`; both project to the same
/// public tool name. Reject both exact duplicate manifest entries and those alias collisions so
/// the catalog can never disagree with the subprocess handler selected by the raw operation name.
pub fn validate_manifest_operations(manifest: &PluginManifest) -> std::result::Result<(), String> {
    let mut raw_names = std::collections::BTreeSet::new();
    let mut public_names = std::collections::BTreeMap::new();
    for op in &manifest.operations {
        if op.name.trim().is_empty() {
            return Err(format!(
                "plugin `{}` manifest contains a blank operation name",
                manifest.name
            ));
        }
        if !raw_names.insert(op.name.clone()) {
            return Err(format!(
                "plugin `{}` manifest contains duplicate operation `{}`",
                manifest.name, op.name
            ));
        }

        let public_name = op.projected_name(&manifest.name);
        if public_name.trim().is_empty() {
            return Err(format!(
                "plugin `{}` operation `{}` has a blank public name",
                manifest.name, op.name
            ));
        }
        if let Some(previous) = public_names.insert(public_name.clone(), op.name.clone()) {
            return Err(format!(
                "plugin `{}` operations `{previous}` and `{}` both project as `{public_name}`",
                manifest.name, op.name
            ));
        }
    }
    Ok(())
}

/// The reserved internal preflight op a plugin may serve (D-88): `{operation, input}` →
/// `{operation, valid, problems, warnings}`. host-kit's `PluginBuilder::build` auto-registers it,
/// and its verdict is the SAME check the plugin's runtime dispatch enforces — so a host's
/// `--dry-run` path that merges this verdict can never disagree with a live call. Hosts
/// feature-detect it by presence in the manifest (older plugins simply don't serve it).
pub const VALIDATE_OP: &str = "plugin.validate";

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

/// A token grant an [`OAuth2Spec`] method allows the host to run on the plugin's behalf
/// (plugin-oauth, D-80). The plugin performs none of these itself — it only declares which are
/// supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthGrant {
    /// Browser + loopback-callback PKCE (`flux auth login <plugin>`).
    AuthorizationCode,
    /// Resource-owner password grant (`flux auth login <plugin> --password`).
    Password,
    /// Refresh an expired access token from a stored refresh token.
    RefreshToken,
    /// Two-legged client-credentials grant (no user).
    ClientCredentials,
}

/// The loopback redirect a plugin's `authorization_code` login binds — a local `127.0.0.1:{port}{path}`
/// listener the browser is redirected to with the auth code. Local-only: never an outbound host, so it
/// is outside the plugin egress allow-list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthRedirect {
    /// The loopback port to bind (e.g. `1456`).
    pub port: u16,
    /// The callback path the browser is redirected to (e.g. `/auth/callback`).
    pub path: String,
}

/// Declares that an [`AuthMethod`]'s purpose is **OAuth2-backed** (plugin-oauth, D-80): the host runs
/// every token grant (login + refresh) and injects only a fresh bearer, so the plugin performs no
/// OAuth itself. `authorize_path`/`token_path` are joined onto the auth method's declared `endpoint`
/// base URL, so the token host stays host-declared and egress-gated (never a plugin-supplied URL).
/// Every field deserializes with a default and `AuthMethod.oauth2` is `None` for a plain env→secret
/// method, so legacy manifests round-trip unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OAuth2Spec {
    /// The declared [`EndpointSpec`] name whose base URL `authorize_path`/`token_path` resolve
    /// against — its host allow-list is what admits the token exchange through the egress gate.
    #[serde(default)]
    pub endpoint: String,
    /// The authorize endpoint path (the browser redirect target for `authorization_code`), joined
    /// onto the endpoint base URL.
    #[serde(default)]
    pub authorize_path: String,
    /// The token endpoint path (every grant + refresh POSTs here), joined onto the endpoint base URL.
    #[serde(default)]
    pub token_path: String,
    /// The OAuth2 client id.
    #[serde(default)]
    pub client_id: String,
    /// Requested scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The grants the host may run for this method.
    #[serde(default)]
    pub grants: Vec<OAuthGrant>,
    /// The loopback redirect for the `authorization_code` login flow (required for that grant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect: Option<OAuthRedirect>,
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
    /// When set, this purpose is OAuth2-backed (plugin-oauth, D-80): the host runs the token grants
    /// (login/refresh) and injects the resulting Bearer access token. `None` for a plain env→secret
    /// method — a method with no `oauth2` block resolves exactly as before. When both `oauth2` and
    /// `env` are set, `env` is the fallback used until a login stores tokens (D-81).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth2: Option<OAuth2Spec>,
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

    /// An OAuth2 method (plugin-oauth, D-80): the host runs the grants and injects a Bearer access
    /// token. `env` may still be supplied as a pre-login fallback (D-81).
    pub fn oauth2(purpose: impl Into<String>, spec: OAuth2Spec) -> Self {
        Self {
            purpose: purpose.into(),
            scheme: AuthScheme::Bearer,
            oauth2: Some(spec),
            ..Self::default()
        }
    }
}

/// A configurable API endpoint (base URL) the host resolves by name — the binding a plugin
/// addresses **by reference** on the ref-based IO paths (`http.do`/`conn.dial` with an
/// `endpoint_ref`). Resolution is host-side only: there is no capability that hands the resolved
/// URL string back to the plugin (the `endpoint` URL-handback was retired in D-32).
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
    /// A **default** base URL used when no declared env key resolves (D-32) — host-side, so a
    /// plugin with a well-known public default (e.g. `https://gitlab.com`) works with zero config
    /// while the URL still never crosses to the plugin. A set env key always wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// A host-side URL **template** composed from declared non-secret config values (D-32): each
    /// `{name}` placeholder substitutes the manifest `config` entry `name`'s env-resolved value
    /// (percent-encoded). When set, the endpoint resolves from the template and `env` is unused —
    /// how a *dynamic* base like the Atlassian gateway
    /// (`https://api.atlassian.com/ex/jira/{cloud_id}`) stays host-composed: the plugin addresses
    /// it by name and the composed URL never crosses to the plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// A declared **non-secret** configuration value the plugin may read through the gated `config`
/// host capability (D-32) — e.g. jira's Atlassian `cloud_id`, resolved from env keys in order.
/// Deny-by-default like every capability: only declared names resolve, and a declared env key
/// that is secret-classified (a granted `secrets` entry or an auth method's secret env) is
/// refused — `config` can never return a secret value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigSpec {
    /// The config name the plugin references (e.g. `"cloud_id"`).
    pub name: String,
    /// Env-var keys holding the value, tried in order.
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
    /// Plugin-authored operation groups. These travel with the manifest so a plugin can organize its
    /// projected tools without requiring each workspace to define `.flux/groups.toml` entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<ToolGroup>,
    /// Configurable API endpoints (base URLs) the host resolves from env.
    #[serde(default)]
    pub endpoints: Vec<EndpointSpec>,
    /// Declared **non-secret** config values the plugin may read via the gated `config` host
    /// capability (D-32). Also the substitution source for [`EndpointSpec::template`] placeholders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<ConfigSpec>,
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

/// Bound on consecutive host frames that fail to parse before [`serve_io`] gives up (D-54). A lone
/// malformed frame is tolerated (skip + diagnostic) — frames arrive only from the trusted parent
/// host, so a single bad one is transient noise. This many *in a row* means the framing itself is
/// broken (host bug or stream corruption): further reads won't self-heal, and silently spinning
/// would strand the host awaiting responses to request ids that will never come. Exiting lets the
/// host's own hard error ("plugin closed the connection") surface instead of an indefinite hang.
pub(crate) const MAX_CONSECUTIVE_MALFORMED_FRAMES: u32 = 5;

/// Run the plugin: read request frames from stdin, dispatch, write response frames to stdout.
/// Operation calls may issue host-capability callbacks via the provided [`GuestHost`]. Blocks
/// until stdin closes. Call this from a plugin binary's `main`.
pub fn serve(handler: impl PluginHandler) {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve_io(stdin.lock(), stdout.lock(), std::io::stderr(), handler);
}

/// The testable core of [`serve`]: reader, writer, and diagnostic sink are injected so tests can
/// drive the loop in-process without spawning a binary. Behavior for well-formed traffic is
/// unchanged from the direct-stdio loop.
///
/// A line that fails to parse as a [`Frame`] is skipped (never treated as fatal on its own): one
/// diagnostic line is written to `diag` — stdout stays the protocol channel, so diagnostics never go
/// there — naming only the line's byte length and the parse error, never the raw content (well-formed
/// frames can carry secrets, and there is no way to tell a merely-truncated well-formed frame from
/// genuine garbage). [`MAX_CONSECUTIVE_MALFORMED_FRAMES`] malformed frames *in a row* end the loop
/// (with a final diagnostic); any frame that parses successfully resets the counter.
pub(crate) fn serve_io<R: std::io::BufRead, W: std::io::Write, D: std::io::Write>(
    mut reader: R,
    mut writer: W,
    mut diag: D,
    handler: impl PluginHandler,
) {
    let mut line = String::new();
    let mut consecutive_malformed: u32 = 0;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or read error
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req = match serde_json::from_str::<Frame>(trimmed) {
            Ok(req) => {
                consecutive_malformed = 0;
                req
            }
            Err(e) => {
                consecutive_malformed += 1;
                let _ = writeln!(
                    diag,
                    "flux-plugin: dropped malformed frame from host ({} bytes; parse error: {e}) \
                     [{consecutive_malformed}/{MAX_CONSECUTIVE_MALFORMED_FRAMES} consecutive]",
                    trimmed.len(),
                );
                if consecutive_malformed >= MAX_CONSECUTIVE_MALFORMED_FRAMES {
                    let _ = writeln!(
                        diag,
                        "flux-plugin: {MAX_CONSECUTIVE_MALFORMED_FRAMES} consecutive malformed \
                         frames from host — exiting serve loop"
                    );
                    break;
                }
                continue;
            }
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

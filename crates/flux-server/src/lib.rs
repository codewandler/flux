//! `flux-server` — a long-running HTTP API around a [`FlowEngine`], so a flux agent can be driven
//! headlessly or remotely. It backs the `a2a` channel (`flux app run`) and is mounted by
//! [`router`] onto a chosen agent's engine; the standalone [`serve`] form is reused by the CLI's
//! `flux app run --serve <addr>` (no-program) path.
//!
//! Routes:
//! - `GET  /health`                       → `ok`
//! - `GET  /.well-known/agent-card.json`  → A2A agent card (discovery; `…/agent.json` is an alias)
//! - `POST /a2a`                          → A2A JSON-RPC 2.0 (`message/send`, `message/stream`)
//! - `POST /sessions`                     → `{ id, model }`
//! - `GET  /sessions/{id}`                 → session info
//! - `POST /sessions/{id}/messages`        → `{ text, tool_calls, usage }`
//!
//! The agent runs tools through the same safety envelope as the CLI; build it with auto-approve
//! since HTTP requests have no interactive approver.

mod a2a;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, FromRef, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use futures::Stream;
use serde_json::{json, Value};
use tower_http::timeout::TimeoutLayer;

use flux_auth::request::{AuthContext, AuthError, RequestAuthenticator};
use flux_core::Usage;
use flux_flow::engine::FlowEngine;
use flux_flow::AgentSink;
use flux_runtime::TurnIdentity;

type Shared = Arc<FlowEngine>;
/// Higher-level A2A task/session ordering. `FlowEngine` independently serializes turns and owns
/// their lexical caller identity; this gate exists only for mint/TTL/registry transition ordering.
pub(crate) type TurnGate = Arc<tokio::sync::Mutex<()>>;

// ── Auth modes (D-69) ─────────────────────────────────────────────────────────

/// How the server authenticates requests — three explicit modes.
#[derive(Clone)]
pub enum ServerAuth {
    /// No authentication. This mode is **guaranteed loopback-only by construction**: [`router`]
    /// (and [`router_multi`]) refuse to build an `Open` router for a non-loopback bind, so every
    /// serving path — including a lower-level caller that mounts the router into its own
    /// `axum::serve` — inherits the refusal rather than being trusted to re-derive it. The
    /// auto-approving daemon behind an open listener is remote code execution off loopback; there
    /// is no escape hatch (front it with an authenticating proxy instead).
    Open,
    /// One static shared secret for the whole deployment (the pre-D-69 mode): every request
    /// presents `Authorization: Bearer <secret>`, compared in constant time. There is no
    /// principal — the whole server is one auth realm. `external_url`, when set, is the base the
    /// public agent card advertises instead of the request `Host` header (a Host-poisoned card
    /// fetch would otherwise phish the shared secret to an attacker host — set it for any
    /// non-loopback bind).
    SharedSecret {
        secret: String,
        external_url: Option<String>,
    },
    /// Per-request bearer → principal resolution: every request is authenticated by the injected
    /// [`RequestAuthenticator`], sessions are tagged with and scoped to the caller's realm, and
    /// every turn runs under the request principal's `(Caller, Trust)` — never the service
    /// identity (see [`server_turn_context`]).
    Principal(PrincipalAuth),
}

impl ServerAuth {
    /// The pre-D-69 token knob mapped onto the explicit modes: `Some` → shared secret, `None` →
    /// open (loopback-only, enforced at router construction — see [`ServerAuth::Open`]). Kept for
    /// the surfaces whose config still speaks "optional token" (the CLI's `FLUX_SERVER_TOKEN`, the
    /// `a2a` channel adapter).
    pub fn from_token(token: Option<String>) -> Self {
        Self::shared_secret(token, None)
    }

    /// Shared-secret mode with an optional advertised base URL (see [`ServerAuth::SharedSecret`]);
    /// `None` token → [`ServerAuth::Open`].
    pub fn shared_secret(token: Option<String>, external_url: Option<String>) -> Self {
        match token {
            Some(secret) => ServerAuth::SharedSecret {
                secret,
                external_url,
            },
            None => ServerAuth::Open,
        }
    }

    /// The configured externally-reachable base the agent card should advertise (never the request
    /// `Host` header), when this mode has one. `None` → the card falls back to `Host` derivation,
    /// acceptable only for a loopback/dev bind.
    fn card_external_url(&self) -> Option<&str> {
        match self {
            ServerAuth::Open => None,
            ServerAuth::SharedSecret { external_url, .. } => external_url.as_deref(),
            ServerAuth::Principal(p) => Some(&p.external_url),
        }
    }
}

impl std::fmt::Debug for ServerAuth {
    /// Redacting: never renders the shared secret (or the authenticator's internals) — a `Debug`
    /// of the server config is exactly where an auth secret leaks into a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerAuth::Open => f.write_str("Open"),
            ServerAuth::SharedSecret { external_url, .. } => f
                .debug_struct("SharedSecret")
                .field("secret", &"<redacted>")
                .field("external_url", external_url)
                .finish(),
            ServerAuth::Principal(p) => f.debug_tuple("Principal").field(p).finish(),
        }
    }
}

/// The principal-auth mode's configuration. Constructing this requires the externally reachable
/// base URL up front: the agent card advertises where to send bearer tokens, so in this mode the
/// card's `url` must derive from deployment config — deriving it from the request's `Host` header
/// would let a Host-poisoned request on the (public, auth-exempt) card route redirect clients'
/// tokens to an attacker host.
#[derive(Clone)]
pub struct PrincipalAuth {
    pub(crate) authenticator: Arc<dyn RequestAuthenticator>,
    pub(crate) external_url: String,
}

impl std::fmt::Debug for PrincipalAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalAuth")
            .field("external_url", &self.external_url)
            .finish_non_exhaustive()
    }
}

impl PrincipalAuth {
    /// `external_url` is the externally reachable base (e.g. `https://agents.example.com`); the
    /// card advertises `<external_url>/a2a`.
    pub fn new(
        authenticator: Arc<dyn RequestAuthenticator>,
        external_url: impl Into<String>,
    ) -> Self {
        Self {
            authenticator,
            external_url: external_url.into(),
        }
    }

    /// Build principal-mode auth from RFC 7662 introspection parameters — the ONE construction
    /// point for the introspection authenticator, shared by every surface (the CLI's `--serve`
    /// and the `a2a` channel adapter) so the security-critical claim mapping and client wiring
    /// never diverge. Wraps the `Introspector` in the caching decorator. Requires the `introspect`
    /// feature.
    #[cfg(feature = "introspect")]
    pub fn from_introspection(params: IntrospectionParams) -> Result<Self, String> {
        use flux_auth::introspect::{CachedAuthenticator, IntrospectionConfig, Introspector};
        // Fail-open footgun: a tenancy deployment that maps an account claim but leaves
        // `require_account` off silently admits account-less tokens into per-principal (`user:`)
        // realms alongside its `acct:` realms. Warn loudly — realm namespaces are disjoint so this
        // is not a leak, but it is almost never intended.
        if params.account_claim.is_some() && !params.require_account {
            eprintln!(
                "(warning: [server] introspect_account_claim is set but require_account is false — \
                 tokens lacking the account claim will authenticate into per-principal realms; set \
                 introspect_require_account=true to reject them)"
            );
        }
        let mut ic = IntrospectionConfig::new(params.endpoint);
        ic.client = params.client;
        ic.allow_http = params.allow_http;
        ic.account_claim = params.account_claim;
        ic.roles_claim = params.roles_claim;
        ic.require_account = params.require_account;
        let introspector = Introspector::new(ic).map_err(|e| e.to_string())?;
        Ok(PrincipalAuth::new(
            Arc::new(CachedAuthenticator::new(introspector)),
            params.external_url,
        ))
    }
}

/// Parameters for [`PrincipalAuth::from_introspection`] — the surface-agnostic inputs each caller
/// gathers from its own config source (flux-config for the CLI, program settings for the channel
/// adapter) before handing them to the one construction point. `client` carries the ALREADY-
/// RESOLVED `(client_id, client_secret)`, never a secret literal read from a committed config file.
#[cfg(feature = "introspect")]
pub struct IntrospectionParams {
    pub endpoint: String,
    pub client: Option<(String, String)>,
    pub allow_http: bool,
    pub account_claim: Option<String>,
    pub roles_claim: Option<String>,
    pub require_account: bool,
    pub external_url: String,
}

/// Constant wire bodies — auth failures never carry backend detail (an introspection error's text
/// can leak internal endpoint topology, and interpolating into a header would be CRLF injection).
/// The `Unavailable` payload is logged server-side only.
pub(crate) const UNAUTHORIZED_BODY: &str = "unauthorized";
const UNAVAILABLE_BODY: &str = "authentication backend unavailable";
/// One constant 404 shape for the realm guard: a session that does not exist and a session owned
/// by another realm are byte-identical to the caller (A2A §13.1 — never reveal existence).
const NOT_FOUND_BODY: &str = "not found";

/// 401 with the byte-constant RFC 6750 challenge (identical across all causes — no oracle).
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            flux_auth::request::WWW_AUTHENTICATE,
        )],
        UNAUTHORIZED_BODY,
    )
        .into_response()
}

/// 503 with a constant body — the auth backend failing must fail closed, distinguishably from 401.
fn auth_unavailable() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE_BODY).into_response()
}

/// The realm guard's constant 404 (see [`NOT_FOUND_BODY`]).
pub(crate) fn realm_not_found() -> Response {
    (StatusCode::NOT_FOUND, NOT_FOUND_BODY).into_response()
}

/// The caller's realm: the tenancy key every principal-mode session is tagged with and scoped by.
/// Deliberately NON-optional — a principal without an account claim gets a principal-derived realm
/// rather than joining a shared "no account" pool (`None == None` would let all account-less
/// callers read and continue each other's sessions).
///
/// The two sources live in **disjoint namespaces** (`acct:` / `user:`) so an account-claim value
/// can never collide with the principal-derived form: without the prefix, an attacker whose IdP
/// emits `account = "user:victim"` would land in the same realm as an account-less principal
/// `victim` and read/continue their sessions. This is the same reserved-prefix discipline the
/// claim mapping applies to `account:` mirror groups, extended to the realm key itself.
fn realm_of(ctx: &AuthContext) -> String {
    match &ctx.account {
        Some(account) => format!("acct:{account}"),
        None => format!("user:{}", ctx.caller.principal.id),
    }
}

/// Immutable request-owned inputs for one server-driven engine turn.
#[derive(Clone)]
pub(crate) struct ServerTurnContext {
    pub(crate) realm: Option<String>,
    pub(crate) identity: Option<TurnIdentity>,
}

/// Resolve realm and caller identity together without mutating the shared executor. The resulting
/// identity is moved into `FlowEngine` only after its engine-owned turn gate is acquired.
pub(crate) fn server_turn_context(
    auth: &ServerAuth,
    ctx: Option<&AuthContext>,
) -> Result<ServerTurnContext, Box<Response>> {
    match auth {
        ServerAuth::Open | ServerAuth::SharedSecret { .. } => Ok(ServerTurnContext {
            realm: None,
            identity: None,
        }),
        ServerAuth::Principal(_) => {
            let ctx = ctx.ok_or_else(|| Box::new(unauthorized()))?;
            Ok(ServerTurnContext {
                realm: Some(realm_of(ctx)),
                identity: Some(TurnIdentity::new(ctx.caller.clone(), ctx.trust.clone())),
            })
        }
    }
}

/// Drive a server request through the engine's mandatory turn gate with the request's immutable
/// identity. Open/shared-secret modes retain the executor's assembled default identity.
pub(crate) async fn run_server_turn(
    engine: &FlowEngine,
    turn: &ServerTurnContext,
    session_id: &str,
    input: &str,
    sink: &mut dyn AgentSink,
    cancel: &tokio_util::sync::CancellationToken,
) -> flux_core::Result<()> {
    match &turn.identity {
        Some(identity) => {
            engine
                .run_turn_cancellable_as(session_id, input, sink, cancel, identity.clone())
                .await
        }
        None => {
            engine
                .run_turn_cancellable(session_id, input, sink, cancel)
                .await
        }
    }
}

/// Resolve only the caller realm for A2A operations that do not run an engine turn.
pub(crate) fn caller_realm(
    auth: &ServerAuth,
    ctx: Option<&AuthContext>,
) -> Result<Option<String>, ()> {
    server_turn_context(auth, ctx)
        .map(|turn| turn.realm)
        .map_err(|_| ())
}

/// The A2A session TTL in seconds (C-18): A2A-minted sessions whose last activity is older than
/// this are swept lazily before the next A2A session is created. `0` disables pruning.
#[derive(Clone, Copy, Debug)]
pub(crate) struct A2aTtl(pub(crate) u64);

/// Discovery metadata for the served agent — what the A2A agent card advertises. The `/a2a` URL is
/// not stored here; it is derived per-request from the `Host`/`X-Forwarded-Proto` headers.
#[derive(Clone)]
pub struct CardInfo {
    /// Agent name (card `name`).
    pub name: String,
    /// One-paragraph description (card `description`).
    pub description: String,
    /// Advertised skills as `(id, name, description)` tuples.
    pub skills: Vec<(String, String, String)>,
    /// Optional provider/organization advertised on the card (A2A `provider`). `None` → omitted.
    pub provider: Option<flux_a2a::AgentProvider>,
    /// Optional documentation URL advertised on the card (A2A `documentationUrl`). `None` → omitted.
    pub documentation_url: Option<String>,
    /// Optional icon URL advertised on the card (A2A `iconUrl`). `None` → omitted.
    pub icon_url: Option<String>,
}

impl CardInfo {
    /// The default card: flux's built-in coding agent (what the standalone server advertises).
    pub fn flux_coding() -> Self {
        Self {
            name: "flux".to_string(),
            description: "flux — a precise, autonomous coding agent. Reads, writes, edits, \
                 searches, and runs code in a workspace. Carries tasks from instruction to \
                 verified completion through a deterministic Flux-Lang plan + guarded safety \
                 envelope."
                .to_string(),
            skills: vec![(
                "coding".to_string(),
                "Coding Agent".to_string(),
                "Read, write, edit, search, and execute code tasks in a workspace. The \
                 agent plans, executes, and verifies — then reports back."
                    .to_string(),
            )],
            provider: None,
            documentation_url: None,
            icon_url: None,
        }
    }

    /// A card for a program-declared agent, named and described from its `agent` declaration.
    pub fn for_agent(name: impl Into<String>, description: Option<String>) -> Self {
        let name = name.into();
        let description = description.unwrap_or_else(|| format!("flux agent `{name}`."));
        Self {
            skills: vec![("agent".to_string(), name.clone(), description.clone())],
            name,
            description,
            provider: None,
            documentation_url: None,
            icon_url: None,
        }
    }

    /// Advertise a provider/organization on the card (builder-style). See [`provider`](Self::provider).
    pub fn with_provider(mut self, provider: flux_a2a::AgentProvider) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Advertise a documentation URL on the card (builder-style). See
    /// [`documentation_url`](Self::documentation_url).
    pub fn with_documentation_url(mut self, url: impl Into<String>) -> Self {
        self.documentation_url = Some(url.into());
        self
    }

    /// Advertise an icon URL on the card (builder-style). See [`icon_url`](Self::icon_url).
    pub fn with_icon_url(mut self, url: impl Into<String>) -> Self {
        self.icon_url = Some(url.into());
        self
    }
}

/// Shared router state: the engine that runs turns plus the agent card to advertise. Handlers extract
/// either piece via [`FromRef`], so existing `State<Arc<FlowEngine>>` handlers keep working.
#[derive(Clone)]
pub struct ServerState {
    engine: Arc<FlowEngine>,
    card: Arc<CardInfo>,
    turn_gate: TurnGate,
    a2a_ttl: A2aTtl,
    auth: Arc<ServerAuth>,
    /// The in-process registry of live A2A tasks (A-54): cancel/resubscribe handles + the
    /// sweep keep-list. One per router, like the turn gate.
    tasks: Arc<a2a::TaskRegistry>,
}

impl FromRef<ServerState> for Arc<FlowEngine> {
    fn from_ref(s: &ServerState) -> Self {
        s.engine.clone()
    }
}

impl FromRef<ServerState> for Arc<CardInfo> {
    fn from_ref(s: &ServerState) -> Self {
        s.card.clone()
    }
}

impl FromRef<ServerState> for TurnGate {
    fn from_ref(s: &ServerState) -> Self {
        s.turn_gate.clone()
    }
}

impl FromRef<ServerState> for A2aTtl {
    fn from_ref(s: &ServerState) -> Self {
        s.a2a_ttl
    }
}

impl FromRef<ServerState> for Arc<ServerAuth> {
    fn from_ref(s: &ServerState) -> Self {
        s.auth.clone()
    }
}

impl FromRef<ServerState> for Arc<a2a::TaskRegistry> {
    fn from_ref(s: &ServerState) -> Self {
        s.tasks.clone()
    }
}

/// Bind `addr` and serve until shutdown, authenticating per `auth` (see [`ServerAuth`] for the
/// three modes). [`ServerAuth::Open`] requires a loopback bind.
pub async fn serve(addr: &str, agent: FlowEngine, auth: ServerAuth) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    eprintln!("flux server listening on http://{addr}");
    eprintln!("  A2A agent card:  http://{addr}/.well-known/agent-card.json");
    eprintln!("  A2A endpoint:    http://{addr}/a2a  (message/send, message/stream)");
    serve_on(listener, agent, auth).await
}

/// Serve on an already-bound listener (lets callers pick an ephemeral port).
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    agent: FlowEngine,
    auth: ServerAuth,
) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    // The non-loopback refusal now lives in `router` (construction-time, C-190); `serve_on` simply
    // propagates it, so there is one enforcement point every caller shares.
    let router = router(Arc::new(agent), auth, CardInfo::flux_coding(), addr)?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// The one predicate that decides whether an *unauthenticated* server may bind `addr`: loopback
/// only. IP-classification based (`is_loopback` covers all of `127.0.0.0/8` and `::1`), which is a
/// deliberately different question from the `a2a` push-notification target guard's hostname
/// allow-list (`a2a.rs` `configured_push_private_net`): that one governs *outbound* SSRF egress and
/// is routed through the DNS-aware `guard_url`, this one governs the *inbound* listen address. Two
/// layers, two questions — they are not, and should not be, the same check.
fn unauthenticated_bind_allowed(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// The construction-time guard behind the safety invariant *"an unauthenticated server is never
/// exposed off loopback"* (`AGENTS.md`: *there are no bypass paths — don't add one*). Enforced HERE,
/// at router build, rather than only inside [`serve_on`]: a lower-level caller that mounts the router
/// into its own `axum::serve` (the `a2a` channel does exactly this) inherits the refusal by
/// construction instead of being silently responsible for re-deriving it. [`ServerAuth::Open`] on a
/// non-loopback bind is remote code execution against the auto-approving daemon, so it is refused
/// outright — there is deliberately no escape hatch. A deployment that must face the network fronts
/// the loopback daemon with an authenticating reverse proxy (or configures shared-secret/principal
/// auth), it does not open the listener itself.
fn guard_open_bind(auth: &ServerAuth, addr: SocketAddr) -> anyhow::Result<()> {
    if matches!(auth, ServerAuth::Open) && !unauthenticated_bind_allowed(addr) {
        anyhow::bail!(
            "refusing to build an unauthenticated router for non-loopback bind {addr}; set \
             FLUX_SERVER_TOKEN (shared-secret auth) or bind to 127.0.0.1/::1"
        );
    }
    Ok(())
}

/// Serve a resolver-keyed multi-agent mount (D-63) until shutdown — the guarded entry point for
/// [`router_multi`]. Refuses an unauthenticated ([`ServerAuth::Open`]) non-loopback bind, exactly
/// as [`serve`] does for the single-agent surface: an open, auto-approving `/:agent_id/a2a` is
/// remote code execution.
pub async fn serve_multi(
    addr: &str,
    resolver: Arc<dyn AgentResolver>,
    auth: ServerAuth,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_multi_on(listener, resolver, auth).await
}

/// [`serve_multi`] on an already-bound listener (ephemeral-port callers). Enforces the same
/// Open-on-non-loopback refusal as [`serve_on`].
pub async fn serve_multi_on(
    listener: tokio::net::TcpListener,
    resolver: Arc<dyn AgentResolver>,
    auth: ServerAuth,
) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    // Same construction-time refusal as the single-agent mount (C-190): `router_multi` enforces it,
    // `serve_multi_on` propagates it.
    let router = router_multi(resolver, auth, addr)?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Wait for the process-level shutdown signals a daemon should honor.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("failed to install Ctrl-C handler: {e}");
        }
    };

    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(e) => {
                    eprintln!("failed to install SIGTERM handler: {e}");
                    std::future::pending::<()>().await;
                }
            }
        };

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}

// ── Daemon resource limits (C-189) ───────────────────────────────────────────

/// Default request-body cap: 1 MiB. Every body-buffering handler (`Json`/`Bytes`) rejects a body
/// over this with `413 Payload Too Large` *during extraction* — before the handler runs. A2A
/// JSON-RPC envelopes and session-message prompts sit far below it; a deployment that legitimately
/// ships larger uploads raises it via `FLUX_SERVER_MAX_BODY_BYTES`.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Default request timeout: 300s. Generous enough for a full multi-tool agent turn on the
/// blocking `message/send` / `POST /sessions/{id}/messages` / `/webhook` paths, finite enough that
/// a wedged request cannot pin a connection forever. SSE/streaming routes are exempt (a long-lived
/// stream is not a stuck request). Raise via `FLUX_SERVER_REQUEST_TIMEOUT_SECS`.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 300;

/// The daemon's resource limits (C-189): `SECURITY.md` names denial of service in the `--serve`
/// daemon as in scope, and without these every mounted router accepts an unbounded body and holds
/// a connection for as long as a handler runs. Both close that gap for the two vectors that need
/// no keying decision; rate limiting (which does — per token / principal / realm) is deliberately
/// out of scope for C-189.
///
/// Both fields default conservatively and are overridable per deployment via env (the same knob
/// style as `FLUX_A2A_MAX_INFLIGHT_PER_REALM`), read once at router-build time.
#[derive(Clone, Copy, Debug)]
pub struct ServerLimits {
    /// Max request-body bytes any body-buffering handler accepts before returning `413` during
    /// extraction (see [`DEFAULT_MAX_BODY_BYTES`]).
    pub max_body_bytes: usize,
    /// Max wall-clock a non-streaming request may take to PRODUCE its response before `408` is
    /// returned (see [`DEFAULT_REQUEST_TIMEOUT_SECS`]). Bounds response production, not body
    /// streaming — which is exactly why an SSE response, produced promptly then streamed for the
    /// life of the turn, is unharmed even where the layer is applied.
    pub request_timeout: Duration,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
        }
    }
}

impl ServerLimits {
    /// The limits in force, reading env overrides once (mirrors [`max_inflight_per_realm`]):
    /// `FLUX_SERVER_MAX_BODY_BYTES` (positive integer bytes) and `FLUX_SERVER_REQUEST_TIMEOUT_SECS`
    /// (positive integer seconds; `0`/missing/unparseable falls back to the documented default —
    /// `0` is never read as "disable", so the daemon is never accidentally left unbounded).
    fn from_env() -> Self {
        let d = Self::default();
        let max_body_bytes = std::env::var("FLUX_SERVER_MAX_BODY_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(d.max_body_bytes);
        let request_timeout = std::env::var("FLUX_SERVER_REQUEST_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&n| n > 0)
            .map(Duration::from_secs)
            .unwrap_or(d.request_timeout);
        Self {
            max_body_bytes,
            request_timeout,
        }
    }
}

/// The request `TimeoutLayer` in force. `TimeoutLayer::with_status_code` (not the deprecated
/// `::new`) so the timeout answers a real `408 Request Timeout`.
fn request_timeout_layer(limits: ServerLimits) -> TimeoutLayer {
    TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, limits.request_timeout)
}

/// Build the API router over `engine`, advertising `card` on the A2A discovery endpoint and
/// authenticating per `auth` (every route except `/health` and the agent card). Public so the
/// `a2a` channel ([`flux_channels`]) can mount it onto a program agent's engine with its own
/// graceful-shutdown serve.
///
/// `bind` is the address the caller will serve this router on. Construction **refuses**
/// [`ServerAuth::Open`] on a non-loopback `bind` (see [`guard_open_bind`]): the unauthenticated +
/// off-loopback combination is unrepresentable as a built router, so a caller that mounts this into
/// its own `axum::serve` gets the safety invariant by construction rather than re-deriving it. That
/// is why this returns a `Result` — the refusal is the error.
pub fn router(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    bind: SocketAddr,
) -> anyhow::Result<Router> {
    guard_open_bind(&auth, bind)?;
    Ok(router_with_ttl(engine, auth, card, a2a_ttl_from_config()))
}

/// Resolve the A2A session TTL from the layered flux config (`[server] a2a_session_ttl_secs`,
/// project over user, default 1h, `0` = never prune). Resolved here — at router build — so every
/// mount of the router (the standalone server and the `a2a` channel) gets the same retention
/// behavior without each caller plumbing the knob. A malformed config file falls back to the
/// default with a warning rather than failing the surface (the CLI already fails loudly on it).
fn a2a_ttl_from_config() -> A2aTtl {
    let ttl = std::env::current_dir()
        .ok()
        .and_then(|cwd| match flux_runtime::metadata::load_config(&cwd) {
            Ok(cfg) => Some(cfg.a2a_session_ttl_secs()),
            Err(e) => {
                eprintln!("(ignoring malformed flux config for the A2A session TTL: {e})");
                None
            }
        })
        .unwrap_or(flux_config::DEFAULT_A2A_SESSION_TTL_SECS);
    A2aTtl(ttl)
}

/// [`router`] with an explicit A2A session TTL (tests inject one; production resolves from config).
fn router_with_ttl(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    a2a_ttl: A2aTtl,
) -> Router {
    router_with_ttl_and_limits(engine, auth, card, a2a_ttl, ServerLimits::from_env())
}

/// [`router_with_ttl`] with explicit resource limits (C-189). Tests inject tiny limits to exercise
/// the `413`/`408` paths; production reads them from the environment once at build time.
fn router_with_ttl_and_limits(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    a2a_ttl: A2aTtl,
    limits: ServerLimits,
) -> Router {
    let auth = Arc::new(auth);
    let state = ServerState {
        engine,
        card: Arc::new(card),
        turn_gate: Arc::new(tokio::sync::Mutex::new(())),
        a2a_ttl,
        auth: auth.clone(),
        tasks: Arc::new(a2a::TaskRegistry::default()),
    };
    let timeout = request_timeout_layer(limits);

    // Auth-exempt routes — registered outside the middleware layer so path-string comparison
    // cannot be bypassed by percent-encoding or double-slash tricks. Constant-response liveness /
    // discovery handlers, but they still carry the request timeout so every non-streaming route is
    // bounded uniformly (C-189).
    let exempt = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/.well-known/agent-card.json", get(a2a::agent_card))
        .route("/.well-known/agent.json", get(a2a::agent_card))
        .layer(timeout);

    // Session-addressed REST routes: ONE structural realm guard over this subtree — including the
    // write path (`POST …/messages`) — so a route added here later is realm-guarded by
    // construction, never by per-handler enumeration. (Session ids are guessable `s_<n>`; guarding
    // reads while leaving a write route open would be cross-tenant read+write.) The SSE stream
    // route is deliberately NOT here — it lives in `sessions_stream` below so it can be exempted
    // from the request timeout; both sub-routers apply `realm_guard`, so the whole `/sessions/{id}/*`
    // subtree stays realm-guarded regardless of the split.
    let sessions_rest = Router::new()
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/messages", post(post_message))
        .route("/sessions/{id}/usage", get(get_session_usage))
        .route_layer(middleware::from_fn_with_state(state.clone(), realm_guard));

    // Non-streaming protected routes + the REST session subtree carry the request TimeoutLayer: a
    // wedged handler (or a blocking turn that overruns the generous default) yields `408` instead
    // of pinning the connection. `/a2a` belongs here — its blocking `message/send` runs a full
    // turn in-handler, exactly the unbounded hold the timeout bounds; its `message/stream` path
    // returns the SSE response promptly, and the timeout bounds response PRODUCTION (not body
    // streaming — tower-http `TimeoutLayer`), so an in-flight stream is never severed.
    let timed = Router::new()
        .route("/a2a", post(a2a::a2a_handler))
        .route("/sessions", post(create_session))
        .route("/usage", get(get_usage_all))
        .route("/webhook", post(webhook))
        .merge(sessions_rest)
        .layer(timeout);

    // SSE routes: DELIBERATELY exempt from the request timeout. An SSE response holds the
    // connection open for the whole turn by design — a long-lived stream is not a stuck request,
    // and a request timeout is the wrong tool for it. (tower-http's `TimeoutLayer` would not
    // actually fire here either — it bounds response production and the handler returns its `Sse`
    // promptly — but the exemption is made structural so a future SSE route that does more work
    // before returning stays safe too.) The body-size cap below still applies; an SSE request
    // carries no large upload.
    let sessions_stream = Router::new()
        .route("/sessions/{id}/stream", get(stream_message))
        .route_layer(middleware::from_fn_with_state(state.clone(), realm_guard));

    // `require_auth` (the outer route_layer, applied after the merge) runs BEFORE `realm_guard`, so
    // authentication always precedes any existence signal (A2A §13.1).
    let protected = timed
        .merge(sessions_stream)
        .route_layer(middleware::from_fn_with_state(auth, require_auth));

    // DefaultBodyLimit over the whole surface (C-189): a body over the cap is rejected with `413`
    // during extraction, before any handler runs. Applied outermost so every route — exempt,
    // timed, and streaming — is covered by construction, and no future route can forget it.
    exempt
        .merge(protected)
        .layer(DefaultBodyLimit::max(limits.max_body_bytes))
        .with_state(state)
}

// ── Multi-agent A2A mount (D-63) ────────────────────────────────────────────────

/// Resolves a path segment (`/:agent_id/…`) to the agent that serves it — the seam that turns
/// flux-server's single-agent A2A surface into an N-agent mount keyed by path, so a multi-tenant
/// host gets flux's A2A session lifecycle (TTL retention, `message/stream` SSE, `contextId`
/// continuity) instead of rebuilding it.
///
/// `resolve` receives the **already-authenticated** [`AuthContext`] on the JSON-RPC path (the auth
/// layer runs first), so a resolver may scope which agents a principal can even see — but it never
/// authenticates (auth stays one layer, D-63's answered open question). Returning `None` yields a
/// constant 404 indistinguishable from any other unknown resource (A2A §13.1). The resolved engine
/// is pinned for the whole request — including a streaming turn's lifetime — so re-resolution can
/// never swap the agent mid-stream.
///
/// **Card-route caveat:** the discovery card is public (A2A requires it), so `resolve` is called
/// there with `auth = None` and a 200-vs-404 distinguishes a known `agent_id` from an unknown one
/// *before* any token check. If a deployment's `agent_id`s are themselves sensitive (per-tenant
/// existence must not be enumerable), do not key them on guessable strings — the public card
/// cannot hide existence without violating the spec.
#[async_trait::async_trait]
pub trait AgentResolver: Send + Sync {
    async fn resolve(&self, agent_id: &str, auth: Option<&AuthContext>) -> Option<ResolvedAgent>;
}

/// What an [`AgentResolver`] yields: the engine that runs turns for this agent plus its discovery
/// card. (Each agent owns its own `FlowEngine` — and thus its own event store — so A2A session
/// TTL, `contextId` continuity, and per-principal realm scoping are already isolated per agent.)
///
/// **Principal-mode contract:** the mount passes an immutable [`TurnIdentity`] into this engine's
/// gate-held turn entry. The same lexical runtime context is propagated across the supervised
/// `task` boundary, so a spawned sub-agent snapshots the request principal rather than the
/// assembly-time fallback identity. Engine/spawner assembly may still share one immutable
/// `IdentityCell` as their non-principal default; request handling never retargets it.
#[derive(Clone)]
pub struct ResolvedAgent {
    pub engine: Arc<FlowEngine>,
    pub card: Arc<CardInfo>,
}

/// A fixed set of agents keyed by name — the built-in resolver for a program that declares its
/// agents up front (`flux app run`). Dynamic hosts (per-tenant agents minted at runtime)
/// implement [`AgentResolver`] themselves.
pub struct StaticResolver(std::collections::HashMap<String, ResolvedAgent>);

impl StaticResolver {
    pub fn new() -> Self {
        Self(std::collections::HashMap::new())
    }

    pub fn with_agent(
        mut self,
        name: impl Into<String>,
        engine: Arc<FlowEngine>,
        card: CardInfo,
    ) -> Self {
        self.0.insert(
            name.into(),
            ResolvedAgent {
                engine,
                card: Arc::new(card),
            },
        );
        self
    }
}

impl Default for StaticResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AgentResolver for StaticResolver {
    async fn resolve(&self, agent_id: &str, _auth: Option<&AuthContext>) -> Option<ResolvedAgent> {
        self.0.get(agent_id).cloned()
    }
}

/// Router state for the multi-agent A2A mount. Like [`ServerState`] but the engine/card are
/// resolved per request from the path rather than baked in.
#[derive(Clone)]
pub struct MultiState {
    pub(crate) resolver: Arc<dyn AgentResolver>,
    pub(crate) turn_gate: TurnGate,
    pub(crate) a2a_ttl: A2aTtl,
    pub(crate) auth: Arc<ServerAuth>,
    /// Live A2A tasks across every mounted agent (A-54); entries are scoped by `agent_id`, so
    /// two agents' identical session ids can never collide.
    pub(crate) tasks: Arc<a2a::TaskRegistry>,
}

impl FromRef<MultiState> for TurnGate {
    fn from_ref(s: &MultiState) -> Self {
        s.turn_gate.clone()
    }
}
impl FromRef<MultiState> for A2aTtl {
    fn from_ref(s: &MultiState) -> Self {
        s.a2a_ttl
    }
}
impl FromRef<MultiState> for Arc<ServerAuth> {
    fn from_ref(s: &MultiState) -> Self {
        s.auth.clone()
    }
}
impl FromRef<MultiState> for Arc<dyn AgentResolver> {
    fn from_ref(s: &MultiState) -> Self {
        s.resolver.clone()
    }
}
impl FromRef<MultiState> for Arc<a2a::TaskRegistry> {
    fn from_ref(s: &MultiState) -> Self {
        s.tasks.clone()
    }
}

/// Build a resolver-keyed multi-agent A2A mount (D-63): each agent is served under `/:agent_id/`
/// with flux's full A2A machinery. Routes:
/// - `GET  /health`
/// - `GET  /:agent_id/.well-known/agent-card.json` (+ `/agent.json` alias) — discovery, public
/// - `POST /:agent_id/a2a` — JSON-RPC 2.0 (`message/send`, `message/stream`)
///
/// Auth is one outer layer ([`require_auth`], same three modes), so the resolver sees the
/// authenticated principal and every A2A turn runs the safety envelope under it. Per-agent REST
/// session routes (`/:agent_id/sessions/*`) are intentionally out of scope here — the mount serves
/// the A2A protocol surface a multi-agent host actually needs; the single-agent [`router`] remains
/// the way to expose the full REST surface for one engine.
///
/// Construction **refuses** [`ServerAuth::Open`] on a non-loopback `bind` (C-190), exactly as the
/// single-agent [`router`] does: an open mount on a public interface auto-approves every tool call,
/// so the unauthenticated + off-loopback combination is refused at build time and a caller wiring
/// `axum::serve` directly inherits the guarantee instead of re-deriving it. Hence the `Result`.
pub fn router_multi(
    resolver: Arc<dyn AgentResolver>,
    auth: ServerAuth,
    bind: SocketAddr,
) -> anyhow::Result<Router> {
    guard_open_bind(&auth, bind)?;
    Ok(router_multi_with_ttl(resolver, auth, a2a_ttl_from_config()))
}

fn router_multi_with_ttl(
    resolver: Arc<dyn AgentResolver>,
    auth: ServerAuth,
    a2a_ttl: A2aTtl,
) -> Router {
    router_multi_with_ttl_and_limits(resolver, auth, a2a_ttl, ServerLimits::from_env())
}

/// [`router_multi_with_ttl`] with explicit resource limits (C-189).
fn router_multi_with_ttl_and_limits(
    resolver: Arc<dyn AgentResolver>,
    auth: ServerAuth,
    a2a_ttl: A2aTtl,
    limits: ServerLimits,
) -> Router {
    let auth = Arc::new(auth);
    let state = MultiState {
        resolver,
        turn_gate: Arc::new(tokio::sync::Mutex::new(())),
        a2a_ttl,
        auth: auth.clone(),
        tasks: Arc::new(a2a::TaskRegistry::default()),
    };
    let timeout = request_timeout_layer(limits);
    // Discovery card is public (structurally auth-exempt), exactly as in the single-agent mount.
    let exempt = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/{agent_id}/.well-known/agent-card.json",
            get(a2a::agent_card_multi),
        )
        .route(
            "/{agent_id}/.well-known/agent.json",
            get(a2a::agent_card_multi),
        )
        .layer(timeout);
    // The mount's one work route carries the request timeout (C-189): `/{agent_id}/a2a` bounds its
    // blocking `message/send` full-turn hold, while its `message/stream` SSE — produced promptly,
    // then streamed for the life of the turn — is unaffected, since the timeout bounds response
    // production, not body streaming. There is no separate SSE-only route to exempt here.
    let protected = Router::new()
        .route("/{agent_id}/a2a", post(a2a::a2a_handler_multi))
        .layer(timeout)
        .route_layer(middleware::from_fn_with_state(auth, require_auth));
    // DefaultBodyLimit over every route (C-189), applied outermost — see [`router_with_ttl_and_limits`].
    exempt
        .merge(protected)
        .layer(DefaultBodyLimit::max(limits.max_body_bytes))
        .with_state(state)
}

/// The auth gate, per [`ServerAuth`] mode. Exempt routes (`/health`, the agent card) are
/// registered outside this middleware's scope in [`router`] — no path-string bypass is possible.
///
/// - `Open` — pass-through (loopback-only bind enforced in [`serve_on`]).
/// - `SharedSecret` — constant-time compare of `Authorization: Bearer <secret>` (pre-D-69, plus
///   the RFC 7235-required `WWW-Authenticate` challenge on 401).
/// - `Principal` — resolve the bearer to an [`AuthContext`] via the configured
///   [`RequestAuthenticator`] and stash it in request extensions.
///
/// BOTH authenticated modes reject a request carrying more than one `Authorization` header (a
/// front proxy honoring a different copy than we read is a smuggling-style divergence) via the
/// shared [`single_auth_header`].
async fn require_auth(
    State(auth): State<Arc<ServerAuth>>,
    mut req: Request,
    next: Next,
) -> Response {
    match auth.as_ref() {
        ServerAuth::Open => next.run(req).await,
        ServerAuth::SharedSecret { secret, .. } => {
            let header = match single_auth_header(&req) {
                Ok(h) => h,
                Err(resp) => return *resp,
            };
            let presented = header.and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
            if !constant_time_eq(presented.as_bytes(), secret.as_bytes()) {
                return unauthorized();
            }
            next.run(req).await
        }
        ServerAuth::Principal(p) => {
            let header = match single_auth_header(&req) {
                Ok(h) => h.map(str::to_owned),
                Err(resp) => return *resp,
            };
            let token = match flux_auth::request::bearer_from_header(header.as_deref()) {
                Ok(t) => t.to_owned(),
                Err(_) => return unauthorized(),
            };
            match p.authenticator.authenticate(&token).await {
                Ok(ctx) => {
                    req.extensions_mut().insert(ctx);
                    next.run(req).await
                }
                Err(AuthError::Unauthorized) => unauthorized(),
                Err(AuthError::Unavailable(detail)) => {
                    // Log-only payload: the wire response stays a constant string.
                    eprintln!("(auth backend unavailable: {detail})");
                    auth_unavailable()
                }
            }
        }
    }
}

/// The single `Authorization` header value, or a constant 401 if more than one is present — a
/// front proxy that validates/normalizes a different copy than axum reads is a request-smuggling
/// divergence. Applied uniformly by both authenticated modes in [`require_auth`]. (`Box`ed error
/// to keep the `Ok` path small — the caller un-boxes on the single reject.)
fn single_auth_header(req: &Request) -> Result<Option<&str>, Box<Response>> {
    let mut it = req.headers().get_all(header::AUTHORIZATION).iter();
    let first = it.next();
    if it.next().is_some() {
        return Err(Box::new(unauthorized()));
    }
    Ok(first.and_then(|v| v.to_str().ok()))
}

/// Realm guard for every `/sessions/{id}/*` route (principal mode only; other modes pass through
/// untouched). A session that does not exist and a session owned by another realm produce the
/// same constant 404 ([`realm_not_found`]) — indistinguishable by status, body, or headers.
async fn realm_guard(State(state): State<ServerState>, mut req: Request, next: Next) -> Response {
    if !matches!(state.auth.as_ref(), ServerAuth::Principal(_)) {
        return next.run(req).await;
    }
    // Fail closed: no resolved principal on a principal-mode session route is a constant 401
    // (unreachable behind `require_auth`, which runs first).
    let Some(ctx) = req.extensions().get::<AuthContext>() else {
        return unauthorized();
    };
    let realm = realm_of(ctx);
    use axum::RequestExt;
    let id = match req.extract_parts::<Path<String>>().await {
        Ok(Path(id)) => id,
        Err(_) => return realm_not_found(),
    };
    match state.engine.events.info(&id) {
        Ok(info) if info.context.account.as_deref() == Some(realm.as_str()) => next.run(req).await,
        // Missing session, unreadable store row, or another realm's session: one constant shape.
        _ => realm_not_found(),
    }
}

/// Length-aware constant-time byte comparison (avoids leaking the token via response timing).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn err500(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Mint a session tagged with the caller's realm in principal mode (`EventContext.account`, the
/// D-02 substrate the realm guard and `find_correlated_in_realm` scope by); untagged otherwise
/// (byte-for-byte pre-D-69). Fail closed: principal mode without a resolved principal is an error.
fn mint_session(
    agent: &FlowEngine,
    auth: &ServerAuth,
    ctx: Option<&AuthContext>,
) -> Result<String, Box<Response>> {
    match auth {
        ServerAuth::Open | ServerAuth::SharedSecret { .. } => agent
            .events
            .create_session(&agent.model)
            .map_err(|e| Box::new(err500(e).into_response())),
        ServerAuth::Principal(_) => {
            let ctx = ctx.ok_or_else(|| Box::new(unauthorized()))?;
            let evctx = flux_events::EventContext {
                account: Some(realm_of(ctx)),
                ..Default::default()
            };
            agent
                .events
                .create_session_with_context(&agent.model, &evctx)
                .map_err(|e| Box::new(err500(e).into_response()))
        }
    }
}

async fn create_session(
    State(agent): State<Shared>,
    State(auth): State<Arc<ServerAuth>>,
    ctx: Option<Extension<AuthContext>>,
) -> Result<Json<Value>, Response> {
    let id = mint_session(&agent, &auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    Ok(Json(json!({ "id": id, "model": agent.model })))
}

async fn get_session(
    State(agent): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let info = agent
        .events
        .info(&id)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(Json(json!({
        "id": info.id,
        "model": info.model,
        "created_at_ms": info.created_at_ms,
    })))
}

#[derive(serde::Deserialize)]
struct MessageRequest {
    input: String,
}

async fn post_message(
    State(agent): State<Shared>,
    State(auth): State<Arc<ServerAuth>>,
    ctx: Option<Extension<AuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, Response> {
    let mut sink = Collect::default();
    let turn = server_turn_context(&auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    run_server_turn(
        &agent,
        &turn,
        &id,
        &req.input,
        &mut sink,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(|e| err500(e).into_response())?;
    Ok(Json(json!({
        "text": sink.text,
        "tool_calls": sink.tools,
        // Full usage (C-06): every tier, not just input/output — a caller pricing spend itself
        // (or just wanting the true context-window occupancy) needs the cache + reasoning tiers
        // this used to silently drop.
        "usage": sink.usage.map(|u| usage_json(&u)),
    })))
}

/// The full [`Usage`] as JSON — every tier, so a caller never loses cache/reasoning figures the way
/// the old `post_message` response did (C-06).
fn usage_json(u: &Usage) -> Value {
    json!({
        "input": u.input_tokens,
        "output": u.output_tokens,
        "cache_creation": u.cache_creation_input_tokens,
        "cache_read": u.cache_read_input_tokens,
        "reasoning": u.reasoning_tokens,
    })
}

/// One [`flux_events::ModelCost`] row as JSON: tokens per tier, call count, and cost (when the model
/// is priced) — the shape both usage endpoints below return per model.
fn model_cost_json(row: &flux_events::ModelCost) -> Value {
    json!({
        "model": row.model,
        "calls": row.calls,
        "usage": usage_json(&row.usage),
        "cost_usd": row.cost.map(|m| m.usd),
        "subscription": row.cost.map(|m| m.subscription),
    })
}

/// `GET /sessions/{id}/usage` — per-model token tiers + cost for one session (C-06).
async fn get_session_usage(
    State(agent): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pricing = flux_credentials::load_pricing_table();
    let rows = agent.events.cost_summary(&id, &pricing).map_err(err500)?;
    Ok(Json(json!({
        "session_id": id,
        "models": rows.iter().map(model_cost_json).collect::<Vec<_>>(),
    })))
}

/// `GET /usage` — per-model token tiers + cost (C-06). Across every session in the open/shared-
/// secret modes; in principal mode, scoped to the caller's realm (summed over the realm's own
/// streams — another tenant's spend must not be readable, A2A §13.1).
async fn get_usage_all(
    State(agent): State<Shared>,
    State(auth): State<Arc<ServerAuth>>,
    ctx: Option<Extension<AuthContext>>,
) -> Result<Json<Value>, Response> {
    let pricing = flux_credentials::load_pricing_table();
    let rows = match auth.as_ref() {
        ServerAuth::Open | ServerAuth::SharedSecret { .. } => agent
            .events
            .cost_summary_all(&pricing)
            .map_err(|e| err500(e).into_response())?,
        ServerAuth::Principal(_) => {
            // Realm-scoped, but through the SAME store-level fold as the unscoped rollup
            // (pricing + legacy/canonical key de-splitting), so the two modes never disagree for
            // the same data — only the stream set differs.
            let ctx = ctx.as_ref().map(|e| &e.0).ok_or_else(unauthorized)?;
            agent
                .events
                .cost_summary_for_account(&realm_of(ctx), &pricing)
                .map_err(|e| err500(e).into_response())?
        }
    };
    Ok(Json(json!({
        "models": rows.iter().map(model_cost_json).collect::<Vec<_>>(),
    })))
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    input: String,
}

/// `GET /sessions/{id}/stream?input=…` → Server-Sent Events. Emits `text` events as tokens arrive,
/// `tool` events as tools run, and a final `done` event. The turn runs on a spawned task feeding an
/// mpsc channel that backs the SSE stream.
async fn stream_message(
    State(agent): State<Shared>,
    State(auth): State<Arc<ServerAuth>>,
    ctx: Option<Extension<AuthContext>>,
    Path(id): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    // Resolve before establishing SSE so principal mode without a context is a normal 401.
    let turn = server_turn_context(&auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let agent = agent.clone();
    tokio::spawn(async move {
        let mut sink = SseSink { tx: tx.clone() };
        if let Err(e) = run_server_turn(
            &agent,
            &turn,
            &id,
            &q.input,
            &mut sink,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        {
            let _ = tx.send(Event::default().event("error").data(e.to_string()));
        }
        let _ = tx.send(Event::default().event("done").data("end"));
    });
    let stream = async_stream::stream! {
        while let Some(ev) = rx.recv().await {
            yield Ok(ev);
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Forwards a turn's deltas as SSE events over an mpsc channel.
struct SseSink {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
}

impl AgentSink for SseSink {
    fn text_delta(&mut self, t: &str) {
        let _ = self.tx.send(Event::default().event("text").data(t));
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        let _ = self.tx.send(Event::default().event("tool").data(name));
    }
}

/// Inbound webhook: a single external event creates a fresh session and runs one turn. This is
/// the trigger surface for integrations (a CI hook, or a chat message bridged by an external adapter).
async fn webhook(
    State(agent): State<Shared>,
    State(auth): State<Arc<ServerAuth>>,
    ctx: Option<Extension<AuthContext>>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, Response> {
    // In principal mode the webhook's fresh session is tagged with the caller's realm, like
    // every other mint — an untagged session would be unreachable to its own creator.
    let session_id = mint_session(&agent, &auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    let mut sink = Collect::default();
    let turn = server_turn_context(&auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    run_server_turn(
        &agent,
        &turn,
        &session_id,
        &req.input,
        &mut sink,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(|e| err500(e).into_response())?;
    Ok(Json(json!({
        "session_id": session_id,
        "text": sink.text,
        "tool_calls": sink.tools,
    })))
}

#[derive(Default)]
pub(crate) struct Collect {
    pub(crate) text: String,
    pub(crate) tools: Vec<String>,
    pub(crate) usage: Option<Usage>,
}

impl AgentSink for Collect {
    fn text_delta(&mut self, t: &str) {
        self.text.push_str(t);
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        self.tools.push(name.to_string());
    }
    fn turn_end(&mut self, usage: Option<Usage>) {
        self.usage = usage;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use tower::ServiceExt; // for `oneshot`

    #[test]
    fn constant_time_eq_matches() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secres"));
        assert!(!constant_time_eq(b"secret", b"secre")); // length mismatch
    }

    #[test]
    fn unauthenticated_bind_is_loopback_only() {
        assert!(unauthenticated_bind_allowed("127.0.0.1:0".parse().unwrap()));
        assert!(unauthenticated_bind_allowed("[::1]:0".parse().unwrap()));
        assert!(!unauthenticated_bind_allowed("0.0.0.0:0".parse().unwrap()));
        assert!(!unauthenticated_bind_allowed("[::]:0".parse().unwrap()));
    }

    /// Build a tiny router carrying only the auth layer over a `/health` and a protected route, so
    /// the gate can be exercised without standing up a full `Agent`.
    /// Mirror the split-router structure from [`router`]: exempt routes outside the middleware,
    /// protected routes inside.
    fn guarded_app(auth: ServerAuth) -> Router {
        let exempt = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route(
                "/.well-known/agent-card.json",
                get(|| async { Json(json!({})) }),
            )
            .route("/.well-known/agent.json", get(|| async { Json(json!({})) }));
        let protected = Router::new()
            .route("/protected", get(|| async { "data" }))
            .route_layer(middleware::from_fn_with_state(Arc::new(auth), require_auth));
        exempt.merge(protected)
    }

    async fn status(app: Router, path: &str, auth: Option<&str>) -> StatusCode {
        let mut rb = HttpRequest::get(path);
        if let Some(a) = auth {
            rb = rb.header("authorization", a);
        }
        app.oneshot(rb.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn auth_required_when_token_configured() {
        let app = || guarded_app(ServerAuth::from_token(Some("s3cr3t".to_string())));
        // No / wrong token → 401 on a protected route.
        assert_eq!(
            status(app(), "/protected", None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(app(), "/protected", Some("Bearer nope")).await,
            StatusCode::UNAUTHORIZED
        );
        // Correct token → 200.
        assert_eq!(
            status(app(), "/protected", Some("Bearer s3cr3t")).await,
            StatusCode::OK
        );
        // /health and /.well-known/agent.json are exempt (liveness probes / A2A discovery).
        assert_eq!(status(app(), "/health", None).await, StatusCode::OK);
        assert_eq!(
            status(app(), "/.well-known/agent-card.json", None).await,
            StatusCode::OK
        );
        assert_eq!(
            status(app(), "/.well-known/agent.json", None).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn no_token_configured_is_pass_through() {
        // With no configured token (loopback-only mode), routes are open.
        assert_eq!(
            status(guarded_app(ServerAuth::Open), "/protected", None).await,
            StatusCode::OK
        );
    }

    /// A provider that never gets called in the usage-endpoint tests below — the fixture engine
    /// exists only so `router()` has a real `Arc<FlowEngine>` to mount; the usage endpoints read
    /// straight from `agent.events`, seeded directly.
    struct UnusedProvider;
    #[async_trait::async_trait]
    impl flux_provider::Provider for UnusedProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(
            &self,
            _req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    /// A provider that declares an intent, then answers with a one-word prose turn.
    struct ProseProvider;
    #[async_trait::async_trait]
    impl flux_provider::Provider for ProseProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(
            &self,
            req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            let chunks = if req.tools.iter().any(|tool| tool.name == "declare_intent") {
                vec![
                    flux_core::Chunk::Block(flux_core::ContentBlock::ToolUse {
                        id: "intent".into(),
                        name: "declare_intent".into(),
                        input: serde_json::json!({
                            "intent": "answer the current message",
                            "capability_families": [],
                        }),
                    }),
                    flux_core::Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::ToolUse),
                    },
                ]
            } else {
                vec![
                    flux_core::Chunk::TextDelta("ok".into()),
                    flux_core::Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A minimal `FlowEngine` for the usage-endpoint tests: real machinery, but no turn is ever run
    /// through it — the events store is seeded directly with `CallUsage`/`TurnEnded` events instead,
    /// which is all `get_session_usage`/`get_usage_all` read.
    fn usage_test_engine() -> (Arc<FlowEngine>, Arc<flux_events::EventStore>) {
        test_engine(Arc::new(UnusedProvider))
    }

    /// A `FlowEngine` over `provider` with a fresh in-memory event store (shared out for seeding
    /// and assertions).
    fn test_engine(
        provider: Arc<dyn flux_provider::Provider>,
    ) -> (Arc<FlowEngine>, Arc<flux_events::EventStore>) {
        let dir =
            std::env::temp_dir().join(format!("flux-server-usage-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let mut registry = flux_runtime::ToolRegistry::new();
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = flux_runtime::Executor::new(
            registry,
            flux_runtime::PermissionManager::from_rules(&[], &[]),
            Arc::new(flux_runtime::AllowApprover),
            flux_runtime::ToolContext::new(system),
        );
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let flow = flux_flow::state::FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble(
            provider,
            executor,
            events.clone(),
            flow,
            "claude-sonnet-4-6".into(),
            "test".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            dir,
        )
        .unwrap();
        (Arc::new(engine), events)
    }

    async fn get_json(app: Router, path: &str) -> (StatusCode, Value) {
        let res = app
            .oneshot(HttpRequest::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, body)
    }

    /// C-06 server endpoint: `GET /sessions/{id}/usage` returns cache tiers + cost, and `GET /usage`
    /// rolls the same rows up across sessions — the story's named failing-first test.
    #[tokio::test]
    async fn usage_endpoint_returns_cache_tiers_and_cost() {
        let (engine, events) = usage_test_engine();
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let turn_id = events.begin_turn(&sid, "hi", "claude-sonnet-4-6").unwrap();
        events
            .record_call_usage(
                &sid,
                turn_id,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 1_000_000,
                    output_tokens: 100_000,
                    cache_creation_input_tokens: 200_000,
                    cache_read_input_tokens: 500_000,
                    reasoning_tokens: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        events
            .end_turn(&sid, turn_id, "accepted", 1, "done", None)
            .unwrap();

        let app = router(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let (status, body) = get_json(app.clone(), &format!("/sessions/{sid}/usage")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["session_id"], sid);
        let models = body["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        let row = &models[0];
        assert_eq!(row["model"], "claude-sonnet-4-6");
        assert_eq!(row["usage"]["input"], 1_000_000);
        assert_eq!(row["usage"]["output"], 100_000);
        // The cache tiers `post_message`'s OLD usage JSON used to drop entirely:
        assert_eq!(row["usage"]["cache_creation"], 200_000);
        assert_eq!(row["usage"]["cache_read"], 500_000);
        assert_eq!(row["usage"]["reasoning"], 0);
        // Cost is present and positive (a known model, builtin pricing table).
        assert!(row["cost_usd"].as_f64().unwrap() > 0.0);
        assert_eq!(row["subscription"], false);

        // The aggregate endpoint rolls up the same session.
        let (status2, body2) = get_json(app, "/usage").await;
        assert_eq!(status2, StatusCode::OK);
        let models2 = body2["models"].as_array().unwrap();
        assert_eq!(models2.len(), 1);
        assert_eq!(models2[0]["usage"]["input"], 1_000_000);
    }

    async fn post_json(app: Router, path: &str, body: Value) -> (StatusCode, Value) {
        let res = app
            .oneshot(
                HttpRequest::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, body)
    }

    /// An a2a-tagged D-02 context envelope, as `create_a2a_session` stamps it.
    fn a2a_ctx() -> flux_events::EventContext {
        flux_events::EventContext {
            agent_id: Some(a2a::A2A_AGENT_ID.into()),
            ..Default::default()
        }
    }

    /// C-18: sessions minted by the A2A surface are tagged `agent_id = "a2a"` at creation (with
    /// the request's `contextId` as correlation id), and only THOSE are eligible for TTL pruning
    /// — an untagged CLI/TUI session older than the cutoff survives every sweep.
    #[tokio::test]
    async fn a2a_ttl_prunes_only_expired_a2a_sessions() {
        let (engine, events) = test_engine(Arc::new(ProseProvider));
        // An untagged (CLI-style) session that predates everything — must never be pruned.
        let cli = events.create_session("m").unwrap();

        // Mint the A2A session through the REAL handler (message/send), proving creation-time
        // tagging on the production path.
        let app = router_with_ttl(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(60),
        );
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "message/send",
            "params": { "message": {
                "contextId": "ctx-42",
                "parts": [{ "kind": "text", "text": "hi" }],
            } }
        });
        let (status, _res) = post_json(app, "/a2a", body).await;
        assert_eq!(status, StatusCode::OK);

        let tagged: Vec<_> = events
            .list(10)
            .unwrap()
            .into_iter()
            .filter(|s| s.context.agent_id.as_deref() == Some(a2a::A2A_AGENT_ID))
            .collect();
        assert_eq!(
            tagged.len(),
            1,
            "the A2A handler tags its session at creation"
        );
        assert_eq!(
            tagged[0].context.correlation_id.as_deref(),
            Some("ctx-42"),
            "the request contextId rides the D-02 envelope as the correlation id"
        );
        let a2a_id = tagged[0].id.clone();

        // One TTL (plus ε) after the a2a session's last activity: it has expired; the CLI
        // session is even older, but untagged — never eligible.
        let now = events.info(&a2a_id).unwrap().updated_at_ms + 60_000 + 1;
        assert_eq!(a2a::prune_expired_a2a_sessions_at(&events, 60, now, &[]), 1);
        assert!(
            events.info(&a2a_id).is_err(),
            "expired a2a session is pruned"
        );
        assert!(
            events.info(&cli).is_ok(),
            "a CLI/TUI session is never pruned, whatever its age"
        );
    }

    /// C-18: age is measured from LAST ACTIVITY, not creation — an a2a session created long
    /// before the cutoff but active after it survives the sweep.
    #[tokio::test]
    async fn recently_active_a2a_session_survives_pruning() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let stale = events.create_session_with_context("m", &a2a_ctx()).unwrap();
        let active = events.create_session_with_context("m", &a2a_ctx()).unwrap();
        let active_created = events.info(&active).unwrap().created_at_ms;

        // Both sessions age past the cutoff below… but `active` sees a message afterwards.
        std::thread::sleep(std::time::Duration::from_millis(10));
        events
            .record_message(&active, &flux_core::Message::user_text("still here"))
            .unwrap();

        // Cutoff strictly after both creations, strictly before the touch: prune as of
        // `cutoff + 1s TTL`.
        let cutoff = events
            .info(&stale)
            .unwrap()
            .updated_at_ms
            .max(active_created)
            + 1;
        assert!(
            events.info(&active).unwrap().updated_at_ms > cutoff,
            "sanity: the touch moved last-activity past the cutoff"
        );
        assert_eq!(
            a2a::prune_expired_a2a_sessions_at(&events, 1, cutoff + 1_000, &[]),
            1
        );
        assert!(
            events.info(&stale).is_err(),
            "the inactive session is pruned"
        );
        assert!(
            events.info(&active).is_ok(),
            "created before the cutoff but active after it → survives (age = last activity)"
        );
        assert!(
            active_created < cutoff,
            "sanity: survival is due to activity, not creation time"
        );
    }

    /// C-18: `a2a_session_ttl_secs = 0` disables pruning — even an ancient a2a session survives.
    #[test]
    fn a2a_ttl_zero_disables_pruning() {
        let events = flux_events::EventStore::in_memory().unwrap();
        let old = events.create_session_with_context("m", &a2a_ctx()).unwrap();
        // "Ten years later", with pruning disabled, nothing is swept.
        let far_future = events.info(&old).unwrap().updated_at_ms + 315_360_000_000;
        assert_eq!(
            a2a::prune_expired_a2a_sessions_at(&events, 0, far_future, &[]),
            0
        );
        assert!(events.info(&old).is_ok(), "ttl 0 means never prune");
    }

    // ── Daemon resource limits (C-189) ───────────────────────────────────────

    /// A provider that sleeps before every stream call, then answers like [`ProseProvider`]
    /// (declare_intent, then a one-word turn). Used to drive a handler PAST the request timeout so
    /// the `TimeoutLayer` fires (C-189).
    struct SlowProvider {
        delay: Duration,
    }
    #[async_trait::async_trait]
    impl flux_provider::Provider for SlowProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(
            &self,
            req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            tokio::time::sleep(self.delay).await;
            let chunks = if req.tools.iter().any(|tool| tool.name == "declare_intent") {
                vec![
                    flux_core::Chunk::Block(flux_core::ContentBlock::ToolUse {
                        id: "intent".into(),
                        name: "declare_intent".into(),
                        input: serde_json::json!({
                            "intent": "answer the current message",
                            "capability_families": [],
                        }),
                    }),
                    flux_core::Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::ToolUse),
                    },
                ]
            } else {
                vec![
                    flux_core::Chunk::TextDelta("ok".into()),
                    flux_core::Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// C-189 failing-first: a request body over the configured cap is rejected with `413 Payload
    /// Too Large` during extraction — before the handler runs. Pre-change (no `DefaultBodyLimit`,
    /// axum's implicit 2 MiB default) an 8 KiB body is not rejected for size; with a 1 KiB cap it
    /// is. `/a2a` buffers the body via `Json<JsonRpcRequest>`, so the reject precedes any dispatch.
    #[tokio::test]
    async fn body_over_limit_is_rejected_with_413() {
        let (engine, _events) = usage_test_engine();
        let limits = ServerLimits {
            max_body_bytes: 1024,
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let oversized = "x".repeat(8 * 1024);
        let res = app
            .oneshot(
                HttpRequest::post("/a2a")
                    .header("content-type", "application/json")
                    .body(Body::from(oversized))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// C-189 failing-first: a handler that outlives the request timeout yields `408 Request
    /// Timeout` instead of hanging. A 50 ms timeout against a provider that sleeps 500 ms per call
    /// fires long before the turn could finish. Pre-change (no `TimeoutLayer`) the request runs the
    /// turn to completion and returns `200`, never `408`.
    #[tokio::test]
    async fn slow_handler_times_out_with_408() {
        let (engine, events) = test_engine(Arc::new(SlowProvider {
            delay: Duration::from_millis(500),
        }));
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let limits = ServerLimits {
            request_timeout: Duration::from_millis(50),
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let res = app
            .oneshot(
                HttpRequest::post(format!("/sessions/{sid}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "input": "hi" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT);
    }

    /// C-189: the SSE stream route is EXEMPT from the request timeout — a long-lived stream is not
    /// a stuck request. Even with a 50 ms timeout and a provider that sleeps 500 ms, the stream is
    /// established (`200`) rather than severed with `408`: the handler returns its `Sse` response
    /// promptly and the turn streams behind it. (This confirms the exemption's intent; the layer
    /// would not fire on this fast-returning handler even if applied — see [`router_with_ttl`].)
    #[tokio::test]
    async fn sse_stream_route_is_exempt_from_the_request_timeout() {
        let (engine, events) = test_engine(Arc::new(SlowProvider {
            delay: Duration::from_millis(500),
        }));
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let limits = ServerLimits {
            request_timeout: Duration::from_millis(50),
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let res = app
            .oneshot(
                HttpRequest::get(format!("/sessions/{sid}/stream?input=hi"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    // ── Non-loopback auth by construction (C-190) ────────────────────────────

    /// C-190 failing-first: the invariant that an unauthenticated (`Open`) server may not be exposed
    /// on a non-loopback address must hold at ROUTER CONSTRUCTION — not only inside `serve_on`. A
    /// caller that mounts the real router and serves it itself (the `a2a` channel does exactly this,
    /// via `flux_server::router` + `axum::serve`) must not be able to stand up an open router bound
    /// to a routable address. This test drives the REAL construction path, not a hand-built
    /// `guarded_app`, so it pins the construction-time guarantee the C-189 review flagged as untested.
    ///
    /// Pre-change, `router` took no bind address and always built a fully-permissive open router; a
    /// direct mounter reached every protected route unauthenticated on any interface. After: `router`
    /// refuses the unauthenticated + non-loopback combination, so the open router cannot be built for
    /// a routable bind at all.
    #[tokio::test]
    async fn unauthenticated_non_loopback_router_is_refused_at_construction() {
        let (engine, _events) = usage_test_engine();

        // Unauthenticated + non-loopback: refused when the router is built.
        let refused = router(
            engine.clone(),
            ServerAuth::Open,
            CardInfo::flux_coding(),
            "0.0.0.0:8080".parse().unwrap(),
        );
        assert!(
            refused.is_err(),
            "Open + non-loopback must be refused at router construction, not only in serve_on"
        );

        // Authenticated + non-loopback is fine — the refusal is specifically the UNAUTHENTICATED
        // case (a shared secret makes a routable bind safe).
        assert!(
            router(
                engine.clone(),
                ServerAuth::from_token(Some("s3cr3t".to_string())),
                CardInfo::flux_coding(),
                "0.0.0.0:8080".parse().unwrap(),
            )
            .is_ok(),
            "an authenticated non-loopback router still builds"
        );

        // Open + loopback is the dev path — it builds, and (being open) serves a protected route
        // without a token, which is exactly why the non-loopback refusal above matters.
        let app = router(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .expect("an open loopback router builds");
        let res = app
            .oneshot(HttpRequest::post("/sessions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "an open loopback router reaches its protected routes without auth"
        );
    }
}

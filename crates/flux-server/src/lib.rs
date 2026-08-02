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
//! - `GET  /approvals`                     → effects parked awaiting a human decision (C-453)
//! - `POST /approvals/{id}`                → deliver one decision
//!
//! The agent runs tools through the same safety envelope as the CLI. **Which approval posture it
//! runs under is the caller's choice**, and this crate serves whichever one was picked: build the
//! engine with `AllowApprover` for an unattended agent constrained by policy + sandbox + budget, or
//! with `flux_runtime::RemoteApprover` and pass its queue to [`serve_with_approvals`] /
//! [`router_with_approvals_in`] so a human answers each effect over the `/approvals` routes.
//! Before C-453 only the first was reachable here — see [`ApprovalGate`].

mod a2a;
pub mod public_docs;
mod resource;
pub mod system;

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

/// The value-held `HOME` every config read in this crate resolves its **user** layer through
/// (C-297/C-332), re-exported so a caller of [`router_in`]/[`router_multi_in`] can name the
/// parameter without depending on `flux-runtime` directly.
pub use flux_runtime::metadata::DiscoveryEnv;

use resource::{ResourceGovernor, WorkPermit};

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
    /// is no escape hatch (front it with an authenticating proxy instead). The refusal keys on the
    /// *property* rather than on this variant — see [`ServerAuth::is_effectively_open`], because a
    /// `SharedSecret` with an empty secret is `Open` under a different name (C-321).
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
    ///
    /// `Some("")` deliberately still yields `SharedSecret { secret: "" }` rather than being
    /// normalised to `Open` or rejected here: silently rewriting an operator's configuration is how
    /// the empty secret stayed invisible in the first place, and an infallible constructor cannot
    /// report the config error that would justify the rewrite. It is instead refused where it can
    /// do damage —
    /// [`guard_open_bind`] will not let it reach a non-loopback bind, and [`require_auth`] will not
    /// let it authenticate anything. Producers that can report a config error should refuse it at
    /// load as well (the CLI's `FLUX_SERVER_TOKEN` filter, the `a2a` adapter's).
    pub fn shared_secret(token: Option<String>, external_url: Option<String>) -> Self {
        match token {
            Some(secret) => ServerAuth::SharedSecret {
                secret,
                external_url,
            },
            None => ServerAuth::Open,
        }
    }

    /// **Is this mode unauthenticated in effect, whatever it is called?** `true` for [`Open`], and
    /// `true` for a [`SharedSecret`] whose secret is empty.
    ///
    /// The second case is the whole point (C-321). A request carrying no `Authorization` header
    /// presents `""`, and `constant_time_eq(b"", b"")` is `true` — so an empty expected secret
    /// admits every anonymous caller, exactly as `Open` does, while reading everywhere it is printed
    /// or logged as "shared-secret". A guard that pattern-matches the `Open` *variant* therefore
    /// does not capture the property it means, and an empty secret walks past it; this predicate is
    /// that property stated once, so the guard cannot drift from it again.
    ///
    /// Emptiness, not blankness: `" "` is a bad secret, but a request must still present it to
    /// authenticate, so it is not this bypass and is not silently reclassified here. Producers that
    /// can fail at load reject whitespace-only tokens up front, where a config error is actionable.
    ///
    /// [`Open`]: ServerAuth::Open
    /// [`SharedSecret`]: ServerAuth::SharedSecret
    fn is_effectively_open(&self) -> bool {
        match self {
            ServerAuth::Open => true,
            ServerAuth::SharedSecret { secret, .. } => secret.is_empty(),
            ServerAuth::Principal(_) => false,
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
    permit: &mut WorkPermit,
) -> flux_core::Result<()> {
    let events = engine.events.clone();
    let session = session_id.to_string();
    let mut turn_started = move |turn_id| permit.track_turn(events.clone(), &session, turn_id);
    match &turn.identity {
        Some(identity) => {
            engine
                .run_turn_cancellable_as_observed(
                    session_id,
                    input,
                    sink,
                    cancel,
                    identity.clone(),
                    &mut turn_started,
                )
                .await
        }
        None => {
            engine
                .run_turn_cancellable_observed(session_id, input, sink, cancel, &mut turn_started)
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
    resources: Arc<ResourceGovernor>,
    /// The approval queue the served engine parks on, when the operator chose the remote-approval
    /// posture (C-453). `None` for every other posture.
    approvals: ApprovalGate,
}

/// The served agent's approval queue, or its absence — the router-side half of the remote-approval
/// posture (C-453).
///
/// ⚠ **Read this if you operate a served agent.** The envelope is *authorization → approval →
/// guarded IO*, and approval is the only stage with a human in it. Which posture that stage runs
/// under is a legitimate choice — prompt per effect, or do not prompt and constrain through policy,
/// sandbox and budget instead (the right design for high-autonomy work, and why flux raises
/// unattended surfaces to the fail-closed `require` sandbox profile). What was missing before C-453
/// is that a served agent could not *make* that choice: every approver in the tree was local, so
/// the served surface had `AllowApprover` or `DenyApprover` and nothing with a human in it. This
/// type is what makes the first posture reachable here.
///
/// A `None` gate is not a failure mode: it is every other posture, and the `/approvals` routes say
/// so rather than pretending to be a control that nobody is behind.
#[derive(Clone, Default)]
pub struct ApprovalGate(Option<Arc<flux_runtime::ApprovalQueue>>);

impl ApprovalGate {
    /// No remote-approval posture on this router — the engine's own approver decides alone.
    pub fn none() -> Self {
        Self(None)
    }

    /// Serve `queue`. It must be the very queue the engine's `RemoteApprover` parks on: a router
    /// holding a *different* queue lists nothing and denies nothing — every effect would sit
    /// unanswered until it timed out, which is safe but useless.
    pub fn serving(queue: Arc<flux_runtime::ApprovalQueue>) -> Self {
        Self(Some(queue))
    }
}

impl From<Option<Arc<flux_runtime::ApprovalQueue>>> for ApprovalGate {
    fn from(queue: Option<Arc<flux_runtime::ApprovalQueue>>) -> Self {
        Self(queue)
    }
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

impl FromRef<ServerState> for Arc<ResourceGovernor> {
    fn from_ref(s: &ServerState) -> Self {
        s.resources.clone()
    }
}

impl FromRef<ServerState> for ApprovalGate {
    fn from_ref(s: &ServerState) -> Self {
        s.approvals.clone()
    }
}

/// Bind `addr` and serve until shutdown, authenticating per `auth` (see [`ServerAuth`] for the
/// three modes). [`ServerAuth::Open`] requires a loopback bind.
///
/// The readiness line is rendered by [`flux_core::readiness::serving_announcement`] rather than
/// spelled here: `flux-orchestrate`'s `fleet.start` matches it to decide a fleet worker is live,
/// and being three layers below this crate it cannot import it to check the wording agrees (C-277).
pub async fn serve(addr: &str, agent: FlowEngine, auth: ServerAuth) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    eprintln!(
        "{}",
        flux_core::readiness::serving_announcement(&addr.to_string())
    );
    eprintln!("  A2A agent card:  http://{addr}/.well-known/agent-card.json");
    eprintln!("  A2A endpoint:    http://{addr}/a2a  (message/send, message/stream)");
    serve_on(listener, agent, auth).await
}

/// [`serve`] for an agent running the **remote-approval** posture (C-453): `approvals` is the queue
/// the engine's `flux_runtime::RemoteApprover` parks on, and the `/approvals` routes serve it.
///
/// Passing [`ApprovalGate::none()`] is exactly [`serve`] — every other posture.
pub async fn serve_with_approvals(
    addr: &str,
    agent: FlowEngine,
    auth: ServerAuth,
    approvals: ApprovalGate,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    eprintln!(
        "{}",
        flux_core::readiness::serving_announcement(&addr.to_string())
    );
    eprintln!("  A2A agent card:  http://{addr}/.well-known/agent-card.json");
    eprintln!("  A2A endpoint:    http://{addr}/a2a  (message/send, message/stream)");
    if approvals.0.is_some() {
        eprintln!("  Approvals:       http://{addr}/approvals  (GET to list, POST /approvals/{{id}} to decide)");
    }
    serve_on_with_approvals(listener, agent, auth, approvals).await
}

/// Serve on an already-bound listener (lets callers pick an ephemeral port).
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    agent: FlowEngine,
    auth: ServerAuth,
) -> anyhow::Result<()> {
    serve_on_with_approvals(listener, agent, auth, ApprovalGate::none()).await
}

/// [`serve_on`] carrying an [`ApprovalGate`] — the one place both forms converge, so the
/// non-loopback refusal and the route set cannot differ between postures.
pub async fn serve_on_with_approvals(
    listener: tokio::net::TcpListener,
    agent: FlowEngine,
    auth: ServerAuth,
    approvals: ApprovalGate,
) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    // The non-loopback refusal now lives in `router` (construction-time, C-190); `serve_on` simply
    // propagates it, so there is one enforcement point every caller shares.
    let router = router_with_approvals(
        Arc::new(agent),
        auth,
        CardInfo::flux_coding(),
        addr,
        approvals,
    )?;
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
/// construction instead of being silently responsible for re-deriving it. An unauthenticated
/// non-loopback bind is remote code execution against the auto-approving daemon, so it is refused
/// outright — there is deliberately no escape hatch. A deployment that must face the network fronts
/// the loopback daemon with an authenticating reverse proxy (or configures shared-secret/principal
/// auth), it does not open the listener itself.
///
/// "Unauthenticated" is [`ServerAuth::is_effectively_open`], **not** `matches!(auth, Open)`. That
/// distinction is C-321: this guard used to key on the variant, and a `SharedSecret` carrying an
/// empty secret — which authenticates every anonymous request — is not that variant, so it walked
/// straight past a comment promising no escape hatch and bound `0.0.0.0`. Keying on the property
/// means a future mode that is open in effect is refused by this guard without anyone having to
/// remember to extend a `matches!`.
///
/// [`require_auth`] independently refuses to authenticate against an empty secret, so neither half
/// depends on the other being right. This is the half that runs **before a port is served**, which
/// is the only half that can prevent the exposure rather than survive it.
fn guard_open_bind(auth: &ServerAuth, addr: SocketAddr) -> anyhow::Result<()> {
    if auth.is_effectively_open() && !unauthenticated_bind_allowed(addr) {
        // Two different operator mistakes, two different fixes: "you configured nothing" vs "you
        // configured a token that is the empty string" (`token secret "K"` with `K` exported empty
        // resolves to exactly that, and `std::env::var` does not filter it).
        if matches!(auth, ServerAuth::SharedSecret { .. }) {
            anyhow::bail!(
                "refusing to build a router for non-loopback bind {addr}: the shared secret is \
                 empty, which authenticates every request — including one carrying no \
                 `Authorization` header at all. Give it a value (a `secret \"KEY\"` reference \
                 resolves to an empty string when `KEY` is exported empty), or bind to \
                 127.0.0.1/::1"
            );
        }
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

/// Default number of authenticated protected-route requests admitted per resource bucket/minute.
pub const DEFAULT_REQUESTS_PER_MINUTE: u32 = 120;
/// Default live turns per authenticated principal/shared realm/open-loopback bucket.
pub const DEFAULT_MAX_INFLIGHT_PER_KEY: usize = 4;
/// Default completed provider-call circuit-breaker threshold per bucket/24-hour process window.
pub const DEFAULT_PROVIDER_CALLS_PER_DAY: u64 = 1_000;
/// Default completed priced-spend circuit-breaker threshold per bucket/24-hour process window.
pub const DEFAULT_PROVIDER_SPEND_USD_PER_DAY: f64 = 25.0;
/// Maximum number of principal/realm buckets retained by one router.
pub const DEFAULT_MAX_RESOURCE_KEYS: usize = 4_096;

/// The daemon's resource limits (C-189/C-261): body and response-production bounds apply across
/// the router; authenticated request rate, live work, and completed provider usage are keyed by
/// principal/auth realm. Call/spend thresholds are retrospective circuit breakers, not prepaid
/// reservations: newly admitted work stops after completed usage reaches a threshold, while work
/// already admitted may overshoot it within the concurrency bound.
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
    /// Authenticated protected-route requests admitted per
    /// [`request_rate_window`](Self::request_rate_window), including reads.
    pub requests_per_window: u32,
    /// Fixed request-rate accounting window.
    pub request_rate_window: Duration,
    /// Live work admitted per principal/realm bucket across REST, webhook, and A2A.
    pub max_inflight_per_key: usize,
    /// Completed provider calls observed before new work is rejected for the remainder of the
    /// [`provider_budget_window`](Self::provider_budget_window).
    pub provider_calls_per_window: u64,
    /// Completed priced provider spend observed before new work is rejected for the remainder of
    /// the provider-budget window.
    pub provider_spend_usd_per_window: f64,
    /// Fixed call/spend accounting window.
    pub provider_budget_window: Duration,
    /// Cardinality cap for in-process principal/realm limit state.
    pub max_resource_keys: usize,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            requests_per_window: DEFAULT_REQUESTS_PER_MINUTE,
            request_rate_window: Duration::from_secs(60),
            max_inflight_per_key: DEFAULT_MAX_INFLIGHT_PER_KEY,
            provider_calls_per_window: DEFAULT_PROVIDER_CALLS_PER_DAY,
            provider_spend_usd_per_window: DEFAULT_PROVIDER_SPEND_USD_PER_DAY,
            provider_budget_window: Duration::from_secs(24 * 60 * 60),
            max_resource_keys: DEFAULT_MAX_RESOURCE_KEYS,
        }
    }
}

impl ServerLimits {
    /// The limits in force, reading env overrides once (mirrors [`max_inflight_per_realm`]):
    /// `FLUX_SERVER_MAX_BODY_BYTES` (positive integer bytes) and `FLUX_SERVER_REQUEST_TIMEOUT_SECS`
    /// (positive integer seconds; `0`/missing/unparseable falls back to the documented default —
    /// `0` is never read as "disable", so the daemon is never accidentally left unbounded).
    ///
    /// The layered flux config supplies the fallbacks the env knobs do not set, and its **user**
    /// layer is `<env home>/.flux/config.toml` — hence the explicit [`DiscoveryEnv`] rather than a
    /// `std::env` read (C-392; the seam is C-332's `load_config_in`).
    fn from_env_in(env: &DiscoveryEnv) -> Self {
        let d = Self::default();
        let config = std::env::current_dir()
            .ok()
            .and_then(|cwd| flux_runtime::metadata::load_config_in(&cwd, env).ok());
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
        let requests_per_window = positive_env("FLUX_SERVER_REQUESTS_PER_MINUTE")
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| {
                config
                    .as_ref()?
                    .server
                    .requests_per_minute
                    .filter(|n| *n > 0)
            })
            .unwrap_or(d.requests_per_window);
        let max_inflight_per_key = positive_env("FLUX_SERVER_MAX_INFLIGHT_PER_PRINCIPAL")
            .and_then(|n| usize::try_from(n).ok())
            .or_else(|| {
                config
                    .as_ref()?
                    .server
                    .max_inflight_per_principal
                    .filter(|n| *n > 0)
            })
            .unwrap_or(d.max_inflight_per_key);
        let provider_calls_per_window = positive_env("FLUX_SERVER_PROVIDER_CALLS_PER_DAY")
            .or_else(|| {
                config
                    .as_ref()?
                    .server
                    .provider_calls_per_day
                    .filter(|n| *n > 0)
            })
            .unwrap_or(d.provider_calls_per_window);
        let provider_spend_usd_per_window = std::env::var("FLUX_SERVER_PROVIDER_SPEND_USD_PER_DAY")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| {
                config
                    .as_ref()?
                    .server
                    .provider_spend_usd_per_day
                    .filter(|v| v.is_finite() && *v > 0.0)
            })
            .unwrap_or(d.provider_spend_usd_per_window);
        Self {
            max_body_bytes,
            request_timeout,
            requests_per_window,
            max_inflight_per_key,
            provider_calls_per_window,
            provider_spend_usd_per_window,
            ..d
        }
    }
}

fn positive_env(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
}

/// The request `TimeoutLayer` in force. `TimeoutLayer::with_status_code` (not the deprecated
/// `::new`) so the timeout answers a real `408 Request Timeout`.
fn request_timeout_layer(limits: ServerLimits) -> TimeoutLayer {
    TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, limits.request_timeout)
}

/// Request-owned cancellation installed on protected non-streaming work. When the deadline wins,
/// the middleware cancels this token and waits for the handler to finalize before returning 408.
#[derive(Clone)]
pub(crate) struct RequestCancellation(tokio_util::sync::CancellationToken);

async fn cancellable_request_timeout(
    State(timeout): State<Duration>,
    mut request: Request,
    next: Next,
) -> Response {
    let cancel = tokio_util::sync::CancellationToken::new();
    request
        .extensions_mut()
        .insert(RequestCancellation(cancel.clone()));
    let response = next.run(request);
    tokio::pin!(response);
    tokio::select! {
        result = &mut response => result,
        _ = tokio::time::sleep(timeout) => {
            cancel.cancel();
            // The engine cancellation path closes the durable turn and shuts down child work.
            // Await it before returning so middleware never drops the future/permit half-finalized.
            let _ = response.await;
            StatusCode::REQUEST_TIMEOUT.into_response()
        }
    }
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
    router_in(engine, auth, card, bind, &DiscoveryEnv::from_process())
}

/// [`router`] serving an [`ApprovalGate`] — the remote-approval posture (C-453).
pub fn router_with_approvals(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    bind: SocketAddr,
    approvals: ApprovalGate,
) -> anyhow::Result<Router> {
    router_with_approvals_in(
        engine,
        auth,
        card,
        bind,
        &DiscoveryEnv::from_process(),
        approvals,
    )
}

/// [`router_in`] serving an [`ApprovalGate`] — the remote-approval posture (C-453). This is the
/// form tests use, since it pins the config read to an explicit [`DiscoveryEnv`] (C-392).
pub fn router_with_approvals_in(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    bind: SocketAddr,
    env: &DiscoveryEnv,
    approvals: ApprovalGate,
) -> anyhow::Result<Router> {
    guard_open_bind(&auth, bind)?;
    guard_approval_auth(&auth, &approvals)?;
    Ok(router_with_ttl_in(
        engine,
        auth,
        card,
        a2a_ttl_from_config_in(env),
        env,
        approvals,
    ))
}

/// A principal-authenticated server has many mutually isolated callers, while C-453's approval
/// queue is deliberately one operator queue for one served agent. Combining them would let any
/// authenticated principal list and answer every other principal's effects. Refuse that topology
/// until the protocol carries a separately authorized supervisor identity.
fn guard_approval_auth(auth: &ServerAuth, approvals: &ApprovalGate) -> anyhow::Result<()> {
    if approvals.0.is_some() && matches!(auth, ServerAuth::Principal(_)) {
        anyhow::bail!(
            "remote approval cannot be combined with principal authentication: the approval queue \
             is deployment-wide, so one authenticated principal could list and answer another's \
             effects. Use a shared operator token for this single-agent posture; principal mode \
             needs a separately authorized supervisor identity before it can serve approvals"
        );
    }
    Ok(())
}

/// [`router`] against an explicit [`DiscoveryEnv`] rather than the process's own (C-392).
///
/// Router construction resolves two things from the layered flux config — the A2A session TTL
/// (`[server] a2a_session_ttl_secs`) and the resource limits (`[server] requests_per_minute`,
/// `max_inflight_per_principal`, …) — and that config's **user** layer is
/// `<env home>/.flux/config.toml`. Without this seam every test that builds a router inherits
/// whatever the operator happens to keep in their own `~/.flux/config.toml`, so the verdict is a
/// function of the machine rather than of the fixture, and the resulting failure looks exactly like
/// a real regression in whatever diff is in flight. Tests pass [`DiscoveryEnv::empty`]; production
/// goes through [`router`], which passes [`DiscoveryEnv::from_process`].
///
/// This is the same seam, and the same idiom, as `flux_runtime::metadata::load_config_in` (C-332)
/// and `DiscoveryEnv` itself (C-297) — a value-held env, not a third injection style.
///
/// It refuses [`ServerAuth::Open`] on a non-loopback `bind` identically to [`router`]: the guard
/// runs here, and [`router`] reaches it by delegation, so there is exactly one enforcement point
/// and the safety invariant cannot be threaded around by picking the other entry point.
pub fn router_in(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    bind: SocketAddr,
    env: &DiscoveryEnv,
) -> anyhow::Result<Router> {
    guard_open_bind(&auth, bind)?;
    Ok(router_with_ttl_in(
        engine,
        auth,
        card,
        a2a_ttl_from_config_in(env),
        env,
        ApprovalGate::none(),
    ))
}

/// Resolve the A2A session TTL from the layered flux config (`[server] a2a_session_ttl_secs`,
/// project over user, default 1h, `0` = never prune). Resolved at router build — so every
/// mount of the router (the standalone server and the `a2a` channel) gets the same retention
/// behavior without each caller plumbing the knob. A malformed config file falls back to the
/// default with a warning rather than failing the surface (the CLI already fails loudly on it).
///
/// The user layer is `<env home>/.flux/config.toml`, which is why this takes a [`DiscoveryEnv`]
/// (C-392) rather than reading process `HOME` through `load_config`.
fn a2a_ttl_from_config_in(env: &DiscoveryEnv) -> A2aTtl {
    let ttl = std::env::current_dir()
        .ok()
        .and_then(
            |cwd| match flux_runtime::metadata::load_config_in(&cwd, env) {
                Ok(cfg) => Some(cfg.a2a_session_ttl_secs()),
                Err(e) => {
                    eprintln!("(ignoring malformed flux config for the A2A session TTL: {e})");
                    None
                }
            },
        )
        .unwrap_or(flux_config::DEFAULT_A2A_SESSION_TTL_SECS);
    A2aTtl(ttl)
}

/// [`router_in`] with an explicit A2A session TTL (tests inject one; production resolves from
/// config). `env` still reaches [`ServerLimits::from_env_in`], so a TTL-injecting test is not
/// silently left reading the operator's home for its *limits*.
fn router_with_ttl_in(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    a2a_ttl: A2aTtl,
    env: &DiscoveryEnv,
    approvals: ApprovalGate,
) -> Router {
    router_with_ttl_limits_and_approvals(
        engine,
        auth,
        card,
        a2a_ttl,
        ServerLimits::from_env_in(env),
        approvals,
    )
}

/// [`router_with_ttl_in`] with explicit resource limits (C-189) and the approval posture (C-453).
/// Tests inject tiny limits to exercise the `413`/`408` paths; production reads them from the
/// environment once at build time. The single construction point for [`ServerState`], so every
/// entry point above agrees on the route set.
fn router_with_ttl_limits_and_approvals(
    engine: Arc<FlowEngine>,
    auth: ServerAuth,
    card: CardInfo,
    a2a_ttl: A2aTtl,
    limits: ServerLimits,
    approvals: ApprovalGate,
) -> Router {
    let auth = Arc::new(auth);
    let resources = Arc::new(ResourceGovernor::new(limits));
    let state = ServerState {
        engine,
        card: Arc::new(card),
        turn_gate: Arc::new(tokio::sync::Mutex::new(())),
        a2a_ttl,
        auth: auth.clone(),
        tasks: Arc::new(a2a::TaskRegistry::default()),
        resources,
        approvals,
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

    // Non-streaming protected routes + the REST session subtree carry a cancellation-aware request
    // deadline: a wedged handler yields `408`, but only after its owning turn and children finalize.
    // `/a2a` belongs here because blocking `message/send` runs a full turn in-handler. Its
    // `message/stream` path returns the response promptly, so body streaming is not severed.
    // The approval routes belong here — inside `protected`, so they inherit `require_auth` by
    // construction. ⚠ In the supported shared-token/open-loopback modes, who may answer an
    // approval is exactly who the server admits; principal mode plus a global queue is refused at
    // construction. A decision endpoint outside the auth layer would let anyone who can reach the
    // port approve the agent's effects, which is strictly worse than having no approval stage.
    // They are mounted
    // unconditionally (rather than only when a queue exists) so a client can tell "this server does
    // not offer remote approval" from "that request is gone" — see [`list_approvals`].
    let timed = Router::new()
        .route("/a2a", post(a2a::a2a_handler))
        .route("/sessions", post(create_session))
        .route("/usage", get(get_usage_all))
        .route("/webhook", post(webhook))
        .route("/approvals", get(list_approvals))
        .route("/approvals/{id}", post(decide_approval))
        .merge(sessions_rest)
        .layer(middleware::from_fn_with_state(
            limits.request_timeout,
            cancellable_request_timeout,
        ));

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

    // `require_auth` is outermost and runs before request-rate admission, which in turn runs before
    // `realm_guard`. Authentication therefore precedes both accounting and any existence signal
    // (A2A §13.1), while every protected route is rate-limited by construction.
    let protected = timed
        .merge(sessions_stream)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            request_rate_guard,
        ))
        .route_layer(middleware::from_fn_with_state(auth, require_auth));

    // DefaultBodyLimit over the whole surface (C-189): a body over the cap is rejected with `413`
    // during extraction, before any handler runs. Applied outermost so every route — exempt,
    // timed, and streaming — is covered by construction, and no future route can forget it.
    exempt
        .merge(protected)
        .layer(DefaultBodyLimit::max(limits.max_body_bytes))
        .with_state(state)
}

// ── Remote approval (C-453) ─────────────────────────────────────────────────────

/// `GET /approvals` — the effects currently parked awaiting a human decision, oldest first.
///
/// Returns `501` when this server is not running the remote-approval posture.
///
/// That is a *statement of posture*, not an error: an operator can be running the agent under
/// `AllowApprover` with policy, sandbox and budget doing the constraining, which is a legitimate
/// and often correct choice. What
/// this route must never do is return an empty list in that case — "nothing is waiting" and "nobody
/// is ever asked" look identical to a client, and only one of them means a human is in the loop.
async fn list_approvals(State(gate): State<ApprovalGate>) -> Response {
    let Some(queue) = gate.0 else {
        return no_remote_approval_posture();
    };
    Json(json!({
        "approvals": queue.pending(),
        "timeout_secs": queue.timeout().as_secs(),
    }))
    .into_response()
}

/// The body of `POST /approvals/{id}`.
#[derive(serde::Deserialize)]
struct ApprovalDecisionBody {
    /// The parked request's `fingerprint`, echoed back verbatim. ⚠ Required, and required to
    /// match: it is what binds this decision to the effect the human was actually shown. Without
    /// it the endpoint would mean "the client said yes", and a `yes` for a benign effect would be
    /// deliverable against a destructive one.
    fingerprint: String,
    /// `"allow"` or `"deny"`. Anything else is a `400` — an unrecognised decision must never fall
    /// through to allow, and must not be silently read as a denial either, because that would hide
    /// a client bug behind a plausible-looking outcome.
    decision: String,
    /// Optional rationale, carried to the model on a denial (C-113's `DenyWithReason`).
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /approvals/{id}` — deliver one human decision for one specific parked effect.
///
/// Refusals are distinguished, because an operator responds to them differently:
/// `404` the request is gone (answered already, timed out, or its turn ended) · `409` the decision
/// named a different effect than the request · `410` the run waiting on it disappeared · `400` the
/// decision word was not `allow`/`deny`. In every one of those, **nothing was approved**, and the
/// parked request — if it is still parked — will time out into a denial on its own.
async fn decide_approval(
    State(gate): State<ApprovalGate>,
    Path(id): Path<String>,
    Json(body): Json<ApprovalDecisionBody>,
) -> Response {
    let Some(queue) = gate.0 else {
        return no_remote_approval_posture();
    };
    // Exhaustive by intent: the fallthrough is a refusal, never a decision.
    let choice = match body.decision.as_str() {
        "allow" => flux_runtime::ApprovalChoice::Allow,
        "deny" => match body.reason {
            Some(why) => flux_runtime::ApprovalChoice::DenyWithReason(why),
            None => flux_runtime::ApprovalChoice::Deny,
        },
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!(
                        "unknown decision {other:?} — use \"allow\" or \"deny\". Nothing was \
                         approved; this request is still awaiting a decision and will be denied if \
                         none arrives."
                    ),
                })),
            )
                .into_response();
        }
    };
    match queue.decide(&id, &body.fingerprint, choice) {
        Ok(()) => Json(json!({ "status": "recorded", "id": id, "decision": body.decision }))
            .into_response(),
        Err(e) => {
            let status = match e {
                flux_runtime::DecideError::UnknownRequest => StatusCode::NOT_FOUND,
                flux_runtime::DecideError::EffectMismatch => StatusCode::CONFLICT,
                flux_runtime::DecideError::Abandoned => StatusCode::GONE,
            };
            (status, Json(json!({ "error": e.to_string() }))).into_response()
        }
    }
}

/// The honest answer when this server runs some other approval posture.
fn no_remote_approval_posture() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "this server is not running the remote-approval posture — no effect on it is \
                      ever parked for a human decision. Its agent was built with a headless \
                      approver (constrained instead by authorization policy, the sandbox floor and \
                      resource budgets). To be asked per effect, start it with the remote-approval \
                      posture.",
        })),
    )
        .into_response()
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
    pub(crate) resources: Arc<ResourceGovernor>,
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
impl FromRef<MultiState> for Arc<ResourceGovernor> {
    fn from_ref(s: &MultiState) -> Self {
        s.resources.clone()
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
    router_multi_in(resolver, auth, bind, &DiscoveryEnv::from_process())
}

/// [`router_multi`] against an explicit [`DiscoveryEnv`] rather than the process's own — the same
/// seam, and for the same reason, as [`router_in`] (C-392). It refuses an [`ServerAuth::Open`]
/// non-loopback bind identically, and [`router_multi`] reaches that refusal by delegating here.
pub fn router_multi_in(
    resolver: Arc<dyn AgentResolver>,
    auth: ServerAuth,
    bind: SocketAddr,
    env: &DiscoveryEnv,
) -> anyhow::Result<Router> {
    guard_open_bind(&auth, bind)?;
    Ok(router_multi_with_ttl_in(
        resolver,
        auth,
        a2a_ttl_from_config_in(env),
        env,
    ))
}

fn router_multi_with_ttl_in(
    resolver: Arc<dyn AgentResolver>,
    auth: ServerAuth,
    a2a_ttl: A2aTtl,
    env: &DiscoveryEnv,
) -> Router {
    router_multi_with_ttl_and_limits(resolver, auth, a2a_ttl, ServerLimits::from_env_in(env))
}

/// [`router_multi_with_ttl_in`] with explicit resource limits (C-189).
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
        resources: Arc::new(ResourceGovernor::new(limits)),
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
    // The mount's one work route carries the cancellation-aware deadline: blocking `message/send`
    // is cancelled and finalized before 408, while promptly produced `message/stream` SSE continues
    // for the life of the turn. There is no separate SSE-only route to exempt here.
    let protected = Router::new()
        .route("/{agent_id}/a2a", post(a2a::a2a_handler_multi))
        .layer(middleware::from_fn_with_state(
            limits.request_timeout,
            cancellable_request_timeout,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            request_rate_guard,
        ))
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
///   the RFC 7235-required `WWW-Authenticate` challenge on 401). An **empty** expected secret
///   authenticates nothing at all rather than everything (C-321 — see the inline note).
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
            // **An empty expected secret authenticates nothing** (C-321). A request with no
            // `Authorization` header presents `""`, and a constant-time compare of two empty byte
            // strings is `true` — so without this line an empty secret would admit every anonymous
            // caller. [`guard_open_bind`] already refuses to build such a router for a non-loopback
            // bind; this is the same rule stated where the comparison happens, so a loopback
            // deployment (which that guard admits by design) still cannot be mistaken for
            // authenticated, and a future path reaching this middleware without that guard cannot
            // reopen the hole.
            if secret.is_empty() {
                return unauthorized();
            }
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

/// Count every request that crossed the authentication boundary before route-specific work. The
/// public health and discovery routers never carry this layer. Principal authentication remains
/// outside the in-process limiter because the principal itself selects the budget key; deployments
/// must bound authentication/introspection traffic at their reverse proxy or identity provider.
async fn request_rate_guard(
    State(auth): State<Arc<ServerAuth>>,
    State(resources): State<Arc<ResourceGovernor>>,
    ctx: Option<Extension<AuthContext>>,
    req: Request,
    next: Next,
) -> Response {
    match resources.admit_request(&auth, ctx.as_ref().map(|extension| &extension.0)) {
        Ok(()) => next.run(req).await,
        Err(response) => *response,
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
    State(resources): State<Arc<ResourceGovernor>>,
    ctx: Option<Extension<AuthContext>>,
    request_cancel: Option<Extension<RequestCancellation>>,
    Path(id): Path<String>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, Response> {
    let mut permit = resources
        .admit_work(&auth, ctx.as_ref().map(|e| &e.0))
        .map_err(|e| *e)?;
    let mut sink = Collect::default();
    let turn = server_turn_context(&auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    let cancel = request_cancel
        .map(|extension| extension.0 .0)
        .unwrap_or_default();
    run_server_turn(
        &agent,
        &turn,
        &id,
        &req.input,
        &mut sink,
        &cancel,
        &mut permit,
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

/// A REST stream may queue at most this many events. [`SseSink`] cancels the owning turn when a
/// synchronous delta cannot enter the buffer, so a stalled client cannot turn tokens into
/// unbounded resident memory.
const REST_SSE_CHANNEL_CAPACITY: usize = 256;

/// `GET /sessions/{id}/stream?input=…` → Server-Sent Events. Emits `text` events as tokens arrive,
/// `tool` events as tools run, and a final `done` event. The turn runs on a spawned task feeding an
/// mpsc channel that backs the SSE stream.
async fn stream_message(
    State(agent): State<Shared>,
    State(auth): State<Arc<ServerAuth>>,
    State(resources): State<Arc<ResourceGovernor>>,
    ctx: Option<Extension<AuthContext>>,
    Path(id): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    // Resolve before establishing SSE so principal mode without a context is a normal 401.
    let turn = server_turn_context(&auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    let permit = resources
        .admit_work(&auth, ctx.as_ref().map(|e| &e.0))
        .map_err(|e| *e)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(REST_SSE_CHANNEL_CAPACITY);
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_task = cancel.clone();
    let drop_guard = cancel.drop_guard();
    let agent = agent.clone();
    tokio::spawn(async move {
        // The permit lives for the entire producer task, not merely until the HTTP response is
        // established. Its drop also accounts durable provider calls/spend on every exit path.
        let mut permit = permit;
        let mut sink = SseSink {
            tx: tx.clone(),
            cancel: cancel_task.clone(),
        };
        if let Err(e) = run_server_turn(
            &agent,
            &turn,
            &id,
            &q.input,
            &mut sink,
            &cancel_task,
            &mut permit,
        )
        .await
        {
            if !cancel_task.is_cancelled() {
                let _ = tx.try_send(Event::default().event("error").data(e.to_string()));
            }
        }
        if !cancel_task.is_cancelled() {
            let _ = tx.try_send(Event::default().event("done").data("end"));
        }
    });
    let stream = async_stream::stream! {
        // Same owner-stream rule as A2A `message/stream`: dropping the response body fires the
        // request-owned token. `run_turn_cancellable` then finalizes a valid cancelled history and
        // cannot begin another approved plan round.
        let _guard = drop_guard;
        while let Some(ev) = rx.recv().await {
            yield Ok(ev);
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Forwards a turn's deltas as SSE events over an mpsc channel.
struct SseSink {
    tx: tokio::sync::mpsc::Sender<Event>,
    cancel: tokio_util::sync::CancellationToken,
}

impl AgentSink for SseSink {
    fn text_delta(&mut self, t: &str) {
        if self
            .tx
            .try_send(Event::default().event("text").data(t))
            .is_err()
        {
            self.cancel.cancel();
        }
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        if self
            .tx
            .try_send(Event::default().event("tool").data(name))
            .is_err()
        {
            self.cancel.cancel();
        }
    }
}

/// Inbound webhook: a single external event creates a fresh session and runs one turn. This is
/// the trigger surface for integrations (a CI hook, or a chat message bridged by an external adapter).
async fn webhook(
    State(agent): State<Shared>,
    State(auth): State<Arc<ServerAuth>>,
    State(resources): State<Arc<ResourceGovernor>>,
    ctx: Option<Extension<AuthContext>>,
    request_cancel: Option<Extension<RequestCancellation>>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, Response> {
    let mut permit = resources
        .admit_work(&auth, ctx.as_ref().map(|e| &e.0))
        .map_err(|e| *e)?;
    // In principal mode the webhook's fresh session is tagged with the caller's realm, like
    // every other mint — an untagged session would be unreachable to its own creator.
    let session_id = mint_session(&agent, &auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    let mut sink = Collect::default();
    let turn = server_turn_context(&auth, ctx.as_ref().map(|e| &e.0)).map_err(|e| *e)?;
    let cancel = request_cancel
        .map(|extension| extension.0 .0)
        .unwrap_or_default();
    run_server_turn(
        &agent,
        &turn,
        &session_id,
        &req.input,
        &mut sink,
        &cancel,
        &mut permit,
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

    /// [`router_with_ttl_limits_and_approvals`] with no remote-approval posture — the shape every
    /// pre-C-453 test in this module was written against.
    ///
    /// It lives *inside* the test module rather than beside its production sibling on purpose:
    /// `website_contract`'s `http_api_reference_covers_every_served_route` recovers the mounted
    /// route set by reading this file up to its first `#[cfg(test)]`, so a test-only `fn` placed
    /// above the `.route(` calls truncates the scan to nothing and the guard silently stops
    /// checking anything.
    fn router_with_ttl_and_limits(
        engine: Arc<FlowEngine>,
        auth: ServerAuth,
        card: CardInfo,
        a2a_ttl: A2aTtl,
        limits: ServerLimits,
    ) -> Router {
        router_with_ttl_limits_and_approvals(
            engine,
            auth,
            card,
            a2a_ttl,
            limits,
            ApprovalGate::none(),
        )
    }

    /// C-277: `serve`'s readiness line is a cross-crate contract, not an `eprintln!`.
    ///
    /// `flux-orchestrate` (L3) decides a fleet worker is live by matching this crate's (L6) stderr.
    /// The layering rule means no test can reach across that pair, so a rewording here used to fail
    /// silently and remotely: `fleet.start` degrades to its 60-second readiness timeout and reports
    /// a worker that never announced itself — indistinguishable, at the call site, from a slow or
    /// hung worker. Both sides now render and match through
    /// [`flux_core::readiness`], and this test is what stops this one from drifting back to a local
    /// literal.
    #[test]
    fn the_serving_announcement_is_rendered_through_the_shared_contract() {
        let source = include_str!("lib.rs");
        // Split so this assertion's own text is not the match it is looking for.
        let literal = ["listening on ", "http://"].concat();
        assert!(
            !source.contains(&literal),
            "flux-server spells the readiness announcement itself. flux-orchestrate's \
             `fleet.start` matches that wording to decide a worker is live and cannot import this \
             crate to check — render it with `flux_core::readiness::serving_announcement` instead."
        );
        assert!(
            source.contains("readiness::serving_announcement("),
            "`serve` must render its readiness line through the shared contract"
        );
    }

    struct PrincipalTestAuthenticator;

    #[async_trait::async_trait]
    impl flux_auth::request::RequestAuthenticator for PrincipalTestAuthenticator {
        async fn authenticate(
            &self,
            bearer: &str,
        ) -> Result<AuthContext, flux_auth::request::AuthError> {
            use flux_policy::{Caller, CallerKind, Principal, Trust, TrustKind, TrustLevel};
            let id = match bearer {
                "alice-token" => "alice",
                "bob-token" => "bob",
                _ => return Err(flux_auth::request::AuthError::Unauthorized),
            };
            Ok(AuthContext {
                account: Some("same-account".into()),
                caller: Caller {
                    principal: Principal {
                        id: id.into(),
                        name: id.into(),
                        kind: CallerKind::User,
                    },
                    groups: Vec::new(),
                    source: "test".into(),
                },
                trust: Trust {
                    kind: TrustKind::Invocation,
                    level: TrustLevel::Verified,
                    scopes: Vec::new(),
                },
            })
        }
    }

    /// A single global queue cannot safely serve principal mode: Alice would otherwise list and
    /// answer Bob's effects despite the rest of the server keeping their sessions in separate
    /// realms. Until approvals carry a separately authorized supervisor identity, construction
    /// must refuse that incoherent combination.
    #[test]
    fn principal_auth_cannot_share_a_global_remote_approval_queue() {
        let (engine, _) = usage_test_engine();
        let auth = ServerAuth::Principal(PrincipalAuth::new(
            Arc::new(PrincipalTestAuthenticator),
            "https://agents.example.test",
        ));
        let result = router_with_approvals_in(
            engine,
            auth,
            CardInfo::flux_coding(),
            "127.0.0.1:0".parse().unwrap(),
            &DiscoveryEnv::empty(),
            ApprovalGate::serving(Arc::new(flux_runtime::ApprovalQueue::new(
                Duration::from_secs(30),
            ))),
        );
        let error = match result {
            Ok(_) => panic!("principal mode accepted a cross-realm global approval queue"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("principal"), "{error}");
        assert!(error.contains("approval"), "{error}");
    }

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

    /// C-321, the request half. A request with **no** `Authorization` header presents `""`, and
    /// `constant_time_eq(b"", b"")` is `true` — so an empty expected secret would authenticate every
    /// anonymous caller while the mode reads, everywhere it is printed or logged, as
    /// "shared-secret". Constructed as a struct literal on purpose: this half must hold for *any*
    /// path that reaches [`require_auth`] with an empty secret, not only for the ones that come
    /// through a constructor which already refuses one.
    #[tokio::test]
    async fn an_empty_shared_secret_authenticates_nothing() {
        let app = || {
            guarded_app(ServerAuth::SharedSecret {
                secret: String::new(),
                external_url: None,
            })
        };
        // The bypass: no header at all.
        assert_eq!(
            status(app(), "/protected", None).await,
            StatusCode::UNAUTHORIZED
        );
        // And its two near neighbours — an empty bearer, and any bearer at all.
        assert_eq!(
            status(app(), "/protected", Some("Bearer ")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(app(), "/protected", Some("Bearer anything")).await,
            StatusCode::UNAUTHORIZED
        );
        // The exempt routes stay exempt — an empty secret bricks authentication, it does not
        // change which routes carry the auth layer.
        assert_eq!(status(app(), "/health", None).await, StatusCode::OK);
    }

    /// C-321, the bind half, at the guard itself. The integration test
    /// (`tests/empty_shared_secret_bind.rs`) exercises this through a real socket and the public
    /// [`router`]; this pins the predicate directly so the guard's own truth table is legible.
    #[test]
    fn empty_shared_secret_is_effectively_open() {
        let public: SocketAddr = "0.0.0.0:8787".parse().unwrap();
        let loopback: SocketAddr = "127.0.0.1:8787".parse().unwrap();
        let empty = || ServerAuth::shared_secret(Some(String::new()), None);
        let real = || ServerAuth::shared_secret(Some("s3cr3t".into()), None);

        assert!(
            guard_open_bind(&empty(), public).is_err(),
            "an empty shared secret is `Open` in everything but name; it must not bind {public}"
        );
        assert!(
            guard_open_bind(&ServerAuth::Open, public).is_err(),
            "regression: the original `Open` refusal must survive"
        );
        assert!(
            guard_open_bind(&real(), public).is_ok(),
            "a real shared secret is authentication; the public bind stays allowed"
        );
        // Loopback: `Open` is allowed there by design, and so is the empty secret — the daemon is
        // not exposed. The *request* half (above) is what makes an empty secret useless there.
        assert!(guard_open_bind(&empty(), loopback).is_ok());
        assert!(guard_open_bind(&ServerAuth::Open, loopback).is_ok());
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

        let app = router_in(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            "127.0.0.1:0".parse().unwrap(),
            &DiscoveryEnv::empty(),
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
        let app = router_with_ttl_in(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(60),
            &DiscoveryEnv::empty(),
            ApprovalGate::none(),
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
        flux_events::SessionLog::open(&events, &active)
            .unwrap()
            .open_turn(flux_core::Message::user_text("still here"))
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

    /// Emits one prose delta and then remains pending until the request cancellation drops its
    /// stream. This lets the REST SSE test establish the response, consume one real frame, and
    /// model a TCP disconnect while provider work is live.
    struct HangingAfterDeltaProvider;
    #[async_trait::async_trait]
    impl flux_provider::Provider for HangingAfterDeltaProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(
            &self,
            req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            if req.tools.iter().any(|tool| tool.name == "declare_intent") {
                return ProseProvider.stream(req).await;
            }
            use futures::StreamExt;
            let first =
                futures::stream::iter(vec![Ok(flux_core::Chunk::TextDelta("started".into()))]);
            Ok(Box::pin(first.chain(futures::stream::pending())))
        }
    }

    struct GatedProvider {
        entered: std::sync::atomic::AtomicBool,
        released: std::sync::atomic::AtomicBool,
        notify: tokio::sync::Notify,
    }

    impl GatedProvider {
        fn new() -> Self {
            Self {
                entered: std::sync::atomic::AtomicBool::new(false),
                released: std::sync::atomic::AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
            }
        }

        async fn wait_entered(&self) {
            while !self.entered.load(std::sync::atomic::Ordering::SeqCst) {
                self.notify.notified().await;
            }
        }

        fn release(&self) {
            self.released
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }

    #[async_trait::async_trait]
    impl flux_provider::Provider for GatedProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(
            &self,
            req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            self.entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.notify.notify_waiters();
            while !self.released.load(std::sync::atomic::Ordering::SeqCst) {
                self.notify.notified().await;
            }
            ProseProvider.stream(req).await
        }
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
        assert!(
            events
                .turns(&sid)
                .unwrap()
                .last()
                .is_some_and(|turn| turn.outcome == "cancelled"),
            "408 is returned only after durable cancellation finalizes"
        );
        flux_events::ValidHistory::new(events.conversation(&sid).unwrap())
            .expect("timeout cancellation leaves a valid provider history");
    }

    /// Blocking A2A uses the same request-owned token; its registry entry and durable turn are
    /// finalized before the timeout response escapes the middleware.
    #[tokio::test]
    async fn blocking_a2a_timeout_cancels_and_finalizes_before_408() {
        let (engine, events) = test_engine(Arc::new(SlowProvider {
            delay: Duration::from_millis(500),
        }));
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
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "message/send",
            "params": {
                "configuration": { "blocking": true },
                "message": {
                    "contextId": "timeout-context",
                    "parts": [{ "kind": "text", "text": "hi" }]
                }
            }
        });
        let response = app
            .oneshot(
                HttpRequest::post("/a2a")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        let session = events
            .list(10)
            .unwrap()
            .into_iter()
            .find(|session| session.context.correlation_id.as_deref() == Some("timeout-context"))
            .expect("blocking A2A minted its task session");
        assert!(
            events
                .turns(&session.id)
                .unwrap()
                .last()
                .is_some_and(|turn| turn.outcome == "cancelled"),
            "A2A timeout waits for durable cancellation"
        );
        flux_events::ValidHistory::new(events.conversation(&session.id).unwrap())
            .expect("A2A timeout leaves a valid provider history");
    }

    /// C-189: the SSE stream route is EXEMPT from the request timeout — a long-lived stream is not
    /// a stuck request. Even with a 50 ms timeout and a provider that sleeps 500 ms, the stream is
    /// established (`200`) rather than severed with `408`: the handler returns its `Sse` response
    /// promptly and the turn streams behind it. (This confirms the exemption's intent; the layer
    /// would not fire on this fast-returning handler even if applied — see [`router_with_ttl_in`].)
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

    /// C-260 failing-first: once the REST SSE body has been polled, dropping it owns cancellation
    /// of the still-live turn. Pre-change the producer had a fresh token nobody could fire and the
    /// detached task remained pending forever. The durable terminal row and `ValidHistory` check
    /// also pin the recurrent provider-session-shape invariant on this new termination path.
    #[tokio::test]
    async fn dropping_rest_sse_body_cancels_and_finalizes_the_turn() {
        use futures::StreamExt;

        let (engine, events) = test_engine(Arc::new(HangingAfterDeltaProvider));
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            ServerLimits::default(),
        );
        let response = app
            .oneshot(
                HttpRequest::get(format!("/sessions/{sid}/stream?input=hi"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        // Polling the body instantiates its cancellation guard. This provider deliberately never
        // completes the prose model response, so no user-visible delta is forwarded yet.
        let _ = tokio::time::timeout(Duration::from_millis(50), body.next()).await;
        assert!(
            !events.turns(&sid).unwrap().is_empty(),
            "the provider turn was live before disconnect"
        );
        drop(body);

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if events
                    .turns(&sid)
                    .unwrap()
                    .last()
                    .is_some_and(|turn| turn.outcome == "cancelled")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("disconnect cancellation reaches durable turn finalization");
        flux_events::ValidHistory::new(events.conversation(&sid).unwrap())
            .expect("disconnect leaves a valid provider history");
    }

    /// C-260 failing-first: the REST sink uses a finite channel and treats a full buffer as a
    /// stalled owner. Pre-change `UnboundedSender` accepted every delta and this token stayed live.
    #[test]
    fn rest_sse_sink_cancels_when_the_bounded_buffer_is_full() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(Event::default().data("prefill")).unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut sink = SseSink {
            tx,
            cancel: cancel.clone(),
        };
        sink.text_delta("one event too many");
        assert!(cancel.is_cancelled());
    }

    /// C-261 failing-first: rate rejection happens before `POST /sessions` mints another session.
    /// The wire result is a real 429 with retry context, not a successful response carrying an
    /// application-level error.
    #[tokio::test]
    async fn request_rate_limit_rejects_before_session_mint() {
        let (engine, events) = usage_test_engine();
        let limits = ServerLimits {
            requests_per_window: 1,
            request_rate_window: Duration::from_secs(60),
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let first = app
            .clone()
            .oneshot(HttpRequest::post("/sessions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let second = app
            .oneshot(HttpRequest::post("/sessions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(second.headers()["x-flux-limit"], "request_rate");
        assert_eq!(events.list(10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn request_rate_is_principal_keyed_and_never_retains_bearer_values() {
        let (engine, events) = usage_test_engine();
        let limits = ServerLimits {
            requests_per_window: 1,
            request_rate_window: Duration::from_secs(60),
            ..ServerLimits::default()
        };
        let auth = ServerAuth::Principal(PrincipalAuth::new(
            Arc::new(PrincipalTestAuthenticator),
            "https://agents.example.test",
        ));
        let app =
            router_with_ttl_and_limits(engine, auth, CardInfo::flux_coding(), A2aTtl(0), limits);
        let request = |token: &'static str| {
            HttpRequest::post("/sessions")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(request("alice-token"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(request("alice-token"))
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            app.oneshot(request("bob-token")).await.unwrap().status(),
            StatusCode::OK,
            "a second principal in the same account has an independent bucket"
        );
        assert_eq!(events.list(10).unwrap().len(), 2);
    }

    /// The authenticated boundary covers read-only protected routes too. Before the limiter was
    /// structural, only handlers that minted sessions or ran turns called it, so repeated usage
    /// reads bypassed request admission entirely.
    #[tokio::test]
    async fn request_rate_covers_authenticated_protected_reads() {
        let (engine, _events) = usage_test_engine();
        let limits = ServerLimits {
            requests_per_window: 1,
            request_rate_window: Duration::from_secs(60),
            ..ServerLimits::default()
        };
        let auth = ServerAuth::Principal(PrincipalAuth::new(
            Arc::new(PrincipalTestAuthenticator),
            "https://agents.example.test",
        ));
        let app =
            router_with_ttl_and_limits(engine, auth, CardInfo::flux_coding(), A2aTtl(0), limits);
        let request = || {
            HttpRequest::get("/usage")
                .header("authorization", "Bearer alice-token")
                .body(Body::empty())
                .unwrap()
        };

        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::OK
        );
        let rejected = app.oneshot(request()).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.headers()["x-flux-limit"], "request_rate");
    }

    /// Request rate is charged once at middleware, while the handler independently reserves its
    /// work slot. With a limit of one, the first turn must run; a handler-level second charge would
    /// reject that same request before its provider call.
    #[tokio::test]
    async fn work_request_is_counted_once_at_the_authenticated_boundary() {
        let (engine, events) = test_engine(Arc::new(ProseProvider));
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let limits = ServerLimits {
            requests_per_window: 1,
            request_rate_window: Duration::from_secs(60),
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let completed = app
            .clone()
            .oneshot(
                HttpRequest::post(format!("/sessions/{sid}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "input": "hi" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(completed.status(), StatusCode::OK);

        let rejected = app
            .oneshot(HttpRequest::get("/usage").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.headers()["x-flux-limit"], "request_rate");
    }

    /// C-261 closure: provider calls are circuit-breaker facts even when the provider reports no
    /// token usage. `ProseProvider` emits no `Chunk::Usage`; the completed turn must still consume
    /// the call budget before the next admission.
    #[tokio::test]
    async fn zero_usage_provider_calls_trip_the_completed_call_breaker() {
        let (engine, events) = test_engine(Arc::new(ProseProvider));
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let limits = ServerLimits {
            requests_per_window: 100,
            provider_calls_per_window: 1,
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let request = || {
            HttpRequest::post(format!("/sessions/{sid}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "input": "hi" }).to_string()))
                .unwrap()
        };

        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::OK
        );
        let rows = events
            .cost_summary(&sid, &flux_credentials::load_pricing_table())
            .unwrap();
        assert!(
            rows.iter().map(|row| row.calls).sum::<u64>() >= 1,
            "zero-token provider calls remain countable durable facts"
        );
        let rejected = app.oneshot(request()).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.headers()["x-flux-limit"], "provider_calls");
    }

    /// One cross-surface slot covers work rather than HTTP response production: while a REST turn
    /// is blocked in its provider, a webhook is rejected before its fresh-session mint. A2A
    /// background and streaming producers own the same permit type for their full task lifetime.
    #[tokio::test]
    async fn live_work_limit_is_shared_across_rest_and_webhook_before_mint() {
        let provider = Arc::new(GatedProvider::new());
        let (engine, events) = test_engine(provider.clone());
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let limits = ServerLimits {
            requests_per_window: 100,
            max_inflight_per_key: 1,
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let first_app = app.clone();
        let first = tokio::spawn(async move {
            first_app
                .oneshot(
                    HttpRequest::post(format!("/sessions/{sid}/messages"))
                        .header("content-type", "application/json")
                        .body(Body::from(json!({ "input": "hold" }).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(2), provider.wait_entered())
            .await
            .expect("first provider call starts");

        let rejected = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "input": "must not mint" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.headers()["x-flux-limit"], "concurrency");
        assert_eq!(events.list(10).unwrap().len(), 1);

        provider.release();
        assert_eq!(first.await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn nonblocking_a2a_holds_cross_surface_slot_until_background_turn_finishes() {
        let provider = Arc::new(GatedProvider::new());
        let (engine, events) = test_engine(provider.clone());
        let limits = ServerLimits {
            requests_per_window: 100,
            max_inflight_per_key: 1,
            ..ServerLimits::default()
        };
        let app = router_with_ttl_and_limits(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            A2aTtl(0),
            limits,
        );
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "message/send",
            "params": { "message": { "parts": [{ "kind": "text", "text": "hold" }] } }
        });
        let submitted = app
            .clone()
            .oneshot(
                HttpRequest::post("/a2a")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(submitted.status(), StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(2), provider.wait_entered())
            .await
            .expect("background A2A provider call starts");

        let rejected = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "input": "must not mint" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(events.list(10).unwrap().len(), 1);

        provider.release();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let done = events
                    .list(10)
                    .unwrap()
                    .first()
                    .is_some_and(|session| !events.turns(&session.id).unwrap().is_empty());
                if done {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background A2A turn finishes after release");
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
        let refused = router_in(
            engine.clone(),
            ServerAuth::Open,
            CardInfo::flux_coding(),
            "0.0.0.0:8080".parse().unwrap(),
            &DiscoveryEnv::empty(),
        );
        assert!(
            refused.is_err(),
            "Open + non-loopback must be refused at router construction, not only in serve_on"
        );

        // Authenticated + non-loopback is fine — the refusal is specifically the UNAUTHENTICATED
        // case (a shared secret makes a routable bind safe).
        assert!(
            router_in(
                engine.clone(),
                ServerAuth::from_token(Some("s3cr3t".to_string())),
                CardInfo::flux_coding(),
                "0.0.0.0:8080".parse().unwrap(),
                &DiscoveryEnv::empty(),
            )
            .is_ok(),
            "an authenticated non-loopback router still builds"
        );

        // Open + loopback is the dev path — it builds, and (being open) serves a protected route
        // without a token, which is exactly why the non-loopback refusal above matters.
        let app = router_in(
            engine,
            ServerAuth::Open,
            CardInfo::flux_coding(),
            "127.0.0.1:0".parse().unwrap(),
            &DiscoveryEnv::empty(),
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

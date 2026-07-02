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
//! - `GET  /sessions/:id`                 → session info
//! - `POST /sessions/:id/messages`        → `{ text, tool_calls, usage }`
//!
//! The agent runs tools through the same safety envelope as the CLI; build it with auto-approve
//! since HTTP requests have no interactive approver.

mod a2a;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{FromRef, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde_json::{json, Value};

use flux_core::Usage;
use flux_flow::engine::FlowEngine;
use flux_flow::AgentSink;

type Shared = Arc<FlowEngine>;
pub(crate) type TurnGate = Arc<tokio::sync::Mutex<()>>;

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
        }
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

/// Bind `addr` and serve until shutdown. When `token` is `Some`, every route except `/health`
/// requires `Authorization: Bearer <token>`; when `None`, no authentication is enforced and the
/// listener must be loopback.
pub async fn serve(addr: &str, agent: FlowEngine, token: Option<String>) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
    eprintln!("flux server listening on http://{addr}");
    eprintln!("  A2A agent card:  http://{addr}/.well-known/agent-card.json");
    eprintln!("  A2A endpoint:    http://{addr}/a2a  (message/send, message/stream)");
    serve_on(listener, agent, token).await
}

/// Serve on an already-bound listener (lets callers pick an ephemeral port).
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    agent: FlowEngine,
    token: Option<String>,
) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    if token.is_none() && !unauthenticated_bind_allowed(addr) {
        anyhow::bail!(
            "refusing unauthenticated non-loopback bind on {addr}; set FLUX_SERVER_TOKEN or bind \
             to 127.0.0.1/::1"
        );
    }
    axum::serve(
        listener,
        router(Arc::new(agent), token, CardInfo::flux_coding()),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

fn unauthenticated_bind_allowed(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
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

/// Build the API router over `engine`, advertising `card` on the A2A discovery endpoint. When `token`
/// is `Some`, every route except `/health` and the agent card requires `Authorization: Bearer <token>`.
/// Public so the `a2a` channel ([`flux_channels`]) can mount it onto a program agent's engine with its
/// own graceful-shutdown serve.
pub fn router(engine: Arc<FlowEngine>, token: Option<String>, card: CardInfo) -> Router {
    router_with_ttl(engine, token, card, a2a_ttl_from_config())
}

/// Resolve the A2A session TTL from the layered flux config (`[server] a2a_session_ttl_secs`,
/// project over user, default 1h, `0` = never prune). Resolved here — at router build — so every
/// mount of the router (the standalone server and the `a2a` channel) gets the same retention
/// behavior without each caller plumbing the knob. A malformed config file falls back to the
/// default with a warning rather than failing the surface (the CLI already fails loudly on it).
fn a2a_ttl_from_config() -> A2aTtl {
    let ttl = std::env::current_dir()
        .ok()
        .and_then(|cwd| match flux_config::load(&cwd) {
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
    token: Option<String>,
    card: CardInfo,
    a2a_ttl: A2aTtl,
) -> Router {
    let state = ServerState {
        engine,
        card: Arc::new(card),
        turn_gate: Arc::new(tokio::sync::Mutex::new(())),
        a2a_ttl,
    };
    // Auth-exempt routes — registered outside the middleware layer so path-string comparison
    // cannot be bypassed by percent-encoding or double-slash tricks.
    let exempt = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/.well-known/agent-card.json", get(a2a::agent_card))
        .route("/.well-known/agent.json", get(a2a::agent_card));

    // Every other route requires a valid Bearer token when one is configured.
    let protected = Router::new()
        .route("/a2a", post(a2a::a2a_handler))
        .route("/sessions", post(create_session))
        .route("/sessions/:id", get(get_session))
        .route("/sessions/:id/messages", post(post_message))
        .route("/sessions/:id/stream", get(stream_message))
        .route("/sessions/:id/usage", get(get_session_usage))
        .route("/usage", get(get_usage_all))
        .route("/webhook", post(webhook))
        .route_layer(middleware::from_fn_with_state(
            Arc::new(token),
            require_auth,
        ));

    exempt.merge(protected).with_state(state)
}

/// Bearer-token gate. With no configured token this is a pass-through; otherwise the request
/// must present a matching `Authorization: Bearer` header (compared in constant time).
/// Exempt routes (`/health`, `/.well-known/agent.json`) are registered outside this middleware's
/// scope in [`router`] — no path-string bypass is possible.
async fn require_auth(
    State(token): State<Arc<Option<String>>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(expected) = token.as_ref() {
        let presented = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(req).await)
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

async fn create_session(State(agent): State<Shared>) -> Result<Json<Value>, (StatusCode, String)> {
    let id = agent.events.create_session(&agent.model).map_err(err500)?;
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
    State(turn_gate): State<TurnGate>,
    Path(id): Path<String>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut sink = Collect::default();
    let _turn = turn_gate.lock().await;
    agent
        .run_turn(&id, &req.input, &mut sink)
        .await
        .map_err(err500)?;
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

/// `GET /sessions/:id/usage` — per-model token tiers + cost for one session (C-06).
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

/// `GET /usage` — per-model token tiers + cost across every session (C-06).
async fn get_usage_all(State(agent): State<Shared>) -> Result<Json<Value>, (StatusCode, String)> {
    let pricing = flux_credentials::load_pricing_table();
    let rows = agent.events.cost_summary_all(&pricing).map_err(err500)?;
    Ok(Json(json!({
        "models": rows.iter().map(model_cost_json).collect::<Vec<_>>(),
    })))
}

#[derive(serde::Deserialize)]
struct StreamQuery {
    input: String,
}

/// `GET /sessions/:id/stream?input=…` → Server-Sent Events. Emits `text` events as tokens arrive,
/// `tool` events as tools run, and a final `done` event. The turn runs on a spawned task feeding an
/// mpsc channel that backs the SSE stream.
async fn stream_message(
    State(agent): State<Shared>,
    State(turn_gate): State<TurnGate>,
    Path(id): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let agent = agent.clone();
    tokio::spawn(async move {
        let mut sink = SseSink { tx: tx.clone() };
        let _turn = turn_gate.lock().await;
        if let Err(e) = agent.run_turn(&id, &q.input, &mut sink).await {
            let _ = tx.send(Event::default().event("error").data(e.to_string()));
        }
        let _ = tx.send(Event::default().event("done").data("end"));
    });
    let stream = async_stream::stream! {
        while let Some(ev) = rx.recv().await {
            yield Ok(ev);
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
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
    State(turn_gate): State<TurnGate>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session_id = agent.events.create_session(&agent.model).map_err(err500)?;
    let mut sink = Collect::default();
    let _turn = turn_gate.lock().await;
    agent
        .run_turn(&session_id, &req.input, &mut sink)
        .await
        .map_err(err500)?;
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
    fn guarded_app(token: Option<String>) -> Router {
        let exempt = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route(
                "/.well-known/agent-card.json",
                get(|| async { Json(json!({})) }),
            )
            .route("/.well-known/agent.json", get(|| async { Json(json!({})) }));
        let protected = Router::new()
            .route("/protected", get(|| async { "data" }))
            .route_layer(middleware::from_fn_with_state(
                Arc::new(token),
                require_auth,
            ));
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
        let app = || guarded_app(Some("s3cr3t".to_string()));
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
            status(guarded_app(None), "/protected", None).await,
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

    /// A provider that answers every call with a one-word prose turn — the agent loop exits on
    /// prose, so a handler-driven test turn completes fast and deterministically.
    struct ProseProvider;
    #[async_trait::async_trait]
    impl flux_provider::Provider for ProseProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(
            &self,
            _req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(flux_core::Chunk::TextDelta("ok".into())),
                Ok(flux_core::Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::EndTurn),
                }),
            ])))
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
            flux_runtime::PermissionManager::from_rules(
                &["plan".into(), "run_plan".into(), "observe".into()],
                &[],
            ),
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

    /// C-06 server endpoint: `GET /sessions/:id/usage` returns cache tiers + cost, and `GET /usage`
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
                },
            )
            .unwrap();
        events
            .end_turn(&sid, turn_id, "accepted", 1, "done", None)
            .unwrap();

        let app = router(engine, None, CardInfo::flux_coding());
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
        let app = router_with_ttl(engine, None, CardInfo::flux_coding(), A2aTtl(60));
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
        assert_eq!(a2a::prune_expired_a2a_sessions_at(&events, 60, now), 1);
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
            a2a::prune_expired_a2a_sessions_at(&events, 1, cutoff + 1_000),
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
            a2a::prune_expired_a2a_sessions_at(&events, 0, far_future),
            0
        );
        assert!(events.info(&old).is_ok(), "ttl 0 means never prune");
    }
}

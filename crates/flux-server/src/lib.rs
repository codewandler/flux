//! `flux-server` — a long-running HTTP API around an [`Agent`], so flux can be driven headlessly
//! or remotely (`flux serve <addr>`).
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
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
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

/// Bind `addr` and serve until shutdown. When `token` is `Some`, every route except `/health`
/// requires `Authorization: Bearer <token>`; when `None`, no authentication is enforced (the CLI
/// only permits that for a loopback bind).
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
    axum::serve(listener, router(Arc::new(agent), token)).await?;
    Ok(())
}

fn router(state: Shared, token: Option<String>) -> Router {
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
    Path(id): Path<String>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let mut sink = Collect::default();
    agent
        .run_turn(&id, &req.input, &mut sink)
        .await
        .map_err(err500)?;
    Ok(Json(json!({
        "text": sink.text,
        "tool_calls": sink.tools,
        "usage": sink.usage.map(|u| json!({ "input": u.input_tokens, "output": u.output_tokens })),
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
    Path(id): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let agent = agent.clone();
    tokio::spawn(async move {
        let mut sink = SseSink { tx: tx.clone() };
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
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session_id = agent.events.create_session(&agent.model).map_err(err500)?;
    let mut sink = Collect::default();
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

    /// Build a tiny router carrying only the auth layer over a `/health` and a protected route, so
    /// the gate can be exercised without standing up a full `Agent`.
    /// Mirror the split-router structure from [`router`]: exempt routes outside the middleware,
    /// protected routes inside.
    fn guarded_app(token: Option<String>) -> Router {
        let exempt = Router::new()
            .route("/health", get(|| async { "ok" }))
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
}

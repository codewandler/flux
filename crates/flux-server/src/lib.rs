//! `flux-server` — a long-running HTTP API around an [`Agent`], so flux can be driven headlessly
//! or remotely (`flux --serve <addr>`).
//!
//! Routes:
//! - `GET  /health` → `ok`
//! - `POST /sessions` → `{ id, model }`
//! - `GET  /sessions/:id` → session info
//! - `POST /sessions/:id/messages` `{ "input": "..." }` → `{ text, tool_calls, usage }`
//!
//! The agent runs tools through the same safety envelope as the CLI; build it with auto-approve
//! since HTTP requests have no interactive approver.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde_json::{json, Value};

use flux_agent::{Agent, AgentSink};
use flux_core::Usage;

type Shared = Arc<Agent>;

/// Bind `addr` and serve until shutdown.
pub async fn serve(addr: &str, agent: Agent) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("flux server listening on http://{}", listener.local_addr()?);
    serve_on(listener, agent).await
}

/// Serve on an already-bound listener (lets callers pick an ephemeral port).
pub async fn serve_on(listener: tokio::net::TcpListener, agent: Agent) -> anyhow::Result<()> {
    axum::serve(listener, router(Arc::new(agent))).await?;
    Ok(())
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/sessions", post(create_session))
        .route("/sessions/:id", get(get_session))
        .route("/sessions/:id/messages", post(post_message))
        .route("/sessions/:id/stream", get(stream_message))
        .route("/webhook", post(webhook))
        .with_state(state)
}

fn err500(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn create_session(State(agent): State<Shared>) -> Result<Json<Value>, (StatusCode, String)> {
    let id = agent.store.create_session(&agent.model).map_err(err500)?;
    Ok(Json(json!({ "id": id, "model": agent.model })))
}

async fn get_session(
    State(agent): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let info = agent
        .store
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
/// the trigger surface for integrations (a CI hook, a chat message bridged by `flux-integrations`).
async fn webhook(
    State(agent): State<Shared>,
    Json(req): Json<MessageRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session_id = agent.store.create_session(&agent.model).map_err(err500)?;
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
struct Collect {
    text: String,
    tools: Vec<String>,
    usage: Option<Usage>,
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

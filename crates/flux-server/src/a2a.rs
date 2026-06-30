//! A2A (Agent-to-Agent) protocol support for `flux-server`.
//!
//! Adds three routes to the router:
//!   `GET  /.well-known/agent-card.json` — agent discovery card (A2A spec)
//!   `GET  /.well-known/agent.json`      — legacy discovery alias (same card)
//!   `POST /a2a`                         — JSON-RPC 2.0 dispatcher
//!
//! Supported methods (current A2A spec):
//! - `message/send`   — run one flux turn, return the resulting `Task` synchronously
//! - `message/stream` — run one flux turn, stream `TaskStatusUpdate` events as Server-Sent Events
//!
//! The wire shapes come from the shared [`flux_a2a`] types, so client and server agree on one
//! definition. Each task creates a fresh session (stateless A2A mode); the `contextId` from the
//! request is echoed so a future stateful mode (one session per `contextId`) needs no client change.
//! The agent card is exempt from bearer-token auth so external agents can discover flux without a key.

use std::convert::Infallible;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_a2a::server;
use flux_a2a::{AgentCard, Message, Task, TaskState, TaskStatus};
use flux_flow::AgentSink;

use std::sync::Arc;

use super::Collect;
use crate::{CardInfo, Shared};

// ── Agent Card ────────────────────────────────────────────────────────────────

/// `GET /.well-known/agent-card.json` (and the `…/agent.json` alias) — A2A discovery.
///
/// The card's `name`/`description`/`skills` come from the served agent's [`CardInfo`] (the built-in
/// coding agent by default, or a program-declared agent when mounted by the `a2a` channel). The `url`
/// field points to the `/a2a` JSON-RPC endpoint on the same host, derived from the request's `Host`
/// (and `X-Forwarded-Proto`) headers so the card is correct whether accessed directly or through a
/// reverse proxy.
pub async fn agent_card(State(card): State<Arc<CardInfo>>, headers: HeaderMap) -> Json<AgentCard> {
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    let url = format!("{scheme}://{host}/a2a");

    Json(server::agent_card(
        &card.name,
        &card.description,
        Some(url),
        env!("CARGO_PKG_VERSION"),
        &card.skills,
        true,
    ))
}

// ── JSON-RPC 2.0 helpers ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

fn rpc_json(id: Option<Value>, result: Value) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn rpc_err(id: Option<Value>, code: i32, msg: impl Into<String>) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg.into() } }))
}

// ── A2A message helpers ───────────────────────────────────────────────────────
//
// Text/contextId extraction, the agent card, the RFC-3339 stamp, and the status-update shaping are
// the reusable A2A protocol logic — they live in `flux_a2a::server` and are shared with other A2A
// surfaces. This module keeps only the flux-server-specific axum
// routes, the engine wiring, and the SSE streaming control-flow.

/// Build an SSE frame: a JSON-RPC response whose `result` is a `TaskStatusUpdateEvent`. The SSE
/// event name is left at the default so the frame is a plain `data:` JSON-RPC response per spec.
fn status_frame(
    id: &Option<Value>,
    task_id: &str,
    context_id: &str,
    state: TaskState,
    message: Option<Message>,
    is_final: bool,
) -> Event {
    let result = server::status_update_value(task_id, context_id, state, message, is_final);
    Event::default().data(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

// ── Main dispatcher ───────────────────────────────────────────────────────────

/// `POST /a2a` — JSON-RPC 2.0 endpoint.
///
/// - `message/send`   → [`send`] (synchronous, returns a `Task`)
/// - `message/stream` → [`subscribe`] (SSE stream of `TaskStatusUpdate`s)
pub async fn a2a_handler(
    State(engine): State<Shared>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if req.jsonrpc != "2.0" {
        return rpc_err(req.id, -32600, "jsonrpc must be \"2.0\"").into_response();
    }
    match req.method.as_str() {
        "message/send" => send(engine, req.id, req.params).await.into_response(),
        "message/stream" => match subscribe(engine, req.id.clone(), req.params).await {
            Ok(sse) => sse.into_response(),
            // Format pre-SSE errors as JSON-RPC so the `id` is not silently dropped.
            Err(msg) => rpc_err(req.id, -32602, msg).into_response(),
        },
        m => rpc_err(req.id, -32601, format!("Method not found: {m}")).into_response(),
    }
}

// ── message/send ──────────────────────────────────────────────────────────────

async fn send(engine: Shared, id: Option<Value>, params: Option<Value>) -> Json<Value> {
    let params = match params {
        Some(p) => p,
        None => return rpc_err(id, -32602, "Missing params"),
    };
    let input = match server::extract_text(&params) {
        Some(t) => t,
        None => return rpc_err(id, -32602, "No text found in message parts"),
    };
    let session_id = match engine.events.create_session(&engine.model) {
        Ok(s) => s,
        Err(e) => return rpc_err(id, -32603, format!("Session error: {e}")),
    };
    let context_id = server::extract_context_id(&params).unwrap_or_else(|| session_id.clone());
    let mut sink = Collect::default();
    match engine.run_turn(&session_id, &input, &mut sink).await {
        Ok(()) => {
            let status = TaskStatus::new(
                TaskState::Completed,
                Some(Message::agent_text(sink.text)),
                Some(server::now_rfc3339()),
            );
            let task = Task::new(session_id, Some(context_id), status);
            match serde_json::to_value(&task) {
                Ok(v) => rpc_json(id, v),
                Err(e) => rpc_err(id, -32603, format!("encode error: {e}")),
            }
        }
        Err(e) => rpc_err(id, -32603, format!("Agent error: {e}")),
    }
}

// ── message/stream ────────────────────────────────────────────────────────────

async fn subscribe(
    engine: Shared,
    id: Option<Value>,
    params: Option<Value>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, String> {
    let params = params.ok_or_else(|| "Missing params".to_string())?;
    let input =
        server::extract_text(&params).ok_or_else(|| "No text in message parts".to_string())?;
    // TODO: sessions created for A2A tasks are never explicitly pruned. Add a TTL-based cleanup
    // pass (e.g. `DELETE FROM sessions WHERE created_at_ms < now - 3_600_000`) on a background timer
    // in `serve_on`, or expire them inside `SessionStore::create_session`.
    let session_id = engine
        .events
        .create_session(&engine.model)
        .map_err(|e| e.to_string())?;
    let context_id = server::extract_context_id(&params).unwrap_or_else(|| session_id.clone());
    let task_id = session_id.clone();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    // `drop_guard` cancels `cancel` when the SSE stream is dropped (client disconnect), which
    // propagates through `cancel_task` into `run_turn_cancellable`.
    let cancel = CancellationToken::new();
    let cancel_task = cancel.clone();
    let drop_guard = cancel.drop_guard();

    let engine_clone = engine.clone();
    tokio::spawn(async move {
        // Initial "working" update so the caller knows the task started.
        let _ = tx.send(status_frame(
            &id,
            &task_id,
            &context_id,
            TaskState::Working,
            None,
            false,
        ));
        let mut sink = StreamSink {
            tx: tx.clone(),
            id: id.clone(),
            task_id: task_id.clone(),
            context_id: context_id.clone(),
            cancel: cancel_task.clone(),
        };
        let result = engine_clone
            .run_turn_cancellable(&session_id, &input, &mut sink, &cancel_task)
            .await;
        // If the client disconnected mid-stream, skip the final event — nobody is listening.
        if !cancel_task.is_cancelled() {
            // The final event carries no message on success — the deltas already streamed are
            // authoritative; on failure it carries the error text.
            let (state, message) = match result {
                Ok(()) => (TaskState::Completed, None),
                Err(e) => (TaskState::Failed, Some(Message::agent_text(e.to_string()))),
            };
            let _ = tx.send(status_frame(
                &id,
                &task_id,
                &context_id,
                state,
                message,
                true,
            ));
        }
        // `tx` (and sink.tx clone) drop here → channel closes → stream ends.
    });

    let stream = async_stream::stream! {
        // Keep the drop guard alive for the stream's lifetime: when axum drops the SSE response
        // (TCP disconnect), `_guard` fires and cancels the in-flight turn.
        let _guard = drop_guard;
        while let Some(ev) = rx.recv().await {
            yield Ok(ev);
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Streams text deltas back as SSE `working` status updates. Each delta is an incremental
/// status-update message; the final `completed` event (sent by the spawner) carries no message.
struct StreamSink {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
    /// The originating JSON-RPC request id, echoed in every frame.
    id: Option<Value>,
    task_id: String,
    context_id: String,
    /// Cancelled when the SSE receiver is dropped (client disconnect); checked between plan rounds
    /// by `run_turn_cancellable`.
    cancel: CancellationToken,
}

impl AgentSink for StreamSink {
    fn text_delta(&mut self, t: &str) {
        // Send only the delta in working events; sending the full accumulated text on every token
        // would be O(N²) in response length.
        let frame = status_frame(
            &self.id,
            &self.task_id,
            &self.context_id,
            TaskState::Working,
            Some(Message::agent_text(t)),
            false,
        );
        if self.tx.send(frame).is_err() {
            // Receiver gone — client disconnected; stop doing work as soon as possible.
            self.cancel.cancel();
        }
    }
}

// The text/contextId extraction, the agent card, the RFC-3339 timestamp, and the status-update
// shaping now live in `flux_a2a::server` (shared with other A2A surfaces) and are unit-tested there.

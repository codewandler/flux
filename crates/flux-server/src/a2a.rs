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

use flux_a2a::{
    AgentCard, Capabilities, Message, Skill, Task, TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use flux_flow::AgentSink;

use super::Collect;
use crate::Shared;

// ── Agent Card ────────────────────────────────────────────────────────────────

/// `GET /.well-known/agent-card.json` (and the `…/agent.json` alias) — A2A discovery.
///
/// The `url` field points to the `/a2a` JSON-RPC endpoint on the same host, derived from the
/// request's `Host` (and `X-Forwarded-Proto`) headers so the card is correct whether accessed
/// directly or through a reverse proxy.
pub async fn agent_card(headers: HeaderMap) -> Json<AgentCard> {
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    let url = format!("{scheme}://{host}/a2a");

    Json(AgentCard {
        name: "flux".to_string(),
        description: "flux — a precise, autonomous coding agent. Reads, writes, edits, \
                      searches, and runs code in a workspace. Carries tasks from instruction to \
                      verified completion through a deterministic Flux-Lang plan + guarded safety \
                      envelope."
            .to_string(),
        url: Some(url),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: Capabilities {
            streaming: true,
            push_notifications: false,
            ..Default::default()
        },
        default_input_modes: vec!["text/plain".to_string()],
        default_output_modes: vec!["text/plain".to_string()],
        skills: vec![Skill {
            id: "coding".to_string(),
            name: "Coding Agent".to_string(),
            description: "Read, write, edit, search, and execute code tasks in a workspace. The \
                          agent plans, executes, and verifies — then reports back."
                .to_string(),
            input_modes: vec!["text/plain".to_string()],
            output_modes: vec!["text/plain".to_string()],
        }],
        interfaces: Vec::new(),
    })
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

/// Concatenate all text parts of an A2A `message` (parts with `kind == "text"`).
fn extract_text(params: &Value) -> Option<String> {
    let parts = params.get("message")?.get("parts")?.as_array()?;
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|p| {
            if p.get("kind")?.as_str()? == "text" {
                p.get("text")?.as_str()
            } else {
                None
            }
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

/// The conversation `contextId` the client supplied on the message, if any.
fn extract_context_id(params: &Value) -> Option<String> {
    params
        .get("message")?
        .get("contextId")?
        .as_str()
        .map(str::to_string)
}

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
    let status = TaskStatus::new(state, message, Some(now_rfc3339()));
    let evt = TaskStatusUpdateEvent::new(task_id, Some(context_id.to_string()), status, is_final);
    let result = serde_json::to_value(&evt).unwrap_or(Value::Null);
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
    let input = match extract_text(&params) {
        Some(t) => t,
        None => return rpc_err(id, -32602, "No text found in message parts"),
    };
    let session_id = match engine.events.create_session(&engine.model) {
        Ok(s) => s,
        Err(e) => return rpc_err(id, -32603, format!("Session error: {e}")),
    };
    let context_id = extract_context_id(&params).unwrap_or_else(|| session_id.clone());
    let mut sink = Collect::default();
    match engine.run_turn(&session_id, &input, &mut sink).await {
        Ok(()) => {
            let status = TaskStatus::new(
                TaskState::Completed,
                Some(Message::agent_text(sink.text)),
                Some(now_rfc3339()),
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
    let input = extract_text(&params).ok_or_else(|| "No text in message parts".to_string())?;
    // TODO: sessions created for A2A tasks are never explicitly pruned. Add a TTL-based cleanup
    // pass (e.g. `DELETE FROM sessions WHERE created_at_ms < now - 3_600_000`) on a background timer
    // in `serve_on`, or expire them inside `SessionStore::create_session`.
    let session_id = engine
        .events
        .create_session(&engine.model)
        .map_err(|e| e.to_string())?;
    let context_id = extract_context_id(&params).unwrap_or_else(|| session_id.clone());
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

// ── Timestamp ─────────────────────────────────────────────────────────────────

/// ISO 8601 / RFC 3339 UTC timestamp from `SystemTime` — no external deps.
/// Uses Howard Hinnant's civil-from-days algorithm (2013).
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let s = secs % 60;
    let min = (secs / 60) % 60;
    let h = (secs / 3_600) % 24;
    // civil_from_days
    let z = secs / 86_400 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // 0 <= doe < 146097
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_rfc3339_looks_valid() {
        let ts = now_rfc3339();
        // Basic shape: 2024-01-01T00:00:00Z
        assert_eq!(ts.len(), 20, "unexpected length: {ts}");
        assert!(ts.ends_with('Z'), "must end with Z: {ts}");
        assert!(ts.contains('T'), "must contain T: {ts}");
        // Year must be >= 2024
        let year: u32 = ts[..4].parse().unwrap();
        assert!(year >= 2024, "year looks wrong: {ts}");
    }

    #[test]
    fn extract_text_happy_path() {
        let params = serde_json::json!({
            "message": {
                "kind": "message",
                "messageId": "m1",
                "role": "user",
                "parts": [{ "kind": "text", "text": "hello flux" }]
            }
        });
        assert_eq!(extract_text(&params).as_deref(), Some("hello flux"));
    }

    #[test]
    fn extract_text_missing_returns_none() {
        let params = serde_json::json!({ "message": { "role": "user", "parts": [] } });
        assert!(extract_text(&params).is_none());
    }

    #[test]
    fn extract_text_ignores_non_text_parts() {
        let params = serde_json::json!({
            "message": { "parts": [{ "kind": "file", "file": {} }] }
        });
        assert!(extract_text(&params).is_none());
    }

    #[test]
    fn extract_context_id_reads_message() {
        let params = serde_json::json!({ "message": { "contextId": "ctx-7", "parts": [] } });
        assert_eq!(extract_context_id(&params).as_deref(), Some("ctx-7"));
        let none = serde_json::json!({ "message": { "parts": [] } });
        assert!(extract_context_id(&none).is_none());
    }
}

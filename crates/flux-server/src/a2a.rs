//! A2A (Agent-to-Agent) protocol support for `flux-server`.
//!
//! Adds two routes to the router:
//!   `GET  /.well-known/agent.json` — agent discovery card (A2A spec §4)
//!   `POST /a2a`                  — JSON-RPC 2.0 dispatcher
//!
//! Supported methods:
//! - `tasks/send`          — run one flux turn, return result synchronously
//! - `tasks/sendSubscribe` — run one flux turn, stream updates as Server-Sent Events
//!
//! Each task creates a fresh session (stateless A2A mode).  The agent card is
//! exempt from bearer-token auth so external agents can discover flux without a key.

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

use flux_agent::AgentSink;

use super::Collect;
use crate::Shared;

// ── Agent Card ────────────────────────────────────────────────────────────────

/// `GET /.well-known/agent.json` — A2A discovery endpoint.
///
/// The `url` field points to the `/a2a` JSON-RPC endpoint on the same host,
/// derived from the request's `Host` (and `X-Forwarded-Proto`) headers so the
/// card is correct whether accessed directly or through a reverse proxy.
pub async fn agent_card(headers: HeaderMap) -> Json<Value> {
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    let url = format!("{scheme}://{host}/a2a");

    Json(json!({
        "name": "flux",
        "description": "flux — a precise, autonomous coding agent. Reads, writes, edits, \
                         searches, and runs code in a workspace. Carries tasks from \
                         instruction to verified completion through a deterministic \
                         Flux-Lang plan + guarded safety envelope.",
        "url": url,
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "stateTransitionHistory": false
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{
            "id": "coding",
            "name": "Coding Agent",
            "description": "Read, write, edit, search, and execute code tasks in a \
                            workspace. The agent plans, executes, and verifies — \
                            then reports back.",
            "inputModes": ["text/plain"],
            "outputModes": ["text/plain"]
        }]
    }))
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

/// Pull all text parts out of an A2A `message` object.
fn extract_text(params: &Value) -> Option<String> {
    let parts = params.get("message")?.get("parts")?.as_array()?;
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|p| {
            if p.get("type")?.as_str()? == "text" {
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

fn agent_message(text: impl Into<String>) -> Value {
    json!({ "role": "agent", "parts": [{ "type": "text", "text": text.into() }] })
}

fn task_status(task_id: &str, state: &str, message: Option<Value>, done: bool) -> Value {
    json!({
        "id": task_id,
        "status": {
            "state": state,
            "message": message,
            "timestamp": now_rfc3339()
        },
        "final": done
    })
}

// ── Main dispatcher ───────────────────────────────────────────────────────────

/// `POST /a2a` — JSON-RPC 2.0 endpoint.
///
/// - `tasks/send`          → [`send`] (synchronous, returns JSON)
/// - `tasks/sendSubscribe` → [`subscribe`] (SSE stream)
pub async fn a2a_handler(
    State(engine): State<Shared>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if req.jsonrpc != "2.0" {
        return rpc_err(req.id, -32600, "jsonrpc must be \"2.0\"").into_response();
    }
    match req.method.as_str() {
        "tasks/send" => send(engine, req.id, req.params).await.into_response(),
        "tasks/sendSubscribe" => match subscribe(engine, req.params).await {
            Ok(sse) => sse.into_response(),
            // Format pre-SSE errors as JSON-RPC so the `id` is not silently dropped.
            Err(msg) => rpc_err(req.id, -32602, msg).into_response(),
        },
        m => rpc_err(req.id, -32601, format!("Method not found: {m}")).into_response(),
    }
}

// ── tasks/send ────────────────────────────────────────────────────────────────

async fn send(engine: Shared, id: Option<Value>, params: Option<Value>) -> Json<Value> {
    let params = match params {
        Some(p) => p,
        None => return rpc_err(id, -32602, "Missing params"),
    };
    let task_id = params
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("task-1")
        .to_string();
    let input = match extract_text(&params) {
        Some(t) => t,
        None => return rpc_err(id, -32602, "No text found in message parts"),
    };
    let session_id = match engine.events.create_session(&engine.model) {
        Ok(s) => s,
        Err(e) => return rpc_err(id, -32603, format!("Session error: {e}")),
    };
    let mut sink = Collect::default();
    match engine.run_turn(&session_id, &input, &mut sink).await {
        Ok(()) => rpc_json(
            id,
            task_status(&task_id, "completed", Some(agent_message(sink.text)), true),
        ),
        Err(e) => rpc_err(id, -32603, format!("Agent error: {e}")),
    }
}

// ── tasks/sendSubscribe ───────────────────────────────────────────────────────

async fn subscribe(
    engine: Shared,
    params: Option<Value>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, String> {
    let params = params.ok_or_else(|| "Missing params".to_string())?;
    let task_id = params
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("task-1")
        .to_string();
    let input = extract_text(&params).ok_or_else(|| "No text in message parts".to_string())?;
    // TODO: sessions created for A2A tasks are never explicitly pruned. Add a TTL-based
    // cleanup pass (e.g. `DELETE FROM sessions WHERE created_at_ms < now - 3_600_000`) on a
    // background timer in `serve_on`, or expire them inside `SessionStore::create_session`.
    let session_id = engine
        .events
        .create_session(&engine.model)
        .map_err(|e| e.to_string())?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    // `drop_guard` cancels `cancel` when the SSE stream is dropped (client disconnect),
    // which propagates through `cancel_task` into `run_turn_cancellable`.
    let cancel = CancellationToken::new();
    let cancel_task = cancel.clone();
    let drop_guard = cancel.drop_guard();

    let engine_clone = engine.clone();
    tokio::spawn(async move {
        // Initial "working" update so the caller knows the task started.
        let _ = tx.send(sse_update(&task_id, "working", None, false));
        let mut sink = StreamSink {
            tx: tx.clone(),
            task_id: task_id.clone(),
            acc: String::new(),
            cancel: cancel_task.clone(),
        };
        let result = engine_clone
            .run_turn_cancellable(&session_id, &input, &mut sink, &cancel_task)
            .await;
        // If the client disconnected mid-stream, skip the final event — nobody is listening.
        if !cancel_task.is_cancelled() {
            let text = std::mem::take(&mut sink.acc);
            let (state, msg) = match result {
                Ok(()) => ("completed", Some(agent_message(text))),
                Err(e) => ("failed", Some(agent_message(e.to_string()))),
            };
            let _ = tx.send(sse_update(&task_id, state, msg, true));
        }
        // `tx` (and sink.tx clone) drop here → channel closes → stream ends.
    });

    let stream = async_stream::stream! {
        // Keep the drop guard alive for the stream's lifetime: when axum drops the SSE
        // response (TCP disconnect), `_guard` fires and cancels the in-flight turn.
        let _guard = drop_guard;
        while let Some(ev) = rx.recv().await {
            yield Ok(ev);
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn sse_update(task_id: &str, state: &str, message: Option<Value>, done: bool) -> Event {
    Event::default()
        .event("task_status_update")
        .data(task_status(task_id, state, message, done).to_string())
}

/// Streams text deltas back as SSE working updates; accumulates the full reply
/// so the spawner can send the final `completed` event.
struct StreamSink {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
    task_id: String,
    /// Full accumulated text; read by the spawner after the turn to build the `completed` event.
    acc: String,
    /// Cancelled when the SSE receiver is dropped (client disconnect); checked between plan
    /// rounds by `run_turn_cancellable`.
    cancel: CancellationToken,
}

impl AgentSink for StreamSink {
    fn text_delta(&mut self, t: &str) {
        self.acc.push_str(t);
        // Send only the delta in working events; the full text goes in the final `completed`
        // event. Sending `self.acc.clone()` on every token is O(N²) in response length.
        if self
            .tx
            .send(sse_update(
                &self.task_id,
                "working",
                Some(agent_message(t)),
                false,
            ))
            .is_err()
        {
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
            "id": "t1",
            "message": {
                "role": "user",
                "parts": [{"type": "text", "text": "hello flux"}]
            }
        });
        assert_eq!(extract_text(&params).as_deref(), Some("hello flux"));
    }

    #[test]
    fn extract_text_missing_returns_none() {
        let params = serde_json::json!({ "id": "t1", "message": { "role": "user", "parts": [] } });
        assert!(extract_text(&params).is_none());
    }
}

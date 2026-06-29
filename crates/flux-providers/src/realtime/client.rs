//! The OpenAI Realtime WebSocket transport (provider-internal).
//!
//! Lifted from the managed-agents `realtime` crate, retargeted onto `flux_core::{Error, Result}`.
//! [`connect_ws`] opens the session and returns a cloneable [`RealtimeHandle`] (send audio / control /
//! tool output) plus a receiver of [`ServerEvent`]s. Audio crosses the wire as base64 PCM16 inside
//! JSON events.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use flux_core::{Error, Result};

use super::config::{ClientConfig, SessionConfig};
use super::event::ServerEvent;

/// A cloneable handle for sending to the realtime session.
#[derive(Clone)]
pub struct RealtimeHandle {
    out: mpsc::Sender<String>,
    token: CancellationToken,
}

impl RealtimeHandle {
    async fn send_json(&self, v: Value) -> Result<()> {
        self.out
            .send(v.to_string())
            .await
            .map_err(|_| Error::Provider("realtime session closed".into()))
    }

    /// Reconfigure the session (`session.update`), rendered in the GA session shape.
    pub async fn update_session(&self, session: &SessionConfig) -> Result<()> {
        self.send_json(json!({ "type": "session.update", "session": session.to_ga_session() }))
            .await
    }

    /// Append a chunk of input audio (PCM16) to the model's buffer.
    pub async fn append_audio(&self, pcm: &[u8]) -> Result<()> {
        let audio = STANDARD.encode(pcm);
        self.send_json(json!({ "type": "input_audio_buffer.append", "audio": audio }))
            .await
    }

    /// Commit the input audio buffer (used without server-VAD).
    pub async fn commit_audio(&self) -> Result<()> {
        self.send_json(json!({ "type": "input_audio_buffer.commit" }))
            .await
    }

    /// Ask the model to produce a response.
    pub async fn create_response(&self) -> Result<()> {
        self.send_json(json!({ "type": "response.create" })).await
    }

    /// Cancel the in-progress response (barge-in). The provider replies `response_cancel_not_active`
    /// if no response was active; that benign error is swallowed in the event mapping, so this is
    /// effectively idempotent.
    pub async fn cancel_response(&self) -> Result<()> {
        self.send_json(json!({ "type": "response.cancel" })).await
    }

    /// Return a tool call's output to the model. Call [`RealtimeHandle::create_response`] afterwards
    /// (once per turn) to have the model continue.
    pub async fn send_function_output(&self, call_id: &str, output: &str) -> Result<()> {
        self.send_json(json!({
            "type": "conversation.item.create",
            "item": { "type": "function_call_output", "call_id": call_id, "output": output }
        }))
        .await
    }

    /// Inject a user text message and request a response (DTMF-as-text, tests, flow-driven turns).
    pub async fn send_user_text(&self, text: &str) -> Result<()> {
        self.send_json(json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }]
            }
        }))
        .await?;
        self.create_response().await
    }

    /// Close the session.
    pub fn close(&self) {
        self.token.cancel();
    }
}

/// Connect to the OpenAI Realtime API and start the session.
///
/// Returns a [`RealtimeHandle`] and a receiver of [`ServerEvent`]s. If `config.session` is set, a
/// `session.update` is sent before returning.
pub async fn connect_ws(
    config: ClientConfig,
) -> Result<(RealtimeHandle, mpsc::Receiver<ServerEvent>)> {
    let url = format!("wss://api.openai.com/v1/realtime?model={}", config.model);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| Error::Http(e.to_string()))?;
    {
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", config.api_key))
                .map_err(|e| Error::Auth(e.to_string()))?,
        );
        // GA `/v1/realtime`: no `OpenAI-Beta: realtime=v1` header — sending it selects the retired
        // beta shape (rejected with `beta_api_shape_disabled`).
    }

    let (ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    let (mut sink, mut stream) = ws.split();

    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (ev_tx, ev_rx) = mpsc::channel::<ServerEvent>(256);
    let token = CancellationToken::new();

    // Writer: drain outgoing JSON to the socket.
    {
        let token = token.clone();
        tokio::spawn(async move {
            // Periodically flush so auto-queued WebSocket Pongs are sent during outbound silence.
            let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(15));
            keepalive.tick().await; // consume the immediate first tick
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = keepalive.tick() => {
                        if sink.flush().await.is_err() {
                            break;
                        }
                    }
                    msg = out_rx.recv() => match msg {
                        Some(text) => {
                            if sink.send(Message::Text(text)).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
            let _ = sink.close().await;
            token.cancel();
        });
    }

    // Reader: parse server events and forward them.
    {
        let token = token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    msg = stream.next() => match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(ev) = ServerEvent::parse(&text) {
                                if ev_tx.send(ev).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => continue,
                        Some(Err(_)) => break,
                    }
                }
            }
            token.cancel();
        });
    }

    let handle = RealtimeHandle { out: out_tx, token };
    if let Some(session) = &config.session {
        handle.update_session(session).await?;
    }
    Ok((handle, ev_rx))
}

//! [`A2aClient`] — an HTTP + JSON-RPC 2.0 client for driving a remote A2A agent.
//!
//! It speaks the current A2A spec: discover via `/.well-known/agent-card.json`, then `message/send`
//! (blocking) or `message/stream` (SSE) per turn, with `tasks/get` as the completion path for
//! agents that answer `message/send` with a still-running task. SSE is decoded with
//! `eventsource-stream` (the same crate the provider transports use).

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::types::{
    AgentCard, JsonRpcRequest, JsonRpcResponse, Message, SendConfiguration, SendMessageParams,
    SendOutcome, StreamEvent, Task, TaskGetParams,
};

/// Errors surfaced by the A2A client.
#[derive(Debug, thiserror::Error)]
pub enum A2aError {
    #[error("http error: {0}")]
    Http(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("decode error: {0}")]
    Decode(String),
    #[error("{0}")]
    Status(String),
    #[error("invalid url: {0}")]
    Url(String),
}

pub type Result<T> = std::result::Result<T, A2aError>;

/// A boxed stream of decoded streaming events.
pub type EventStream = std::pin::Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

/// A client bound to one remote A2A agent.
pub struct A2aClient {
    http: reqwest::Client,
    /// Origin base (`scheme://host[:port]/`) — the root the well-known card paths hang off.
    base: Url,
    /// Where JSON-RPC requests are POSTed. Defaults to `<base>/a2a`; replaced by the card's
    /// advertised endpoint once [`A2aClient::fetch_agent_card`] adopts it.
    rpc_url: Url,
    token: Option<String>,
    headers: Vec<(String, String)>,
}

impl A2aClient {
    /// Build a client from a base URL or a full RPC URL. A bare origin (`http://host:port`) targets
    /// `<origin>/a2a`; a URL with a path (`…/a2a`) is used verbatim as the RPC endpoint.
    pub fn new(input: &str) -> Result<Self> {
        let parsed = Url::parse(input).map_err(|e| A2aError::Url(format!("{input}: {e}")))?;
        let mut base = parsed.clone();
        base.set_path("/");
        base.set_query(None);
        base.set_fragment(None);
        let rpc_url = if parsed.path().trim_matches('/').is_empty() {
            base.join("a2a")
                .map_err(|e| A2aError::Url(format!("{input}: {e}")))?
        } else {
            parsed
        };
        Ok(A2aClient {
            http: reqwest::Client::new(),
            base,
            rpc_url,
            token: None,
            headers: Vec::new(),
        })
    }

    /// Attach a bearer token (sent as `Authorization: Bearer …`) for gated endpoints.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.is_empty());
        self
    }

    /// Attach an extra header to every request.
    pub fn with_header(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.headers.push((key.into(), val.into()));
        self
    }

    /// Override the JSON-RPC endpoint (e.g. from a fetched [`AgentCard`]).
    pub fn with_rpc_url(mut self, url: &str) -> Result<Self> {
        self.rpc_url = Url::parse(url).map_err(|e| A2aError::Url(format!("{url}: {e}")))?;
        Ok(self)
    }

    /// The current JSON-RPC endpoint.
    pub fn rpc_url(&self) -> &str {
        self.rpc_url.as_str()
    }

    fn auth(&self, mut rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(t) = &self.token {
            rb = rb.bearer_auth(t);
        }
        for (k, v) in &self.headers {
            rb = rb.header(k, v);
        }
        rb
    }

    /// Decide which RPC endpoint to adopt from an agent card. Honors the advertised endpoint,
    /// unless a (mis)configured card advertises a **loopback** endpoint while we actually reached
    /// the agent over a non-loopback host — a common container/reverse-proxy footgun. In that case
    /// keep the host we connected to and borrow only the card's path.
    fn adopt_endpoint(&self, advertised: Url) -> Url {
        if is_loopback_host(advertised.host_str()) && !is_loopback_host(self.base.host_str()) {
            if let Ok(joined) = self.base.join(advertised.path()) {
                return joined;
            }
        }
        advertised
    }

    /// Fetch the agent card, trying the newer `agent-card.json` path then the older `agent.json`.
    /// Adopts the card's advertised RPC endpoint as [`A2aClient::rpc_url`] for subsequent calls.
    pub async fn fetch_agent_card(&mut self) -> Result<AgentCard> {
        let mut last_err = A2aError::Status("agent card not found".to_string());
        for path in [".well-known/agent-card.json", ".well-known/agent.json"] {
            let url = self
                .base
                .join(path)
                .map_err(|e| A2aError::Url(e.to_string()))?;
            let rb = self.auth(self.http.get(url));
            match rb.send().await {
                Ok(resp) if resp.status().is_success() => {
                    let card: AgentCard = resp
                        .json()
                        .await
                        .map_err(|e| A2aError::Decode(e.to_string()))?;
                    if let Some(ep) = card.rpc_endpoint() {
                        if let Ok(u) = Url::parse(&ep) {
                            self.rpc_url = self.adopt_endpoint(u);
                        }
                    }
                    return Ok(card);
                }
                Ok(resp) => last_err = A2aError::Status(format!("{path}: HTTP {}", resp.status())),
                Err(e) => last_err = A2aError::Http(e.to_string()),
            }
        }
        Err(last_err)
    }

    /// One JSON-RPC round-trip, deserializing `result` into `T`.
    async fn rpc<P: Serialize, T: DeserializeOwned>(&self, method: &str, params: P) -> Result<T> {
        let req = JsonRpcRequest::new(method, params);
        let rb = self.auth(self.http.post(self.rpc_url.clone()).json(&req));
        let resp = rb.send().await.map_err(|e| A2aError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(A2aError::Http(format!("HTTP {status}: {body}")));
        }
        let envelope: JsonRpcResponse<T> = resp
            .json()
            .await
            .map_err(|e| A2aError::Decode(e.to_string()))?;
        if let Some(e) = envelope.error {
            return Err(A2aError::Rpc {
                code: e.code,
                message: e.message,
            });
        }
        envelope
            .result
            .ok_or_else(|| A2aError::Decode("response had neither result nor error".to_string()))
    }

    /// `message/send` — send a message and get back a [`Task`] or a [`Message`]. With `blocking`,
    /// ask the agent to run to completion before responding.
    pub async fn send(&self, message: Message, blocking: bool) -> Result<SendOutcome> {
        let params = SendMessageParams {
            message,
            configuration: Some(SendConfiguration { blocking }),
        };
        let v: Value = self.rpc("message/send", params).await?;
        SendOutcome::from_value(v).map_err(|e| A2aError::Decode(e.to_string()))
    }

    /// `tasks/get` — fetch a task's current state. Used to poll an async agent to completion.
    pub async fn get_task(&self, id: &str) -> Result<Task> {
        self.rpc("tasks/get", TaskGetParams { id: id.to_string() })
            .await
    }

    /// Poll `tasks/get` until the task reaches a terminal state (or `max_polls` is hit).
    pub async fn await_task(&self, id: &str, interval: Duration, max_polls: usize) -> Result<Task> {
        let mut task = self.get_task(id).await?;
        let mut n = 0;
        while !task.status.state.is_terminal() && n < max_polls {
            tokio::time::sleep(interval).await;
            task = self.get_task(id).await?;
            n += 1;
        }
        Ok(task)
    }

    /// `message/stream` — stream the turn as Server-Sent Events, decoded into [`StreamEvent`]s.
    /// The SSE `event:` name is ignored; every `data:` frame is parsed as a JSON-RPC response whose
    /// `result` is a Task / Message / status-update / artifact-update.
    pub async fn stream(&self, message: Message) -> Result<EventStream> {
        let params = SendMessageParams {
            message,
            configuration: None,
        };
        let req = JsonRpcRequest::new("message/stream", params);
        let rb = self
            .auth(self.http.post(self.rpc_url.clone()).json(&req))
            .header("accept", "text/event-stream");
        let resp = rb.send().await.map_err(|e| A2aError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(A2aError::Http(format!("HTTP {status}: {body}")));
        }
        // A `2xx` that isn't an event stream is almost always a JSON-RPC error body (e.g. the agent
        // doesn't support `message/stream`). Surface it instead of silently yielding no events.
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("text/event-stream"))
            .unwrap_or(false);
        if !is_sse {
            let body = resp.text().await.unwrap_or_default();
            if let Ok(env) = serde_json::from_str::<JsonRpcResponse<Value>>(&body) {
                if let Some(e) = env.error {
                    return Err(A2aError::Rpc {
                        code: e.code,
                        message: e.message,
                    });
                }
            }
            let snippet: String = body.chars().take(200).collect();
            return Err(A2aError::Decode(format!(
                "message/stream did not return an event stream: {snippet}"
            )));
        }

        let stream = resp
            .bytes_stream()
            .eventsource()
            .filter_map(|ev| async move {
                match ev {
                    Ok(ev) => {
                        let data = ev.data.trim();
                        if data.is_empty() || data == "[DONE]" {
                            return None; // keepalive / sentinel
                        }
                        match serde_json::from_str::<JsonRpcResponse<Value>>(data) {
                            Ok(env) => {
                                if let Some(e) = env.error {
                                    return Some(Err(A2aError::Rpc {
                                        code: e.code,
                                        message: e.message,
                                    }));
                                }
                                env.result.map(|v| {
                                    StreamEvent::from_value(v)
                                        .map_err(|e| A2aError::Decode(e.to_string()))
                                })
                            }
                            Err(e) => Some(Err(A2aError::Decode(e.to_string()))),
                        }
                    }
                    Err(e) => Some(Err(A2aError::Decode(e.to_string()))),
                }
            });
        Ok(Box::pin(stream))
    }
}

/// True for hosts that only reach the local machine — used to spot a card that advertises a
/// loopback endpoint we couldn't actually have reached from a remote host.
fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(
        host,
        Some("127.0.0.1" | "localhost" | "::1" | "0.0.0.0" | "[::1]")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_host_detection() {
        assert!(is_loopback_host(Some("127.0.0.1")));
        assert!(is_loopback_host(Some("localhost")));
        assert!(!is_loopback_host(Some("example.com")));
        assert!(!is_loopback_host(None));
    }

    #[test]
    fn adopts_advertised_endpoint_on_same_host() {
        let c = A2aClient::new("http://example.com:9000").unwrap();
        let adopted = c.adopt_endpoint(Url::parse("http://example.com:9000/custom/a2a").unwrap());
        assert_eq!(adopted.as_str(), "http://example.com:9000/custom/a2a");
    }

    #[test]
    fn keeps_connected_host_when_card_advertises_loopback() {
        // A container/proxy footgun: card says 127.0.0.1 but we reached it via a real host.
        let c = A2aClient::new("https://agent.example.com").unwrap();
        let adopted = c.adopt_endpoint(Url::parse("http://127.0.0.1:8080/a2a").unwrap());
        assert_eq!(adopted.host_str(), Some("agent.example.com"));
        assert_eq!(adopted.path(), "/a2a");
        assert_eq!(adopted.scheme(), "https");
    }

    #[test]
    fn local_dev_loopback_is_honored() {
        // Both sides loopback (normal local dev): adopt as-is.
        let c = A2aClient::new("http://127.0.0.1:8787").unwrap();
        let adopted = c.adopt_endpoint(Url::parse("http://127.0.0.1:8787/a2a").unwrap());
        assert_eq!(adopted.as_str(), "http://127.0.0.1:8787/a2a");
    }
}

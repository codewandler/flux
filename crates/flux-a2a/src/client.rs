//! [`A2aClient`] — an HTTP + JSON-RPC 2.0 client for driving a remote A2A agent.
//!
//! It speaks the current A2A spec: discover via `/.well-known/agent-card.json`, then `message/send`
//! (blocking) or `message/stream` (SSE) per turn, with `tasks/get` as the completion path for
//! agents that answer `message/send` with a still-running task. SSE is decoded with
//! `eventsource-stream` (the same crate the provider transports use).

use std::net::SocketAddr;
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
    /// A pinned client is origin-locked: card adoption and explicit RPC overrides may change only
    /// the path, never escape to a hostname that was not part of the vetted address binding.
    origin_locked: bool,
}

impl A2aClient {
    /// Build a client from a base URL or a full RPC URL. A bare origin (`http://host:port`) targets
    /// `<origin>/a2a`; a URL with a path (`…/a2a`) is used verbatim as the RPC endpoint.
    pub fn new(input: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| A2aError::Http(format!("building A2A client: {e}")))?;
        Self::with_http(input, http, false)
    }

    /// Build an origin-locked client that connects only to `pinned` socket addresses.
    ///
    /// The caller must obtain this set from its egress authorization boundary (fleet uses
    /// `flux_system::net::guard_url_scoped_pinned`). Empty sets fail closed. Redirects are disabled,
    /// and later agent-card or explicit RPC endpoint adoption cannot switch origins, so every
    /// request made by this client remains bound to the addresses that were vetted here.
    pub fn new_pinned(input: &str, pinned: &[SocketAddr]) -> Result<Self> {
        if pinned.is_empty() {
            return Err(A2aError::Http(
                "refusing to build an unpinned A2A client: the egress guard vetted no addresses"
                    .to_string(),
            ));
        }
        let parsed = Url::parse(input).map_err(|e| A2aError::Url(format!("{input}: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| A2aError::Url(format!("{input}: url has no host")))?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // A guarded/pinned transport must connect to the vetted addresses directly. Ambient
            // proxy variables would route the request to an unvetted peer and move DNS resolution
            // behind that peer, defeating the authorization decision.
            .no_proxy()
            .resolve_to_addrs(host, pinned)
            .build()
            .map_err(|e| A2aError::Http(format!("building pinned A2A client: {e}")))?;
        Self::with_http(input, http, true)
    }

    fn with_http(input: &str, http: reqwest::Client, origin_locked: bool) -> Result<Self> {
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
            http,
            base,
            rpc_url,
            token: None,
            headers: Vec::new(),
            origin_locked,
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
        let parsed = Url::parse(url).map_err(|e| A2aError::Url(format!("{url}: {e}")))?;
        if self.origin_locked && !same_origin(&self.base, &parsed) {
            return Err(A2aError::Url(format!(
                "{url}: a pinned A2A client cannot change origin"
            )));
        }
        self.rpc_url = parsed;
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
        if self.origin_locked && !same_origin(&self.base, &advertised) {
            return self.endpoint_path_on_base(&advertised);
        }
        if is_loopback_host(advertised.host_str()) && !is_loopback_host(self.base.host_str()) {
            return self.endpoint_path_on_base(&advertised);
        }
        advertised
    }

    /// Retain only an advertised endpoint's path/query on the connected origin. Mutating the URL
    /// components directly is deliberate: `Url::join("//attacker")` treats that path as a new
    /// authority, which would defeat the origin lock it was meant to preserve.
    fn endpoint_path_on_base(&self, advertised: &Url) -> Url {
        let mut endpoint = self.base.clone();
        endpoint.set_path(advertised.path());
        endpoint.set_query(advertised.query());
        endpoint
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

    /// A plain JSON GET against a **surface** route hanging off the same authenticated origin as
    /// the A2A endpoint, returning `(status, body)`; a non-JSON body decodes as `Value::Null`.
    ///
    /// A2A itself is entirely JSON-RPC over one endpoint, so this is deliberately not part of the
    /// protocol surface — it exists because a flux-served agent mounts C-453's `/approvals` beside
    /// `/a2a` under the *same* bearer credential, and [`crate::attach`] must read that posture with
    /// the client that already holds the credential and the origin lock. The status code is
    /// returned rather than folded into an error because the interesting answers here (`501` "this
    /// server asks nobody", `401` "not with this credential") are postures to report, not failures.
    pub(crate) async fn origin_get(&self, path: &str) -> Result<(u16, Value)> {
        let url = self
            .base
            .join(path.trim_start_matches('/'))
            .map_err(|e| A2aError::Url(format!("{path}: {e}")))?;
        let resp = self
            .auth(self.http.get(url))
            .send()
            .await
            .map_err(|e| A2aError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
    }

    /// [`A2aClient::origin_get`]'s write half: POST `body` as JSON, returning `(status, body)`.
    pub(crate) async fn origin_post(&self, path: &str, body: &Value) -> Result<(u16, Value)> {
        let url = self
            .base
            .join(path.trim_start_matches('/'))
            .map_err(|e| A2aError::Url(format!("{path}: {e}")))?;
        let resp = self
            .auth(self.http.post(url).json(body))
            .send()
            .await
            .map_err(|e| A2aError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// `message/send` — send a message and get back a [`Task`] or a [`Message`]. With `blocking`,
    /// ask the agent to run to completion before responding.
    pub async fn send(&self, message: Message, blocking: bool) -> Result<SendOutcome> {
        let params = SendMessageParams {
            message,
            configuration: Some(SendConfiguration {
                blocking,
                ..Default::default()
            }),
        };
        let v: Value = self.rpc("message/send", params).await?;
        SendOutcome::from_value(v).map_err(|e| A2aError::Decode(e.to_string()))
    }

    /// `tasks/get` — fetch a task's current state. Used to poll an async agent to completion.
    pub async fn get_task(&self, id: &str) -> Result<Task> {
        self.rpc("tasks/get", TaskGetParams { id: id.to_string() })
            .await
    }

    /// `tasks/cancel` — ask the remote agent to abort a live task, returning it in its requested
    /// `canceled` state. This is the client half of the server's A-55 cancel path: it fires the
    /// token the run observes between plan rounds, so it aborts work that is genuinely still in
    /// flight rather than merely detaching this client from it.
    ///
    /// A task that is already terminal, or whose run lives on another replica, answers the A2A
    /// `TaskNotCancelable` error (`-32002`) as [`A2aError::Rpc`] — a benign outcome for a caller
    /// that is cancelling opportunistically, not a transport failure.
    ///
    /// Only the **served** dispatch implements this. `flux_a2a::server::is_unsupported_a2a_method`
    /// still classifies `tasks/cancel` as unsupported in the reduced *embeddable* dispatch, so a
    /// remote agent must be served by `flux serve` / flux-server for this to resolve.
    pub async fn cancel_task(&self, id: &str) -> Result<Task> {
        self.rpc("tasks/cancel", TaskGetParams { id: id.to_string() })
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
        self.stream_rpc("message/stream", params).await
    }

    /// `tasks/resubscribe` — re-attach an SSE stream to a task that is already running (or already
    /// finished and still retained), without starting a turn.
    ///
    /// This is the reattach half of an attached session: a resubscriber is an **observer**, so
    /// dropping the returned stream cancels nothing on the far side, unlike the stream
    /// [`A2aClient::stream`] owns. A live task yields a snapshot frame and then follows to the
    /// terminal one; a retained terminal task yields its final frame and closes.
    ///
    /// Only the **served** dispatch implements this; the reduced embeddable dispatch classifies it
    /// unsupported (`-32004`), which arrives as [`A2aError::Rpc`].
    pub async fn resubscribe(&self, task_id: &str) -> Result<EventStream> {
        self.stream_rpc(
            "tasks/resubscribe",
            TaskGetParams {
                id: task_id.to_string(),
            },
        )
        .await
    }

    /// The shared body of every SSE-returning JSON-RPC call: POST `method` with `params`, insist on
    /// an `text/event-stream` response, and decode each `data:` frame's JSON-RPC `result` into a
    /// [`StreamEvent`]. Both `message/stream` and `tasks/resubscribe` are exactly this call with a
    /// different method name, so the non-SSE refusal and the frame decoding cannot drift apart.
    async fn stream_rpc<P: Serialize>(&self, method: &str, params: P) -> Result<EventStream> {
        let req = JsonRpcRequest::new(method, params);
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
                "{method} did not return an event stream: {snippet}"
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

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str()
            .zip(b.host_str())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        && a.port_or_known_default() == b.port_or_known_default()
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

    #[test]
    fn pinned_clients_fail_closed_and_cannot_change_origin() {
        assert!(A2aClient::new_pinned("https://worker.example", &[]).is_err());
        let client = A2aClient::new_pinned(
            "https://worker.example",
            &["93.184.216.34:443".parse().unwrap()],
        )
        .unwrap();
        assert!(client.with_rpc_url("https://attacker.example/a2a").is_err());

        let client = A2aClient::new_pinned(
            "https://worker.example",
            &["93.184.216.34:443".parse().unwrap()],
        )
        .unwrap();
        let adopted = client
            .adopt_endpoint(Url::parse("https://attacker.example//other-host/a2a?x=1").unwrap());
        assert_eq!(adopted.host_str(), Some("worker.example"));
        assert_eq!(adopted.path(), "//other-host/a2a");
        assert_eq!(adopted.query(), Some("x=1"));
    }

    /// C-256: reqwest must never follow an A2A redirect behind the fleet guard. The source is
    /// reached through a fake hostname pinned to loopback; the redirect target must receive zero
    /// connections even though it is otherwise reachable.
    #[tokio::test]
    async fn pinned_client_never_follows_automatic_redirects() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let reached = Arc::new(AtomicBool::new(false));
        let reached_task = reached.clone();
        let target_task = tokio::spawn(async move {
            if tokio::time::timeout(Duration::from_millis(250), target.accept())
                .await
                .is_ok()
            {
                reached_task.store(true, Ordering::SeqCst);
            }
        });

        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_addr = source.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/a2a\r\nContent-Length: 0\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let client = A2aClient::new_pinned(
            &format!("http://worker.test:{}/a2a", source_addr.port()),
            &[source_addr],
        )
        .unwrap();
        let err = client
            .get_task("t_1")
            .await
            .expect_err("a redirect response is not an authorized A2A response");
        assert!(err.to_string().contains("HTTP 302"), "{err}");
        target_task.await.unwrap();
        assert!(!reached.load(Ordering::SeqCst));
    }

    /// C-256 closure: address pinning is meaningless if reqwest may route the request through an
    /// ambient proxy. Run the network assertion in an isolated test process so changing proxy
    /// variables cannot race sibling tests.
    #[tokio::test]
    async fn pinned_client_ignores_ambient_proxy() {
        const CHILD: &str = "FLUX_A2A_PROXY_REGRESSION_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "client::tests::pinned_client_ignores_ambient_proxy",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated proxy regression failed");
            return;
        }

        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn serve_once(listener: tokio::net::TcpListener, reached: Arc<AtomicBool>) {
            let accepted =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
            let Ok(Ok((mut socket, _))) = accepted else {
                return;
            };
            reached.store(true, Ordering::SeqCst);
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }

        let pinned = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pinned_addr = pinned.local_addr().unwrap();
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let pinned_reached = Arc::new(AtomicBool::new(false));
        let proxy_reached = Arc::new(AtomicBool::new(false));
        let pinned_task = tokio::spawn(serve_once(pinned, pinned_reached.clone()));
        let proxy_task = tokio::spawn(serve_once(proxy, proxy_reached.clone()));

        let proxy_url = format!("http://{proxy_addr}");
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            std::env::set_var(key, &proxy_url);
        }
        std::env::set_var("NO_PROXY", "");
        std::env::set_var("no_proxy", "");

        let client = A2aClient::new_pinned(
            &format!("http://worker.test:{}/a2a", pinned_addr.port()),
            &[pinned_addr],
        )
        .unwrap();
        client
            .http
            .get(client.rpc_url.clone())
            .send()
            .await
            .unwrap();

        pinned_task.await.unwrap();
        proxy_task.await.unwrap();
        assert!(pinned_reached.load(Ordering::SeqCst));
        assert!(!proxy_reached.load(Ordering::SeqCst));
    }

    /// A one-shot loopback JSON-RPC stub: accepts one connection, captures the full request
    /// (headers + `Content-Length` body), and answers with `body` as a JSON-RPC `result`. Returns
    /// its base URL and the join handle whose output is the captured raw request. tokio + std only
    /// — offline, like every other transport fixture in the workspace.
    async fn one_shot_rpc(body: serde_json::Value) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": body }).to_string();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            // Read until the full request (headers + Content-Length body) is in `buf`.
            loop {
                let n = match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(hdr_end) = text.find("\r\n\r\n") {
                    let content_len = text[..hdr_end]
                        .lines()
                        .find_map(|l| {
                            let low = l.to_ascii_lowercase();
                            low.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= hdr_end + 4 + content_len {
                        break;
                    }
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
            String::from_utf8_lossy(&buf).to_string()
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn cancel_task_posts_tasks_cancel_for_the_id() {
        // A-116: the client half of the server's A-55 cancel path. Proves the method name and the
        // `id` param on the wire — a `tasks/cancel` that silently posted the wrong method would
        // leave a remote worker running after the caller believed it had stopped it.
        let canceled = serde_json::json!({
            "kind": "task",
            "id": "t_1",
            "status": { "state": "canceled" },
        });
        let (base, handle) = one_shot_rpc(canceled).await;
        let client = A2aClient::new(&base).unwrap();

        let task = client.cancel_task("t_1").await.unwrap();
        assert_eq!(task.id, "t_1");
        assert_eq!(task.status.state, crate::types::TaskState::Canceled);

        let request = handle.await.unwrap();
        assert!(
            request.contains(r#""method":"tasks/cancel""#),
            "expected a tasks/cancel JSON-RPC call, got: {request}"
        );
        assert!(
            request.contains(r#""id":"t_1""#),
            "expected the task id in params, got: {request}"
        );
    }

    #[tokio::test]
    async fn cancel_task_surfaces_task_not_cancelable_as_an_rpc_error() {
        // The server answers `-32002` for a task that is already terminal. That must arrive as a
        // typed `Rpc` error the caller can treat as benign, not as a decode/transport failure.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let payload = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32002, "message": "task is already in a terminal state" },
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });

        let client = A2aClient::new(&format!("http://{addr}")).unwrap();
        match client.cancel_task("t_done").await {
            Err(A2aError::Rpc { code, .. }) => assert_eq!(code, -32002),
            other => panic!("expected a -32002 Rpc error, got {other:?}"),
        }
    }
}

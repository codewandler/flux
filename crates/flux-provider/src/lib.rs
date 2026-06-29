//! `flux-provider` — the provider abstraction.
//!
//! A [`Provider`] turns a [`Request`] into a stream of [`Chunk`](flux_core::Chunk)s. Concrete
//! clients (Anthropic, OpenAI, Ollama) live in their own crates and implement this trait. The
//! trait is object-safe (via `async_trait`) so the runtime can hold a `Box<dyn Provider>` and
//! swap providers/models at will.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use flux_core::{Chunk, Error, Message, Result};

pub mod realtime;
pub use realtime::{
    RealtimeConfig, RealtimeConnection, RealtimeEvent, RealtimeEventStream, RealtimeProvider,
    RealtimeSession, TurnDetection,
};

/// A boxed, sendable stream of response chunks.
pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<Chunk>> + Send>>;

/// A boxed HTTP response body byte stream, with transport errors normalized to [`Error`].
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// Reasoning effort — controls thinking depth and overall token spend on models
/// that support it (Anthropic `output_config.effort`; mapped per provider). Note
/// that some models reject it (e.g. Anthropic Haiku), so it is always opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// A tool definition advertised to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool's input object.
    pub input_schema: serde_json::Value,
}

/// A provider-agnostic inference request.
#[derive(Debug, Clone)]
pub struct Request {
    /// Concrete model id (already resolved from any alias).
    pub model: String,
    /// Optional system prompt.
    pub system: Option<String>,
    /// Conversation messages.
    pub messages: Vec<Message>,
    /// Tools available to the model.
    pub tools: Vec<ToolDef>,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// Sampling temperature (ignored by some providers when thinking is enabled).
    pub temperature: Option<f32>,
    /// Nucleus sampling parameter.
    pub top_p: Option<f32>,
    /// Stop sequences.
    pub stop_sequences: Vec<String>,
    /// Enable adaptive thinking (the provider decides when/how much to reason).
    pub thinking: bool,
    /// Reasoning effort (depth/cost); provider- and model-dependent, opt-in.
    pub effort: Option<Effort>,
    /// Catch-all for provider-specific parameters.
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

impl Request {
    /// A minimal request: a model plus a single user-text message.
    pub fn new(model: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages: vec![Message::user_text(prompt)],
            tools: Vec::new(),
            max_tokens: 4096,
            temperature: None,
            top_p: None,
            stop_sequences: Vec::new(),
            thinking: false,
            effort: None,
            metadata: serde_json::Map::new(),
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_thinking(mut self, on: bool) -> Self {
        self.thinking = on;
        self
    }

    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }
}

/// An LLM provider capable of streaming a response.
#[async_trait]
pub trait Provider: Send + Sync {
    /// A short, stable provider name (e.g. `"anthropic"`).
    fn name(&self) -> &str;

    /// Stream a response for `req`.
    async fn stream(&self, req: Request) -> Result<ChunkStream>;
}

/// Resolves human-friendly model aliases (e.g. `"sonnet"`, tier names) to concrete ids.
pub trait ModelResolver: Send + Sync {
    fn resolve(&self, alias: &str) -> String;
}

/// Optional capability: count the prompt tokens of a request before sending it.
#[async_trait]
pub trait TokenCounter: Send + Sync {
    async fn count_tokens(&self, req: &Request) -> Result<u64>;
}

/// Axis (a): the **wire protocol** — how a [`Request`] is serialized to a JSON body and
/// how the response byte stream is parsed into [`Chunk`]s. Independent of auth/transport.
/// (Anthropic Messages, OpenAI Chat Completions, OpenAI Responses.)
pub trait WireCodec: Send + Sync {
    /// Serialize the request to the provider's JSON body.
    fn build_body(&self, req: &Request) -> Result<serde_json::Value>;

    /// Parse the response byte stream into normalized chunks.
    fn map_stream(&self, bytes: ByteStream) -> ChunkStream;

    /// Protocol-required headers (e.g. `anthropic-version`). Auth and product-gating
    /// headers belong on the [`Credential`], not here.
    fn wire_headers(&self) -> Vec<(&'static str, String)> {
        Vec::new()
    }
}

/// Axis (b): the **auth/transport profile** — endpoint URL, auth + product-gating headers,
/// and any required system-prompt prefix. May refresh OAuth tokens (hence async).
#[async_trait]
pub trait Credential: Send + Sync {
    /// Full URL to POST the request to.
    fn endpoint(&self) -> String;

    /// Attach auth + gating headers to the request (refreshing tokens if needed).
    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder>;

    /// A system-prompt prefix the transport requires (e.g. subscription gating).
    fn system_prefix(&self) -> Option<String> {
        None
    }

    /// The OAuth [`TokenSource`] backing this credential, if any. API-key credentials return
    /// `None`; OAuth-backed credentials (subscription `claude`/`codex`) return their source so the
    /// generic HTTP path can [force a refresh](TokenSource::refresh) on a `401` without knowing the
    /// concrete credential type.
    fn token_source(&self) -> Option<Arc<dyn TokenSource>> {
        None
    }
}

/// A source of OAuth access tokens that refreshes on demand. Implemented by
/// `flux-credentials`; consumed by OAuth [`Credential`]s in the provider crates.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Return a valid access token, refreshing it lazily when it is near expiry.
    async fn access_token(&self) -> Result<String>;

    fn account_id(&self) -> Option<String> {
        None
    }

    /// Force a token refresh **ignoring the expiry buffer**, persisting the result. Called by the
    /// HTTP path after a `401` to recover a stale/wrong-expiry token. The default is a no-op (for
    /// sources that cannot refresh); concurrent calls must coalesce into a single refresh.
    async fn refresh(&self) -> Result<()> {
        Ok(())
    }
}

/// Default number of retries on transient transport/server errors.
pub const DEFAULT_MAX_RETRIES: u32 = 6;

/// True if an HTTP status warrants a retry: rate limiting (429) or any server error (5xx).
pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// Exponential backoff for `attempt` (0-based): 500ms · 2^attempt, capped at 30s.
pub fn backoff_delay(attempt: u32) -> std::time::Duration {
    let ms = 500u64.saturating_mul(1u64 << attempt.min(6)).min(30_000);
    std::time::Duration::from_millis(ms)
}

/// Composes a [`WireCodec`] (axis a) with a [`Credential`] (axis b) into a [`Provider`].
/// This is the single generic HTTP path; every concrete provider is one (codec, credential) cell.
/// The connection attempt (POST + status check) is retried with exponential backoff on transient
/// transport errors and retryable statuses (429/5xx); mid-stream failures are not retried.
pub struct NativeProvider {
    name: String,
    http: reqwest::Client,
    codec: Arc<dyn WireCodec>,
    cred: Arc<dyn Credential>,
    max_retries: u32,
}

impl NativeProvider {
    pub fn new(
        name: impl Into<String>,
        codec: Arc<dyn WireCodec>,
        cred: Arc<dyn Credential>,
    ) -> Self {
        Self {
            name: name.into(),
            http: reqwest::Client::new(),
            codec,
            cred,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }

    /// Override the retry budget for transient connection failures (default [`DEFAULT_MAX_RETRIES`]).
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }
}

#[async_trait]
impl Provider for NativeProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn stream(&self, mut req: Request) -> Result<ChunkStream> {
        if let Some(prefix) = self.cred.system_prefix() {
            req.system = Some(match req.system.take() {
                Some(s) => format!("{prefix}\n\n{s}"),
                None => prefix,
            });
        }

        let body = self.codec.build_body(&req)?;
        let wire_headers = self.codec.wire_headers();
        let span =
            tracing::info_span!("provider.stream", provider = %self.name, model = %req.model);
        let _enter = span.enter();

        // Retry only the connection attempt (POST + status). The token is (re)applied each attempt
        // so an OAuth refresh can recover a 401/expired credential on the next try.
        let mut attempt = 0u32;
        // A 401 forces exactly one OAuth token refresh + retry; a second 401 surfaces the error.
        let mut forced_refresh = false;
        let resp = loop {
            let mut rb = self
                .http
                .post(self.cred.endpoint())
                .header("content-type", "application/json")
                .json(&body);
            for (k, v) in &wire_headers {
                rb = rb.header(*k, v.clone());
            }
            rb = self.cred.apply(rb).await?;

            match rb.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        break resp;
                    }
                    // Force-refresh on 401: the stored expiry can be wrong, so the lazy
                    // refresh-on-expiry path may have re-applied a dead token. If the credential is
                    // OAuth-backed, force one refresh (ignoring the expiry buffer) and retry once.
                    // The retry re-applies the credential, which now reads the freshened token.
                    if status.as_u16() == 401 && !forced_refresh {
                        if let Some(ts) = self.cred.token_source() {
                            tracing::warn!("401 unauthorized; forcing one OAuth refresh and retry");
                            ts.refresh().await?;
                            forced_refresh = true;
                            continue;
                        }
                    }
                    if is_retryable_status(status.as_u16()) && attempt < self.max_retries {
                        let delay = backoff_delay(attempt);
                        tracing::warn!(
                            status = status.as_u16(),
                            attempt,
                            delay_ms = delay.as_millis() as u64,
                            "retrying after retryable status"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    let message = resp.text().await.unwrap_or_default();
                    return Err(Error::Api {
                        status: status.as_u16(),
                        message,
                    });
                }
                Err(e) => {
                    if attempt < self.max_retries {
                        let delay = backoff_delay(attempt);
                        tracing::warn!(
                            error = %e,
                            attempt,
                            delay_ms = delay.as_millis() as u64,
                            "retrying after transport error"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(Error::Http(e.to_string()));
                }
            }
        };

        let bytes: ByteStream = Box::pin(
            resp.bytes_stream()
                .map(|r| r.map_err(|e| Error::Provider(format!("stream: {e}")))),
        );
        Ok(self.codec.map_stream(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn retryable_statuses() {
        for s in [429, 500, 502, 503, 504] {
            assert!(is_retryable_status(s), "{s} should be retryable");
        }
        for s in [200, 400, 401, 403, 404] {
            assert!(!is_retryable_status(s), "{s} should not be retryable");
        }
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_delay(0).as_millis(), 500);
        assert_eq!(backoff_delay(1).as_millis(), 1000);
        assert_eq!(backoff_delay(2).as_millis(), 2000);
        assert!(backoff_delay(20).as_millis() <= 30_000);
    }

    /// A codec that ignores the request and yields no chunks (we only test the connection path).
    struct NullCodec;
    impl WireCodec for NullCodec {
        fn build_body(&self, _req: &Request) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
        fn map_stream(&self, _bytes: ByteStream) -> ChunkStream {
            Box::pin(futures::stream::empty())
        }
    }

    /// A no-op credential pointing at a test endpoint.
    struct NullCred {
        endpoint: String,
    }
    #[async_trait]
    impl Credential for NullCred {
        fn endpoint(&self) -> String {
            self.endpoint.clone()
        }
        async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
            Ok(rb)
        }
    }

    /// A minimal HTTP/1.1 server that returns 503 for its first `fail_times` connections, then 200.
    /// Returns the base URL, the accept-loop handle, and a shared connection counter.
    async fn flaky_server(
        fail_times: usize,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await; // best-effort drain of the request
                let resp = if n < fail_times {
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}/"), handle, count)
    }

    #[tokio::test]
    async fn retries_then_succeeds_on_flaky_5xx() {
        let (url, handle, count) = flaky_server(2).await;
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(NullCred { endpoint: url }),
        )
        .with_max_retries(3);
        let res = provider.stream(Request::new("m", "hi")).await;
        assert!(res.is_ok(), "should recover after transient 503s");
        assert_eq!(count.load(Ordering::SeqCst), 3, "2 failures + 1 success");
        handle.abort();
    }

    #[tokio::test]
    async fn gives_up_after_max_retries() {
        let (url, handle, count) = flaky_server(100).await;
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(NullCred { endpoint: url }),
        )
        .with_max_retries(1);
        let status = match provider.stream(Request::new("m", "hi")).await {
            Err(Error::Api { status, .. }) => status,
            Ok(_) => panic!("expected an Api error, got a stream"),
            Err(e) => panic!("expected an Api error, got {e}"),
        };
        assert_eq!(status, 503);
        assert_eq!(count.load(Ordering::SeqCst), 2, "initial attempt + 1 retry");
        handle.abort();
    }

    // --- 401 force-refresh-then-retry (C-04) ------------------------------------------------

    /// A [`TokenSource`] that starts handing out a dead token and flips to a good one on the first
    /// `refresh()`. Counts refresh calls so a test can assert exactly one refresh fired.
    struct FlipToken {
        refreshed: AtomicBool,
        refresh_calls: AtomicUsize,
    }
    impl FlipToken {
        fn new() -> Self {
            Self {
                refreshed: AtomicBool::new(false),
                refresh_calls: AtomicUsize::new(0),
            }
        }
    }
    #[async_trait]
    impl TokenSource for FlipToken {
        async fn access_token(&self) -> Result<String> {
            Ok(if self.refreshed.load(Ordering::SeqCst) {
                "good-token".to_string()
            } else {
                "dead-token".to_string()
            })
        }
        async fn refresh(&self) -> Result<()> {
            self.refresh_calls.fetch_add(1, Ordering::SeqCst);
            self.refreshed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// An OAuth-backed credential: applies `Bearer <token>` from its [`TokenSource`] and exposes
    /// that source via [`Credential::token_source`] so the HTTP path can force-refresh on a 401.
    struct OAuthTestCred {
        endpoint: String,
        ts: Arc<dyn TokenSource>,
    }
    #[async_trait]
    impl Credential for OAuthTestCred {
        fn endpoint(&self) -> String {
            self.endpoint.clone()
        }
        async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
            let token = self.ts.access_token().await?;
            Ok(rb.header("authorization", format!("Bearer {token}")))
        }
        fn token_source(&self) -> Option<Arc<dyn TokenSource>> {
            Some(self.ts.clone())
        }
    }

    /// A server that returns 401 until a request arrives carrying `Authorization: Bearer good-token`,
    /// then 200. Records each request's `authorization` header so a test can assert the retry
    /// carried the refreshed token. Returns (base url, accept handle, connection counter, auth log).
    #[allow(clippy::type_complexity)]
    async fn auth_gated_server() -> (
        String,
        tokio::task::JoinHandle<()>,
        Arc<AtomicUsize>,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let auths = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let auth_log = auths.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let auth = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .map(|l| {
                        l.split_once(':')
                            .map(|(_, v)| v.trim())
                            .unwrap_or("")
                            .to_string()
                    })
                    .unwrap_or_default();
                auth_log.lock().unwrap().push(auth.clone());
                let resp = if auth == "Bearer good-token" {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                        .to_string()
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}/"), handle, count, auths)
    }

    #[tokio::test]
    async fn oauth_401_triggers_single_refresh_and_retry() {
        let (url, handle, count, auths) = auth_gated_server().await;
        let ts = Arc::new(FlipToken::new());
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(OAuthTestCred {
                endpoint: url,
                ts: ts.clone(),
            }),
        );
        let res = provider.stream(Request::new("m", "hi")).await;
        assert!(
            res.is_ok(),
            "the retry with the refreshed token should succeed"
        );
        assert_eq!(
            ts.refresh_calls.load(Ordering::SeqCst),
            1,
            "exactly one forced refresh"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "one 401 attempt + one retry"
        );
        let auths = auths.lock().unwrap();
        assert_eq!(
            *auths,
            vec![
                "Bearer dead-token".to_string(),
                "Bearer good-token".to_string()
            ],
            "first request used the dead token, the retry used the refreshed token"
        );
        handle.abort();
    }

    /// A server that returns 401 on **every** request (a refresh would not help — e.g. a revoked
    /// grant). Counts connections so a test can assert the retry happens at most once.
    async fn always_401_server() -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let resp =
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}/"), handle, count)
    }

    #[tokio::test]
    async fn oauth_second_401_surfaces_error_no_infinite_loop() {
        let (url, handle, count) = always_401_server().await;
        let ts = Arc::new(FlipToken::new());
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(OAuthTestCred {
                endpoint: url,
                ts: ts.clone(),
            }),
        );
        let status = match provider.stream(Request::new("m", "hi")).await {
            Err(Error::Api { status, .. }) => status,
            Ok(_) => panic!("expected an Api 401 error, got a stream"),
            Err(e) => panic!("expected an Api 401 error, got {e}"),
        };
        assert_eq!(status, 401);
        assert_eq!(
            ts.refresh_calls.load(Ordering::SeqCst),
            1,
            "refresh fires exactly once, even though the second 401 is unrecoverable"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "initial 401 + exactly one retry, then surface (no infinite loop)"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn oauth_500_does_not_force_refresh() {
        // A 5xx uses the existing backoff/retry and must NOT trigger a token refresh.
        let (url, handle, count) = flaky_server(1).await;
        let ts = Arc::new(FlipToken::new());
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(OAuthTestCred {
                endpoint: url,
                ts: ts.clone(),
            }),
        )
        .with_max_retries(3);
        let res = provider.stream(Request::new("m", "hi")).await;
        assert!(
            res.is_ok(),
            "should recover after the transient 5xx via backoff"
        );
        assert_eq!(
            ts.refresh_calls.load(Ordering::SeqCst),
            0,
            "a 5xx must not force a token refresh"
        );
        assert_eq!(count.load(Ordering::SeqCst), 2, "1 failure + 1 success");
        handle.abort();
    }
}

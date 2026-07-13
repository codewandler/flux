//! `flux-provider` — the provider abstraction.
//!
//! A [`Provider`] turns a [`Request`] into a stream of [`Chunk`](flux_core::Chunk)s. Concrete
//! clients (Anthropic, OpenAI, Ollama) live in their own crates and implement this trait. The
//! trait is object-safe (via `async_trait`) so the runtime can hold a `Box<dyn Provider>` and
//! swap providers/models at will.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

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

pub mod static_providers;
pub use static_providers::{NullProvider, StaticProvider};

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

/// One segment of a segmented system prompt. Segments render in order; `cache: true` marks a
/// prompt-cache breakpoint AFTER this segment (Anthropic `cache_control`), so callers can lay the
/// prompt out cache-first: byte-stable material (op catalog, grammar, identity) in cached segments,
/// per-turn material (session symbols) in a trailing uncached one. Codecs without segment support
/// join the texts in order (see [`Request::system_text`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemSegment {
    pub text: String,
    pub cache: bool,
}

/// A provider-agnostic inference request.
#[derive(Debug, Clone)]
pub struct Request {
    /// Concrete model id (already resolved from any alias).
    pub model: String,
    /// Optional system prompt.
    pub system: Option<String>,
    /// Segmented system prompt (takes precedence over `system` when non-empty). Lets the engine
    /// separate cache-stable prompt material from per-turn material — see [`SystemSegment`].
    pub system_segments: Vec<SystemSegment>,
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
            system_segments: Vec::new(),
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

    /// The full system prompt as one string: the joined segments when segmented, else `system`.
    /// The fallback for codecs (OpenAI Chat/Responses) whose wire has no cache-breakpoint notion —
    /// segment order is preserved so the prefix stays byte-stable for providers that prefix-cache
    /// implicitly.
    pub fn system_text(&self) -> Option<String> {
        if !self.system_segments.is_empty() {
            return Some(
                self.system_segments
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
        }
        self.system.clone()
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

/// Axis (c): an **alternative streaming transport** (e.g. the codex WebSocket) tried *before*
/// the generic HTTP+SSE path. `connect` performs its own handshake/auth (the [`Credential`] is
/// reqwest-bound), sends the codec-built body, and returns the response byte stream **in the
/// same envelope the codec's [`WireCodec::map_stream`] expects** (SSE `data:` lines for the
/// JSON-event codecs). Any `Err` — handshake failure, policy close before data, refused
/// connection — makes [`NativeProvider`] fall back transparently to HTTP; providers without a
/// transport keep the reqwest path untouched.
#[async_trait]
pub trait StreamTransport: Send + Sync {
    /// Open the transport, send `body`, and return the response byte stream. Must fail (rather
    /// than hang or return an empty stream) on any connect-time problem so the HTTP fallback
    /// can take over before the turn is committed to this transport.
    async fn connect(&self, body: &serde_json::Value) -> Result<ByteStream>;
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

/// Apply a server-provided `Retry-After` delay when it is usable, otherwise use exponential
/// backoff. A small bounded jitter prevents a fleet of callers from retrying in lockstep; the
/// delay remains capped so an untrusted header cannot stall a turn indefinitely.
fn retry_delay(response: Option<&reqwest::Response>, attempt: u32) -> std::time::Duration {
    let server_ms = response
        .and_then(|r| r.headers().get(reqwest::header::RETRY_AFTER))
        .and_then(|v| v.to_str().ok())
        .and_then(retry_after_ms);
    let base = server_ms.unwrap_or_else(|| backoff_delay(attempt).as_millis() as u64);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() as u64 % 251)
        .unwrap_or(0);
    std::time::Duration::from_millis(base.saturating_add(jitter).min(30_000))
}

fn retry_after_ms(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1000).min(30_000))
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
    transport: Option<Arc<dyn StreamTransport>>,
    /// Test-only observation seam for the C-19 fallback note (production writes stderr).
    #[cfg(test)]
    fallback_note_sink: Option<FallbackNoteSink>,
}

/// Test-only sink for the C-19 fallback note — see `NativeProvider::with_fallback_note_sink`.
#[cfg(test)]
type FallbackNoteSink = Arc<dyn Fn(&str) + Send + Sync>;

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
            transport: None,
            #[cfg(test)]
            fallback_note_sink: None,
        }
    }

    /// Override the retry budget for transient connection failures (default [`DEFAULT_MAX_RETRIES`]).
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Attach an alternative [`StreamTransport`] (axis c) tried before the HTTP path. A
    /// connect-time failure falls back transparently to HTTP — see [`StreamTransport`].
    pub fn with_transport(mut self, transport: Arc<dyn StreamTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Route the C-19 fallback note into `sink` instead of stderr so a test can assert on it.
    #[cfg(test)]
    fn with_fallback_note_sink(mut self, sink: FallbackNoteSink) -> Self {
        self.fallback_note_sink = Some(sink);
        self
    }

    /// Emit a C-19 transport-fallback note: stderr in production, the test sink when installed.
    fn emit_fallback_note(&self, note: &str) {
        #[cfg(test)]
        if let Some(sink) = &self.fallback_note_sink {
            sink(note);
            return;
        }
        eprintln!("{note}");
    }
}

/// C-19: format the env-gated marker for the transport→HTTP fallback — `Some` only when
/// `FLUX_TRANSPORT_DEBUG=1`. The fallback otherwise logs only via `tracing::warn!`, which is
/// invisible from the CLI (no subscriber installed), so a broken WS leg would silently complete
/// over HTTP with no observable signal. The prefix is stable — the live smoke gate
/// (`scripts/smoke-live.sh`) greps stderr for it to tell "over WS" apart from "via HTTP fallback".
fn transport_fallback_note(err: &Error) -> Option<String> {
    let on = std::env::var("FLUX_TRANSPORT_DEBUG").is_ok_and(|v| v == "1");
    on.then(|| format!("flux: stream transport fell back to HTTP: {err}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelTraceMode {
    Summary,
    Full,
}

fn model_trace_mode() -> Option<ModelTraceMode> {
    match std::env::var("FLUX_MODEL_TRACE")
        .ok()?
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "summary" | "true" | "on" => Some(ModelTraceMode::Summary),
        "full" => Some(ModelTraceMode::Full),
        _ => None,
    }
}

static MODEL_TRACE_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
type ModelTraceSink = Arc<dyn Fn(&serde_json::Value) + Send + Sync>;

#[cfg(test)]
thread_local! {
    static MODEL_TRACE_SINK: std::cell::RefCell<Option<ModelTraceSink>> = const { std::cell::RefCell::new(None) };
}

fn emit_model_trace(value: serde_json::Value) {
    #[cfg(test)]
    {
        let delivered = MODEL_TRACE_SINK.with(|slot| {
            if let Some(sink) = slot.borrow().as_ref() {
                sink(&value);
                true
            } else {
                false
            }
        });
        if delivered {
            return;
        }
    }
    eprintln!("flux: model_trace {value}");
}

struct ModelTrace {
    id: u64,
    provider: String,
    model: String,
    started: Instant,
    body_built_us: u64,
    response_us: u64,
    first_chunk_us: Option<u64>,
    first_thinking_us: Option<u64>,
    first_tool_us: Option<u64>,
    first_text_us: Option<u64>,
    usage_us: Option<u64>,
    done_us: Option<u64>,
    chunks: u64,
    usage: Option<flux_core::Usage>,
    http_attempts: u32,
    oauth_refreshes: u32,
    transport_fallback: bool,
    terminal: Option<&'static str>,
    emitted: bool,
}

impl ModelTrace {
    fn elapsed_us(&self) -> u64 {
        self.started.elapsed().as_micros().min(u64::MAX as u128) as u64
    }

    fn observe(&mut self, item: &Result<Chunk>) {
        let now = self.elapsed_us();
        self.first_chunk_us.get_or_insert(now);
        self.chunks += 1;
        match item {
            Ok(Chunk::ThinkingDelta(_)) => {
                self.first_thinking_us.get_or_insert(now);
            }
            Ok(Chunk::ToolInputDelta { .. })
            | Ok(Chunk::Block(flux_core::ContentBlock::ToolUse { .. })) => {
                self.first_tool_us.get_or_insert(now);
            }
            Ok(Chunk::TextDelta(_)) | Ok(Chunk::Block(flux_core::ContentBlock::Text { .. })) => {
                self.first_text_us.get_or_insert(now);
            }
            Ok(Chunk::Usage(usage)) => {
                self.usage_us = Some(now);
                self.usage = Some(usage.clone());
            }
            Ok(Chunk::Done { .. }) => {
                self.done_us = Some(now);
            }
            Err(_) => self.terminal = Some("stream_error"),
            _ => {}
        }
    }

    fn emit(&mut self, terminal: &'static str) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        let terminal = self.terminal.unwrap_or(terminal);
        emit_model_trace(serde_json::json!({
            "event": "stream.end",
            "id": self.id,
            "provider": self.provider,
            "model": self.model,
            "terminal": terminal,
            "body_built_us": self.body_built_us,
            "response_us": self.response_us,
            "first_chunk_us": self.first_chunk_us,
            "first_thinking_us": self.first_thinking_us,
            "first_tool_us": self.first_tool_us,
            "first_text_us": self.first_text_us,
            "usage_us": self.usage_us,
            "done_us": self.done_us,
            "total_us": self.elapsed_us(),
            "chunks": self.chunks,
            "usage": self.usage,
            "http_attempts": self.http_attempts,
            "oauth_refreshes": self.oauth_refreshes,
            "transport_fallback": self.transport_fallback,
        }));
    }
}

impl Drop for ModelTrace {
    fn drop(&mut self) {
        self.emit("request_error");
    }
}

struct ModelTraceStream {
    inner: ChunkStream,
    trace: ModelTrace,
}

impl Stream for ModelTraceStream {
    type Item = Result<Chunk>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                self.trace.observe(&item);
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                self.trace.emit("eof");
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for ModelTraceStream {
    fn drop(&mut self) {
        self.trace.emit("dropped");
    }
}

fn begin_model_trace(
    mode: ModelTraceMode,
    provider: &str,
    req: &Request,
    body: &serde_json::Value,
    started: Instant,
) -> ModelTrace {
    let id = MODEL_TRACE_ID.fetch_add(1, Ordering::Relaxed);
    let body_built_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    let segments: Vec<serde_json::Value> = req
        .system_segments
        .iter()
        .map(|segment| serde_json::json!({ "bytes": segment.text.len(), "cache": segment.cache }))
        .collect();
    emit_model_trace(serde_json::json!({
        "event": "request",
        "id": id,
        "provider": provider,
        "model": req.model,
        "thinking": req.thinking,
        "effort": req.effort.map(Effort::as_str),
        "max_tokens": req.max_tokens,
        "system_bytes": req.system_text().map(|s| s.len()).unwrap_or_default(),
        "system_segments": segments,
        "messages": req.messages.len(),
        "message_bytes": serde_json::to_vec(&req.messages).map(|v| v.len()).unwrap_or_default(),
        "tools": req.tools.len(),
        "tool_names": req.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        "body_bytes": serde_json::to_vec(body).map(|v| v.len()).unwrap_or_default(),
        "body_built_us": body_built_us,
    }));
    if mode == ModelTraceMode::Full {
        emit_model_trace(serde_json::json!({
            "event": "request.body",
            "id": id,
            "sensitive": true,
            "body": body,
        }));
    }
    ModelTrace {
        id,
        provider: provider.to_string(),
        model: req.model.clone(),
        started,
        body_built_us,
        response_us: 0,
        first_chunk_us: None,
        first_thinking_us: None,
        first_tool_us: None,
        first_text_us: None,
        usage_us: None,
        done_us: None,
        chunks: 0,
        usage: None,
        http_attempts: 0,
        oauth_refreshes: 0,
        transport_fallback: false,
        terminal: None,
        emitted: false,
    }
}

fn finish_model_trace_stream(stream: ChunkStream, trace: Option<ModelTrace>) -> ChunkStream {
    match trace {
        Some(trace) => Box::pin(ModelTraceStream {
            inner: stream,
            trace,
        }),
        None => stream,
    }
}

#[async_trait]
impl Provider for NativeProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn stream(&self, mut req: Request) -> Result<ChunkStream> {
        let trace_started = Instant::now();
        if let Some(prefix) = self.cred.system_prefix() {
            if !req.system_segments.is_empty() {
                // The transport-required prefix is constant per credential → its own cached
                // segment at the very front, keeping the segments after it prefix-stable.
                req.system_segments.insert(
                    0,
                    SystemSegment {
                        text: prefix,
                        cache: true,
                    },
                );
            } else {
                req.system = Some(match req.system.take() {
                    Some(s) => format!("{prefix}\n\n{s}"),
                    None => prefix,
                });
            }
        }

        let body = self.codec.build_body(&req)?;
        let mut model_trace = model_trace_mode()
            .map(|mode| begin_model_trace(mode, &self.name, &req, &body, trace_started));
        let wire_headers = self.codec.wire_headers();
        let span =
            tracing::info_span!("provider.stream", provider = %self.name, model = %req.model);
        let _enter = span.enter();

        // C-07: an alternative transport (e.g. the codex WebSocket) is tried first. Any
        // connect-time failure — handshake rejection, policy close before data, refused
        // connection — falls back transparently to the generic HTTP+SSE path below.
        if let Some(transport) = &self.transport {
            match transport.connect(&body).await {
                Ok(bytes) => {
                    if let Some(trace) = model_trace.as_mut() {
                        trace.response_us = trace.elapsed_us();
                    }
                    return Ok(finish_model_trace_stream(
                        self.codec.map_stream(bytes),
                        model_trace,
                    ));
                }
                Err(e) => {
                    if let Some(trace) = model_trace.as_mut() {
                        trace.transport_fallback = true;
                    }
                    tracing::warn!(error = %e, "stream transport failed; falling back to HTTP");
                    // C-19: the warning above is invisible from the CLI (no tracing subscriber
                    // is installed), so a broken WS leg would silently complete over HTTP. With
                    // FLUX_TRANSPORT_DEBUG=1 the fallback also emits a stable stderr marker the
                    // live smoke gate greps to tell "over WS" apart from "via HTTP fallback".
                    if let Some(note) = transport_fallback_note(&e) {
                        self.emit_fallback_note(&note);
                    }
                }
            }
        }

        // Retry only the connection attempt (POST + status). The token is (re)applied each attempt
        // so an OAuth refresh can recover a 401/expired credential on the next try.
        let mut attempt = 0u32;
        // A 401 forces exactly one OAuth token refresh + retry; a second 401 surfaces the error.
        let mut forced_refresh = false;
        let resp = loop {
            if let Some(trace) = model_trace.as_mut() {
                trace.http_attempts += 1;
            }
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
                            if let Some(trace) = model_trace.as_mut() {
                                trace.oauth_refreshes += 1;
                            }
                            ts.refresh().await?;
                            forced_refresh = true;
                            continue;
                        }
                    }
                    if is_retryable_status(status.as_u16()) && attempt < self.max_retries {
                        let delay = retry_delay(Some(&resp), attempt);
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
                        let delay = retry_delay(None, attempt);
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
        if let Some(trace) = model_trace.as_mut() {
            trace.response_us = trace.elapsed_us();
        }
        Ok(finish_model_trace_stream(
            self.codec.map_stream(bytes),
            model_trace,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn model_trace_records_request_shape_and_stream_milestones() {
        let records = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = records.clone();
        MODEL_TRACE_SINK.with(|slot| {
            *slot.borrow_mut() = Some(Arc::new(move |value| {
                captured.lock().unwrap().push(value.clone());
            }));
        });

        let req = Request::new("model", "hello")
            .with_thinking(true)
            .with_effort(Effort::High);
        let body = serde_json::json!({"messages": [{"role": "user", "content": "hello"}]});
        let started = Instant::now();
        let mut trace = begin_model_trace(ModelTraceMode::Summary, "test", &req, &body, started);
        trace.response_us = trace.elapsed_us();
        let inner: ChunkStream = Box::pin(futures::stream::iter(vec![
            Ok(Chunk::ThinkingDelta("considering".into())),
            Ok(Chunk::ToolInputDelta {
                name: "read".into(),
                partial_json: "{}".into(),
            }),
            Ok(Chunk::Usage(flux_core::Usage {
                input_tokens: 10,
                output_tokens: 2,
                reasoning_tokens: 1,
                ..Default::default()
            })),
            Ok(Chunk::Done { stop_reason: None }),
        ]));
        let mut stream = finish_model_trace_stream(inner, Some(trace));
        while stream.next().await.is_some() {}
        drop(stream);
        MODEL_TRACE_SINK.with(|slot| *slot.borrow_mut() = None);

        let records = records.lock().unwrap();
        assert_eq!(records.len(), 2, "request + terminal stream record");
        assert_eq!(records[0]["event"], "request");
        assert_eq!(records[0]["effort"], "high");
        assert_eq!(records[0]["thinking"], true);
        assert_eq!(records[1]["event"], "stream.end");
        assert_eq!(records[1]["terminal"], "eof");
        assert!(records[1]["first_thinking_us"].is_number());
        assert!(records[1]["first_tool_us"].is_number());
        assert_eq!(records[1]["usage"]["reasoning_tokens"], 1);
    }

    #[test]
    fn full_model_trace_marks_the_exact_body_sensitive() {
        let records = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = records.clone();
        MODEL_TRACE_SINK.with(|slot| {
            *slot.borrow_mut() = Some(Arc::new(move |value| {
                captured.lock().unwrap().push(value.clone());
            }));
        });
        let req = Request::new("model", "private prompt");
        let body = serde_json::json!({"input": "private prompt"});
        let mut trace =
            begin_model_trace(ModelTraceMode::Full, "test", &req, &body, Instant::now());
        trace.emit("test_complete");
        MODEL_TRACE_SINK.with(|slot| *slot.borrow_mut() = None);
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[1]["event"], "request.body");
        assert_eq!(records[1]["sensitive"], true);
        assert_eq!(records[1]["body"], body);
    }

    #[test]
    fn dropping_a_trace_before_a_stream_exists_records_request_error() {
        let records = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = records.clone();
        MODEL_TRACE_SINK.with(|slot| {
            *slot.borrow_mut() = Some(Arc::new(move |value| {
                captured.lock().unwrap().push(value.clone());
            }));
        });
        let req = Request::new("model", "hello");
        let trace = begin_model_trace(
            ModelTraceMode::Summary,
            "test",
            &req,
            &serde_json::json!({"input": "hello"}),
            Instant::now(),
        );
        drop(trace);
        MODEL_TRACE_SINK.with(|slot| *slot.borrow_mut() = None);

        let records = records.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["event"], "stream.end");
        assert_eq!(records[1]["terminal"], "request_error");
    }

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

    #[test]
    fn retry_after_is_bounded_and_invalid_values_fall_back() {
        assert_eq!(retry_after_ms("2"), Some(2_000));
        assert_eq!(retry_after_ms("999999"), Some(30_000));
        assert_eq!(retry_after_ms("tomorrow"), None);
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

    // --- alternative stream transport (C-07 seam) --------------------------------------------

    /// A fake transport that counts connects and either yields an empty stream or fails.
    struct FakeTransport {
        connects: Arc<AtomicUsize>,
        fail: bool,
    }
    #[async_trait]
    impl StreamTransport for FakeTransport {
        async fn connect(&self, _body: &serde_json::Value) -> Result<ByteStream> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(Error::Http("ws handshake refused".to_string()));
            }
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn transport_is_tried_before_http() {
        let (url, handle, http_hits) = flaky_server(0).await;
        let connects = Arc::new(AtomicUsize::new(0));
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(NullCred { endpoint: url }),
        )
        .with_transport(Arc::new(FakeTransport {
            connects: connects.clone(),
            fail: false,
        }));
        let res = provider.stream(Request::new("m", "hi")).await;
        assert!(res.is_ok());
        assert_eq!(connects.load(Ordering::SeqCst), 1, "transport dialed first");
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            0,
            "HTTP path must stay cold when the transport succeeds"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn transport_failure_falls_back_to_http() {
        let (url, handle, http_hits) = flaky_server(0).await;
        let connects = Arc::new(AtomicUsize::new(0));
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(NullCred { endpoint: url }),
        )
        .with_transport(Arc::new(FakeTransport {
            connects: connects.clone(),
            fail: true,
        }));
        let res = provider.stream(Request::new("m", "hi")).await;
        assert!(res.is_ok(), "the turn must complete over the HTTP fallback");
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            1,
            "a failing transport must fall back to exactly one HTTP attempt"
        );
        handle.abort();
    }

    /// Run one turn through a failing transport (→ HTTP fallback) with the fallback note routed
    /// into `notes` instead of stderr. Helper for the C-19 marker test below.
    async fn stream_with_note_sink(
        url: String,
        notes: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Result<ChunkStream> {
        let provider = NativeProvider::new(
            "test",
            Arc::new(NullCodec),
            Arc::new(NullCred { endpoint: url }),
        )
        .with_transport(Arc::new(FakeTransport {
            connects: Arc::new(AtomicUsize::new(0)),
            fail: true,
        }))
        .with_fallback_note_sink(Arc::new(move |n: &str| {
            notes.lock().unwrap().push(n.to_string())
        }));
        provider.stream(Request::new("m", "hi")).await
    }

    /// C-19: with `FLUX_TRANSPORT_DEBUG=1` the transport→HTTP fallback emits a stable stderr
    /// marker (observed here via the test sink); with the variable unset the fallback stays
    /// silent. Both states live in ONE test so the env-var flip cannot race a sibling test.
    #[tokio::test]
    async fn fallback_note_is_emitted_only_when_env_gated() {
        let notes: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

        // Gated ON → exactly one note, with the stable grep-able prefix.
        std::env::set_var("FLUX_TRANSPORT_DEBUG", "1");
        let (url, handle, _hits) = flaky_server(0).await;
        let res = stream_with_note_sink(url, notes.clone()).await;
        std::env::remove_var("FLUX_TRANSPORT_DEBUG");
        assert!(res.is_ok(), "the turn still completes over HTTP");
        handle.abort();
        {
            let got = notes.lock().unwrap();
            assert_eq!(got.len(), 1, "one note per fallback when gated on");
            assert!(
                got[0].starts_with("flux: stream transport fell back to HTTP:"),
                "stable marker prefix (smoke-live.sh greps it), got: {}",
                got[0]
            );
        }

        // Gated OFF (unset) → the fallback is silent.
        notes.lock().unwrap().clear();
        let (url, handle, _hits) = flaky_server(0).await;
        let res = stream_with_note_sink(url, notes.clone()).await;
        assert!(res.is_ok());
        handle.abort();
        assert!(
            notes.lock().unwrap().is_empty(),
            "no note when FLUX_TRANSPORT_DEBUG is unset"
        );
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

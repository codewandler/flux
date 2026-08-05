//! The `codex` provider — ChatGPT/Codex subscription over the OpenAI Responses wire on the
//! ChatGPT backend.
//!
//! Codex is its own provider (own credential path via `flux-credentials`, own wire quirks —
//! `store:false`, forced reasoning summary, `include:["reasoning.encrypted_content"]`, no
//! `max_output_tokens`), so it owns its public surface here alongside the other providers
//! (`anthropic`, `openrouter`, `ollama`, …). It *shares* the Responses codec and body builder
//! with the API-key `openai` path — those live in [`crate::openai`] (`OpenAiResponses`,
//! `build_responses_body`) — because the two providers speak the same wire protocol; the
//! `codex: bool` flag on the codec toggles the ChatGPT-backend quirks.
//!
//! This is also the single owner of **codex model resolution**: the ChatGPT-subscription backend
//! serves the current GPT-5 family and rejects the legacy `*-codex`-suffixed ids (`gpt-5-codex`, …)
//! with HTTP 400 ("not supported when using Codex with a ChatGPT account"). [`resolve_model`]
//! encodes that knowledge once so every surface — CLI, SDK, server, TUI, the sub-agent spawner —
//! reaches it as `flux_providers::codex::resolve_model` instead of each carrying its own table.

use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use flux_core::{Error, Result};
use flux_provider::{ByteStream, NativeProvider, StreamTransport, TokenSource};

use crate::openai::{
    OpenAiCred, OpenAiResponses, Secret, TurnStateSlot, CODEX_ENDPOINT, CODEX_TURN_STATE_HEADER,
};

/// The gating/attribution headers the ChatGPT backend requires on **every** codex request —
/// applied both on the HTTP path ([`OpenAiCred::apply`]) and on the WS handshake.
fn codex_headers() -> Vec<(&'static str, String)> {
    vec![
        ("OpenAI-Beta", "responses=experimental".to_string()),
        ("originator", "codex_cli_rs".to_string()),
    ]
}

/// Derive the websocket URL for a Responses endpoint (`https://…` → `wss://…`; plain `http`
/// maps to `ws` so hermetic tests can point at a local stub).
fn derive_ws_url(endpoint: &str) -> String {
    if let Some(rest) = endpoint.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        endpoint.to_string()
    }
}

/// The default model the ChatGPT-subscription Codex backend serves. Used when a caller specifies
/// the `codex` provider with no model (bare `codex`) or with a legacy `*-codex` id. OpenAI's
/// model catalog lists `gpt-5.6` as the alias for this concrete `gpt-5.6-sol` model.
pub const DEFAULT_MODEL: &str = "gpt-5.6-sol";

/// Resolve a codex model id to what the live ChatGPT-subscription backend accepts.
///
/// - An empty model (the bare `codex` shorthand) → [`DEFAULT_MODEL`] (`gpt-5.6-sol`).
/// - `gpt-5.6`, listed by OpenAI as the alias for GPT-5.6 Sol, → [`DEFAULT_MODEL`].
/// - A legacy `*-codex`-suffixed id (`gpt-5-codex`, `o3-codex`, …) → [`DEFAULT_MODEL`]; the
///   backend rejects these with HTTP 400.
/// - Any other id is passed through verbatim, so an explicit current concrete id (`gpt-5.6-sol`,
///   `gpt-5.5`, `gpt-5`, …) is sent as-is and a future model is honoured without a flux release.
pub fn resolve_model(model: &str) -> String {
    if model.is_empty() || model == "gpt-5.6" || model.ends_with("-codex") {
        DEFAULT_MODEL.to_string()
    } else {
        model.to_string()
    }
}

/// Build the `codex` provider: ChatGPT/Codex subscription via OAuth, OpenAI Responses wire on the
/// ChatGPT backend. Needs a [`TokenSource`] (from `flux-credentials`).
///
/// The credential carries the `chatgpt-account-id` header (resolved from `~/.codex/auth.json`);
/// `OpenAiCred::apply` surfaces a typed `Error::Auth` if no account id is resolvable rather than
/// letting the backend return an opaque 401.
pub fn oauth(tokens: Arc<dyn TokenSource>) -> NativeProvider {
    oauth_at(tokens, CODEX_ENDPOINT, &derive_ws_url(CODEX_ENDPOINT))
}

/// [`oauth`] with explicit HTTP + WS endpoints, so hermetic tests can point both transports at
/// local stub servers. Production goes through [`oauth`] (live ChatGPT backend, `wss://` derived
/// from [`CODEX_ENDPOINT`]).
/// WS by default since C-159 landed the session-scoped transport; `FLUX_CODEX_WS=off` is the
/// escape hatch back to plain HTTP+SSE.
///
/// **History.** C-07 made WS the default; an interim C-159 change reversed it because flux used
/// the WS wrong — a fresh socket per request, no turn-state replay, the whole `input` resent, so
/// every request reached an arbitrary node with a full cold prompt (measured **WS ~3%** cache hit
/// vs **HTTP ~50%**). C-159 then adopted the upstream client's session-scoped design
/// (`codex-rs/core/src/client.rs`): one cached connection reused across rounds, the
/// `x-codex-turn-state` sticky-routing token replayed on both transports, and reuse sending
/// `previous_response_id` plus only the unseen items. Re-measured (2026-07-28,
/// `codex/gpt-5.6-sol`, 2-step turns, three pairs, both arm orders): **WS 37/37/37%** — the
/// connection makes the hit *deterministic* — against HTTP's shard-luck **0/19/56%**, with cost
/// tracking it. Table in `docs/designs/llm-cache-review.md`.
fn oauth_at(tokens: Arc<dyn TokenSource>, endpoint: &str, ws_url: &str) -> NativeProvider {
    if ws_enabled() {
        return oauth_at_timeout(tokens, endpoint, ws_url, DEFAULT_FIRST_FRAME_TIMEOUT);
    }
    http_native(tokens, endpoint)
}

/// Whether the WS transport is active: the default since C-159; `FLUX_CODEX_WS=off` disables it.
fn ws_enabled() -> bool {
    ws_enabled_value(std::env::var("FLUX_CODEX_WS").ok().as_deref())
}

/// The truthiness rule behind [`ws_enabled`], split out so it can be tested without mutating process
/// env under a parallel test run. Default ON; only an explicit negative turns it off.
fn ws_enabled_value(value: Option<&str>) -> bool {
    !value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

/// [`oauth_at`] with an explicit first-frame connect timeout (C-28), so tests can keep it short
/// instead of waiting out [`DEFAULT_FIRST_FRAME_TIMEOUT`].
fn oauth_at_timeout(
    tokens: Arc<dyn TokenSource>,
    endpoint: &str,
    ws_url: &str,
    first_frame_timeout: Duration,
) -> NativeProvider {
    // One turn-state slot for the whole session, shared by the WS transport and the HTTP
    // credential behind it (C-159): affinity established on either leg steers the other.
    let turn_state: TurnStateSlot = Arc::default();
    http_native_with_turn_state(tokens.clone(), endpoint, Some(turn_state.clone())).with_transport(
        Arc::new(CodexWsTransport {
            url: ws_url.to_string(),
            tokens,
            first_frame_timeout,
            turn_state,
            session: Arc::default(),
        }),
    )
}

/// The codex provider on the plain HTTP+SSE path (no WS transport) — the fallback half of
/// [`oauth_at`], and the reference side of the WS/SSE equivalence test.
fn http_native(tokens: Arc<dyn TokenSource>, endpoint: &str) -> NativeProvider {
    // Pure-HTTP mode still wants the sticky-routing echo (C-159): the token is issued and
    // consumed over plain response/request headers, no WS required.
    http_native_with_turn_state(tokens, endpoint, Some(Arc::default()))
}

/// [`http_native`] with an explicit turn-state slot, so the WS mode can share one slot between
/// the transport and its HTTP fallback credential.
fn http_native_with_turn_state(
    tokens: Arc<dyn TokenSource>,
    endpoint: &str,
    turn_state: Option<TurnStateSlot>,
) -> NativeProvider {
    NativeProvider::new(
        "codex",
        Arc::new(OpenAiResponses { codex: true }),
        Arc::new(OpenAiCred {
            endpoint: endpoint.to_string(),
            secret: Secret::OAuth(tokens),
            extra: codex_headers(),
            send_account_id: true,
            turn_state,
            terminal_error: codex_terminal_error,
        }),
    )
}

/// The ChatGPT backend uses a typed `usageLimitExceeded` error for exhausted subscription windows.
/// Accept its snake-case equivalent and explicit reset-bearing usage/quota payloads, but never a
/// bare 429 (or Retry-After alone), which is an ordinary transient throttle.
fn codex_terminal_error(status: u16, body: &str) -> bool {
    if status != 429 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    let compact: String = lower
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if compact.contains("usagelimitexceeded")
        || compact.contains("usagelimitreached")
        || lower.contains("usage limit reached")
        || lower.contains("usage limit exceeded")
        || lower.contains("purchase more credits")
    {
        return true;
    }

    let reset_bearing = ["\"reset_at\"", "\"resets_at\"", "\"reset_time\""]
        .iter()
        .any(|marker| lower.contains(marker));
    reset_bearing && (lower.contains("usage") || lower.contains("quota"))
}

// ---------------------------------------------------------------------------
// WebSocket transport (C-07)
// ---------------------------------------------------------------------------

/// The codex WebSocket transport — **session-scoped since C-159** (see [`oauth_at`] for the
/// default's round trip: C-07 primary → interim HTTP → back to WS once this design landed),
/// mirroring the upstream codex Rust client. It opens `wss://…/codex/responses` with the
/// auth/gating headers on the tungstenite handshake (the reqwest-bound [`OpenAiCred`] cannot serve
/// a WS — same precedent as the realtime provider), sends the Responses body inline in a
/// `response.create` event frame (the live contract), and yields the response-event frames
/// re-enveloped as SSE bytes so the **existing** Responses codec (`map_responses_stream`) parses
/// them — guaranteeing WS and HTTP produce identical chunks. The WS-only `codex.rate_limits`
/// preamble is skipped pre-commit.
///
/// The session scope is what makes the prompt cache real (C-159): a clean `response.completed`
/// puts the connection back in [`CodexWsTransport::session`] with the conversation it has seen,
/// so the next call reuses the socket — landing on the node that holds the cache — and sends
/// `previous_response_id` plus only the unseen items when the conversation extends the last one.
///
/// Upstream WS is experimental/unstable (1008 policy closes, proxy trouble), so every
/// connect-time failure surfaces as `Err` and [`NativeProvider`] falls back transparently to
/// HTTP-SSE; that fallback is non-negotiable.
struct CodexWsTransport {
    /// The `wss://` (or `ws://` in hermetic tests) Responses URL.
    url: String,
    /// Bearer + `chatgpt-account-id` for the handshake. The WS path never refreshes on auth
    /// failure itself — a rejected handshake falls back to HTTP, which owns the 401→refresh path.
    tokens: Arc<dyn TokenSource>,
    /// Bound on the first-frame wait in [`CodexWsTransport::connect`] (C-28): a proxy that
    /// accepts the upgrade and then blackholes the socket must not pend the turn forever —
    /// exceeding this returns `Err` so the HTTP fallback engages.
    first_frame_timeout: Duration,
    /// The sticky-routing echo (C-159), shared with the session's HTTP credential: replayed as an
    /// upgrade-request header, refreshed from each upgrade response.
    turn_state: TurnStateSlot,
    /// The cached live connection (C-159). `None` while no connection is cached or one is in
    /// flight — `connect` TAKES the session, and the response stream puts it back only after a
    /// clean `response.completed`, so concurrent calls each open their own connection instead of
    /// interleaving on one socket. `Arc` because the restoring side is the response stream, which
    /// outlives the `connect` call. Guards session *state*, never held across a stream.
    session: SessionSlot,
}

type SessionSlot = Arc<tokio::sync::Mutex<Option<WsSession>>>;

/// A cached codex WS connection plus the per-connection state that makes reuse worth having
/// (C-159). Everything here dies with the socket: with `store: false` the server retains
/// conversation state only for the connection's lifetime, so `previous_response_id` from one
/// socket means nothing on the next.
struct WsSession {
    ws: WsStream,
    /// The request properties the connection was opened for — the wire body minus `input` (the
    /// upstream reuse predicate: model / instructions / tools / store / include /
    /// `prompt_cache_key` / … must all match; only the conversation may differ).
    props: Value,
    /// The full cumulative `input` array as of the last completed response, so the next call can
    /// send only its suffix when this is a strict prefix of the new conversation.
    sent_input: Vec<Value>,
    /// The id of the last completed response on this connection — the `previous_response_id` an
    /// incremental follow-up names.
    last_response_id: Option<String>,
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The upstream reuse predicate's key: the wire body with the per-call `input` removed. Two
/// requests whose keys are equal may share a connection; anything else — a model switch, a tool-set
/// change, a different cache key — needs a fresh one.
fn reuse_props(body: &Value) -> Value {
    let mut props = body.clone();
    if let Some(obj) = props.as_object_mut() {
        obj.remove("input");
    }
    props
}

/// `Some(delta)` when `sent` is a strict prefix of `input` — the items the server has not seen.
/// `None` when the conversation was rewritten (compaction, fork, edit): incremental send would
/// desync, so the caller falls back to a full resend.
fn incremental_delta<'a>(sent: &[Value], input: &'a [Value]) -> Option<&'a [Value]> {
    if sent.len() < input.len() && input[..sent.len()] == *sent {
        Some(&input[sent.len()..])
    } else {
        None
    }
}

/// Whether the terminal frame is a clean `response.completed` — the only outcome that earns the
/// connection a place back in the session cache (`response.failed` is terminal too, and a socket
/// that just failed a response is not one to reuse).
// A-37: tolerant parse — a miss means "not completed", never a stream error; the frame itself is
// re-enveloped and re-parsed by the codec regardless.
#[allow(clippy::disallowed_methods)]
fn is_completed_event(payload: &str) -> bool {
    serde_json::from_str::<Value>(payload)
        .ok()
        .is_some_and(|v| v["type"] == "response.completed")
}

/// The id a clean `response.completed` frame carries, for the next request's
/// `previous_response_id`.
// A-37: tolerant parse — a miss means "no id", never a stream error.
#[allow(clippy::disallowed_methods)]
fn completed_response_id(payload: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(payload).ok()?;
    (v["type"] == "response.completed").then(|| v["response"]["id"].as_str().map(str::to_string))?
}

/// Production default for [`CodexWsTransport::first_frame_timeout`] — generous enough to cover a
/// slow-starting reasoning turn's time-to-first-token, but bounded so a wedged proxy can't hang
/// the whole turn indefinitely. Tests override it via [`oauth_at_timeout`] to stay fast.
const DEFAULT_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether a WS event payload semantically ends the response. The live backend resets the socket
/// after the terminal event instead of performing a close handshake (observed 2026-07-02), so the
/// transport stops reading here — a reset *before* the terminal event still surfaces as a stream
/// error (real truncation).
// A-37: the `serde_json::from_str` call here already tolerates failure (`.ok()` /
// `.unwrap_or(false)`) — a parse miss just means "not terminal yet", never a fatal stream error;
// this WS frame is re-enveloped as SSE and re-parsed by `map_responses_stream` regardless. Allowed
// at this tight scope.
#[allow(clippy::disallowed_methods)]
fn is_terminal_event(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v["type"]
                .as_str()
                .map(|t| matches!(t, "response.completed" | "response.failed"))
        })
        .unwrap_or(false)
}

/// The largest char-boundary prefix of `s` no longer than `max` bytes. A raw byte-range slice
/// (`&s[..max]`) panics when `max` lands mid-codepoint, which a >300-byte untrusted WS payload
/// with a multibyte char straddling the offset can trigger (C-28) — the same char-boundary-safe
/// truncation idiom used elsewhere in the codebase for untrusted bytes (e.g.
/// `flux_core::context::truncate_str`, `flux_plugin`'s `truncate_on_char_boundary`), reproduced
/// here rather than shared publicly since neither crate is a dependency of `flux-providers`.
fn truncate_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Re-envelope one WS frame payload as an SSE `data:` event so the SSE-based codec parses it.
/// Multi-line payloads become multiple `data:` lines, which eventsource joins back with `\n` —
/// an exact round-trip.
fn sse_frame(payload: &str) -> Bytes {
    let mut out = String::with_capacity(payload.len() + 16);
    for line in payload.lines() {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    Bytes::from(out)
}

impl CodexWsTransport {
    /// Open a fresh socket: auth/gating headers plus the sticky-routing token on the upgrade
    /// request (C-159), capturing any refreshed token from the upgrade response.
    async fn open_socket(&self) -> Result<WsStream> {
        let mut request = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|e| Error::Http(format!("ws request: {e}")))?;
        {
            let token = self.tokens.access_token().await?;
            let account = self.tokens.account_id().ok_or_else(|| {
                Error::Auth(
                    "codex: no ChatGPT account id — re-login to the Codex CLI so flux can read \
                     it from `~/.codex/auth.json` (top-level `tokens.account_id` or the \
                     `id_token` claims)"
                        .to_string(),
                )
            })?;
            let headers = request.headers_mut();
            headers.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|e| Error::Auth(e.to_string()))?,
            );
            headers.insert(
                "chatgpt-account-id",
                HeaderValue::from_str(&account).map_err(|e| Error::Auth(e.to_string()))?,
            );
            for (k, v) in codex_headers() {
                headers.insert(
                    k,
                    HeaderValue::from_str(&v).map_err(|e| Error::Auth(e.to_string()))?,
                );
            }
            // C-159: replay the latest turn-state token so this connection lands on the node
            // that already holds the turn's cache.
            let stored = self.turn_state.lock().expect("turn-state lock").clone();
            if let Some(token) = stored {
                if let Ok(v) = HeaderValue::from_str(&token) {
                    headers.insert(CODEX_TURN_STATE_HEADER, v);
                }
            }
        }

        let (ws, resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| Error::Http(format!("ws connect: {e}")))?;
        // C-159: the upgrade response can carry a fresh turn-state token — capture it for both
        // this transport's next upgrade and the HTTP fallback (shared slot).
        if let Some(token) = resp
            .headers()
            .get(CODEX_TURN_STATE_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            *self.turn_state.lock().expect("turn-state lock") = Some(token.to_string());
        }
        Ok(ws)
    }

    /// Resume the cached connection for a body whose [`reuse_props`] matched: send the follow-up
    /// — incremental (`previous_response_id` + only the unseen items) when the cached conversation
    /// is a strict prefix of the new one, a full resend on the warm socket otherwise — and commit
    /// once the first substantive frame arrives. Any failure here (dead socket, error frame,
    /// timeout) returns `Err` so [`connect`](StreamTransport::connect) can open a fresh
    /// connection; a stale cache entry must never surface as an HTTP fallback.
    async fn resume(
        &self,
        mut sess: WsSession,
        body: &Value,
        input: &[Value],
    ) -> Result<ByteStream> {
        let wire_body = match (
            incremental_delta(&sess.sent_input, input),
            &sess.last_response_id,
        ) {
            (Some(delta), Some(prev)) => {
                let mut b = body.clone();
                b["input"] = serde_json::Value::Array(delta.to_vec());
                b["previous_response_id"] = serde_json::Value::String(prev.clone());
                b
            }
            _ => body.clone(),
        };
        send_create(&mut sess.ws, &wire_body).await?;
        let first = first_frame(&mut sess.ws, self.first_frame_timeout).await?;
        Ok(self.committed_stream(sess.ws, first, sess.props, input.to_vec()))
    }

    /// The committed response stream. From here on the turn is committed to WS: mid-stream
    /// failures surface as stream errors (matching the HTTP path, which also never retries
    /// mid-stream), and a `Close` before the terminal event is real truncation (C-28). On a clean
    /// `response.completed` the socket goes BACK into the session slot with the conversation it
    /// has seen and the response id (C-159), so the next call can reuse the connection and send
    /// only its delta; a failed/truncated stream drops the socket instead.
    fn committed_stream(
        &self,
        mut ws: WsStream,
        first: String,
        props: Value,
        full_input: Vec<Value>,
    ) -> ByteStream {
        let slot = self.session.clone();
        let stream = try_stream! {
            let mut terminal: Option<String> = None;
            if is_terminal_event(&first) {
                terminal = Some(first);
            } else {
                yield sse_frame(&first);
            }
            while terminal.is_none() {
                let Some(msg) = ws.next().await else { break };
                match msg {
                    Ok(Message::Text(t)) => {
                        if is_terminal_event(&t) {
                            terminal = Some(t.to_string());
                        } else {
                            yield sse_frame(&t);
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let detail = frame
                            .map(|f| format!("{} {}", f.code, f.reason))
                            .unwrap_or_else(|| "no close frame".to_string());
                        Err(Error::Provider(format!(
                            "ws closed before terminal event: {detail}"
                        )))?
                    }
                    Ok(_) => continue,
                    Err(e) => Err(Error::Provider(format!("ws stream: {e}")))?,
                }
            }
            if let Some(t) = terminal {
                // Cache the connection BEFORE yielding the terminal frame: a consumer that stops
                // polling after the last chunk must not cost the session its socket. Only a clean
                // completion restores — `response.failed` (or truncation above) drops the socket.
                if is_completed_event(&t) {
                    let sess = WsSession {
                        ws,
                        props,
                        sent_input: full_input,
                        last_response_id: completed_response_id(&t),
                    };
                    let mut guard = slot.lock().await;
                    // A concurrent call may have restored its own connection first; keep that
                    // one (last would be dropped either way — one cached connection per session).
                    if guard.is_none() {
                        *guard = Some(sess);
                    }
                }
                yield sse_frame(&t);
            }
        };
        Box::pin(stream)
    }
}

/// Send the request as a `response.create` event frame — the live contract (verified 2026-07-02):
/// the first websocket event must be a `response.create` message with the Responses body fields
/// inline; a bare body is rejected, and nesting under a `response` key loses the model.
async fn send_create(ws: &mut WsStream, body: &Value) -> Result<()> {
    let mut create = body.clone();
    if let Some(obj) = create.as_object_mut() {
        obj.insert(
            "type".to_string(),
            serde_json::Value::String("response.create".to_string()),
        );
    }
    ws.send(Message::Text(create.to_string().into()))
        .await
        .map_err(|e| Error::Http(format!("ws send: {e}")))
}

/// Wait for the first substantive frame before committing to the WS path. Policy failures arrive
/// AFTER a successful upgrade — as a 1008 close OR as an `error` EVENT before any response event
/// (both observed live) — and the `codex.rate_limits` preamble arrives before the request is
/// validated and must not commit. Bounded by `timeout` (C-28): a proxy that accepts the upgrade
/// and then blackholes the socket must not pend the turn forever.
// A-37: the kind-sniff here is tolerant (`.ok()` / `.unwrap_or_default()`) — a parse miss just
// falls through to "not a recognized control frame" and keeps waiting. Allowed at this scope.
#[allow(clippy::disallowed_methods)]
async fn first_frame(ws: &mut WsStream, timeout: Duration) -> Result<String> {
    let wait = async {
        Ok::<String, Error>(loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let kind = serde_json::from_str::<serde_json::Value>(&t)
                        .ok()
                        .and_then(|v| v["type"].as_str().map(str::to_string))
                        .unwrap_or_default();
                    match kind.as_str() {
                        "codex.rate_limits" => continue,
                        "error" => {
                            return Err(Error::Http(format!(
                                "ws error before data: {}",
                                truncate_char_boundary(&t, 300)
                            )));
                        }
                        // tungstenite 0.29: `Message::Text` carries `Utf8Bytes`, not `String`.
                        _ => break t.to_string(),
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    let detail = frame
                        .map(|f| format!("{} {}", f.code, f.reason))
                        .unwrap_or_else(|| "no close frame".to_string());
                    return Err(Error::Http(format!("ws closed before data: {detail}")));
                }
                Some(Ok(_)) => continue, // ping/pong/binary — keep waiting for data
                Some(Err(e)) => return Err(Error::Http(format!("ws: {e}"))),
                None => return Err(Error::Http("ws closed before data".to_string())),
            }
        })
    };
    tokio::time::timeout(timeout, wait)
        .await
        .map_err(|_| Error::Http("ws: timed out waiting for the first frame".to_string()))?
}

#[async_trait]
impl StreamTransport for CodexWsTransport {
    async fn connect(&self, body: &Value) -> Result<ByteStream> {
        let props = reuse_props(body);
        let input: Vec<Value> = body["input"].as_array().cloned().unwrap_or_default();

        // C-159: try the cached connection first. TAKE it — while a call is in flight the slot is
        // empty, so a concurrent call opens its own connection instead of sharing the socket.
        let cached = self.session.lock().await.take();
        if let Some(sess) = cached {
            if sess.props == props {
                match self.resume(sess, body, &input).await {
                    Ok(stream) => return Ok(stream),
                    // The live backend resets sockets liberally (observed 2026-07-02), so a dead
                    // cache entry is an expected state, not an error: reconnect fresh below. The
                    // HTTP fallback is reserved for a FRESH connection failing.
                    Err(e) => {
                        tracing::debug!(error = %e, "cached codex ws unusable; reconnecting")
                    }
                }
            }
            // props mismatch: this connection cannot serve the call — drop it and dial fresh.
        }

        let mut ws = self.open_socket().await?;
        send_create(&mut ws, body).await?;
        let first = first_frame(&mut ws, self.first_frame_timeout).await?;
        Ok(self.committed_stream(ws, first, props, input))
    }
}

#[cfg(test)]
// A-37: this module's couple of `serde_json::from_str` calls parse test-authored fixture/request
// JSON with `.expect(...)` — trusted test data, not adversarial provider bytes, so they're outside
// the model-stream invariant `clippy.toml` guards. Allowed at the module scope.
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_defaults_empty_to_gpt56_sol() {
        assert_eq!(resolve_model(""), DEFAULT_MODEL);
        assert_eq!(resolve_model(""), "gpt-5.6-sol");
    }

    #[test]
    fn resolve_model_rewrites_legacy_codex_suffix() {
        // The ChatGPT-subscription backend rejects `*-codex` ids with HTTP 400.
        assert_eq!(resolve_model("gpt-5-codex"), "gpt-5.6-sol");
        assert_eq!(resolve_model("o3-codex"), "gpt-5.6-sol");
    }

    #[test]
    fn resolve_model_maps_gpt56_alias_to_concrete_model() {
        assert_eq!(resolve_model("gpt-5.6"), "gpt-5.6-sol");
        assert_eq!(resolve_model("gpt-5.6-sol"), "gpt-5.6-sol");
    }

    #[test]
    fn resolve_model_passes_current_ids_through_verbatim() {
        assert_eq!(resolve_model("gpt-5.5"), "gpt-5.5");
        assert_eq!(resolve_model("gpt-5"), "gpt-5");
        // A future id is honoured without a flux release.
        assert_eq!(resolve_model("gpt-6"), "gpt-6");
    }

    /// C-159: the session-scoped WS transport is the codex default again (measured 37% consistent
    /// vs HTTP's erratic 0–56%), and `FLUX_CODEX_WS=off` is the escape hatch. `ws_enabled_value`
    /// is the whole gate `oauth_at` branches on, so pinning its truthiness pins the default —
    /// without mutating process env in a parallel test run.
    #[test]
    fn ws_transport_is_the_default_with_an_off_switch() {
        // Unset ⇒ WS. Only an explicit negative turns the transport off.
        assert!(ws_enabled_value(None), "unset must mean WS");
        for (value, want) in [
            ("off", false),
            ("0", false),
            ("false", false),
            ("no", false),
            ("OFF", false),
            (" off ", false),
            ("on", true),
            ("1", true),
            ("true", true),
            ("yes", true),
            // Anything unrecognized keeps the default rather than silently disabling the cache.
            ("", true),
            ("maybe", true),
        ] {
            assert_eq!(
                ws_enabled_value(Some(value)),
                want,
                "FLUX_CODEX_WS={value:?}"
            );
        }
    }

    #[test]
    fn ws_url_derived_from_codex_endpoint() {
        assert_eq!(
            derive_ws_url(CODEX_ENDPOINT),
            "wss://chatgpt.com/backend-api/codex/responses"
        );
        // Hermetic tests point at plain-http local stubs.
        assert_eq!(
            derive_ws_url("http://127.0.0.1:1234/x"),
            "ws://127.0.0.1:1234/x"
        );
    }

    // --- WS transport (C-07): default WS, SSE-equivalent chunks, auth on handshake, HTTP fallback

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::handshake::server::{
        Request as WsRequest, Response as WsResponse,
    };
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    use flux_core::{Chunk, ContentBlock, Error, Result, StopReason};
    use flux_provider::{with_retry_observer, Provider, Request, RetryEvent, RetryObserver};

    /// A token source with a fixed token + account id (what `import_codex` would yield).
    struct StubTokens;
    #[async_trait]
    impl TokenSource for StubTokens {
        async fn access_token(&self) -> Result<String> {
            Ok("tok".to_string())
        }
        fn account_id(&self) -> Option<String> {
            Some("acct_123".to_string())
        }
    }

    #[derive(Default)]
    struct RetryCounter(AtomicUsize);

    impl RetryObserver for RetryCounter {
        fn retrying(&self, _event: &RetryEvent) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The fixture Responses event payloads — shared verbatim between the WS frames and the SSE
    /// body so the two transports are proven equivalent over the *same* wire events.
    fn fixture_events() -> Vec<String> {
        vec![
            r#"{"type":"response.output_text.delta","delta":"Hi"}"#.to_string(),
            r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"fc_1","name":"read","arguments":"{\"path\":\"a.txt\"}"}}"#.to_string(),
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":9,"output_tokens":4}}}"#.to_string(),
        ]
    }

    fn fixture_sse() -> String {
        fixture_events()
            .iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect()
    }

    /// The live protocol's WS-only preamble event: `codex.rate_limits` arrives immediately after
    /// the upgrade, *before* the request is validated (observed live 2026-07-02). The transport
    /// must not treat it as the committing data frame.
    fn rate_limits_preamble() -> String {
        r#"{"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":1.0}}}"#.to_string()
    }

    /// Live WS frame sequence: the preamble followed by the shared fixture events.
    fn live_ws_frames() -> Vec<String> {
        let mut frames = vec![rate_limits_preamble()];
        frames.extend(fixture_events());
        frames
    }

    type HeaderLog = Arc<Mutex<HashMap<String, String>>>;
    /// The last request frame the stub read from the client (the WS request message).
    type RequestLog = Arc<Mutex<Option<String>>>;

    /// A local WS stub: accepts connections, records each handshake's headers and the client's
    /// request frame, streams `frames` as text messages, then closes cleanly. Returns
    /// (ws url, accept-loop handle, connection counter, handshake headers, request frame).
    // The tungstenite handshake callback's `Err` type (http::Response) is fixed by the API and
    // happens to be large; this is a test stub, not a hot Result path.
    #[allow(clippy::result_large_err)]
    async fn ws_stub_server(
        frames: Vec<String>,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        Arc<AtomicUsize>,
        HeaderLog,
        RequestLog,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let headers: HeaderLog = Arc::new(Mutex::new(HashMap::new()));
        let request: RequestLog = Arc::new(Mutex::new(None));
        let (hits2, headers2, request2) = (hits.clone(), headers.clone(), request.clone());
        let handle = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let log = headers2.clone();
                let cb = move |req: &WsRequest, resp: WsResponse| {
                    let mut m = log.lock().unwrap();
                    for (k, v) in req.headers() {
                        m.insert(k.as_str().to_string(), v.to_str().unwrap_or("").to_string());
                    }
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(sock, cb).await else {
                    continue;
                };
                // The client's request message (live contract: a `response.create` event).
                if let Some(Ok(WsMessage::Text(t))) = ws.next().await {
                    *request2.lock().unwrap() = Some(t.to_string());
                }
                for f in &frames {
                    if ws.send(WsMessage::Text(f.clone().into())).await.is_err() {
                        break;
                    }
                }
                let _ = ws.close(None).await;
                // Drain until the close handshake completes.
                while let Some(Ok(_)) = ws.next().await {}
            }
        });
        (format!("ws://{addr}"), handle, hits, headers, request)
    }

    /// A WS stub that sends `frames` and then DROPS the socket without a close handshake — the
    /// live backend's observed end-of-turn behavior (a reset after the terminal event instead of
    /// a clean close). Returns (ws url, handle, connection counter).
    async fn ws_reset_after_frames_server(
        frames: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
                    continue;
                };
                let _ = ws.next().await; // request frame
                for f in &frames {
                    if ws.send(WsMessage::Text(f.clone().into())).await.is_err() {
                        break;
                    }
                }
                drop(ws); // no close handshake — the socket just goes away
            }
        });
        (format!("ws://{addr}"), handle, hits)
    }

    /// A WS stub that accepts the handshake, reads the request frame, then closes with a 1008
    /// policy violation **before any data frame** — the upstream failure mode WS fallback exists
    /// for. Returns (ws url, handle, connection counter).
    async fn ws_policy_close_server() -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
                    continue;
                };
                let _ = ws.next().await; // request frame
                let _ = ws
                    .close(Some(CloseFrame {
                        code: CloseCode::Policy,
                        reason: "policy violation".into(),
                    }))
                    .await;
                while let Some(Ok(_)) = ws.next().await {}
            }
        });
        (format!("ws://{addr}"), handle, hits)
    }

    /// A WS stub that accepts the handshake, reads the request frame, then sends nothing and
    /// closes nothing — a proxy that accepts the upgrade and then blackholes the socket (C-28).
    /// The connection is held open (via `std::future::pending`) until the caller aborts the
    /// returned task. Returns (ws url, handle, connection counter).
    async fn ws_blackhole_server() -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
                    continue;
                };
                let _ = ws.next().await; // request frame
                                         // Blackhole: never send a frame, never close. Held alive until aborted.
                std::future::pending::<()>().await;
            }
        });
        (format!("ws://{addr}"), handle, hits)
    }

    /// A WS stub that sends `frames` (none of which is a terminal event) and then performs a
    /// **clean WS close** — a policy-close or reset arriving mid-turn, before the terminal event
    /// (C-28). Returns (ws url, handle, connection counter).
    async fn ws_close_before_terminal_server(
        frames: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let Ok(mut ws) = tokio_tungstenite::accept_async(sock).await else {
                    continue;
                };
                let _ = ws.next().await; // request frame
                for f in &frames {
                    if ws.send(WsMessage::Text(f.clone().into())).await.is_err() {
                        break;
                    }
                }
                let _ = ws
                    .close(Some(CloseFrame {
                        code: CloseCode::Normal,
                        reason: "done".into(),
                    }))
                    .await;
                while let Some(Ok(_)) = ws.next().await {}
            }
        });
        (format!("ws://{addr}"), handle, hits)
    }

    /// A local HTTP stub answering every request with a 200 `text/event-stream` carrying `body`.
    /// Returns (base url, accept-loop handle, connection counter).
    async fn sse_server(body: String) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await; // best-effort drain of the request
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}/"), handle, hits)
    }

    /// A local HTTP stub that answers every request with one non-success status and the exact
    /// provider body supplied by the test. The hit counter distinguishes the initial request from
    /// retry attempts without coupling the assertion to backoff timing.
    async fn error_server(
        status: u16,
        reason: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}/"), handle, hits)
    }

    /// One ordinary throttle followed by success, proving the terminal classifier is narrow.
    async fn transient_429_server() -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let attempt = hits2.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let response = if attempt == 0 {
                    "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                };
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}/"), handle, hits)
    }

    /// C-545 failing-first contract: the ChatGPT backend's weekly usage limit is a terminal 429,
    /// not a transient per-minute throttle. It must surface on the initial request, retaining the
    /// vendor's reset timestamp and operator message byte-for-byte.
    #[tokio::test]
    async fn usage_limit_exceeded_429_surfaces_without_retry_and_preserves_body() {
        const BODY: &str = r#"{"type":"usageLimitExceeded","message":"Usage limit reached; try again at Aug 11, 2026 09:00 UTC","reset_at":"2026-08-11T09:00:00Z"}"#;
        let (url, handle, hits) = error_server(429, "Too Many Requests", BODY).await;
        let provider = http_native(Arc::new(StubTokens), &url).with_max_retries(1);
        let retries = Arc::new(RetryCounter::default());

        let err = match with_retry_observer(
            retries.clone(),
            provider.stream(Request::new("gpt-5.6-sol", "hi")),
        )
        .await
        {
            Err(err) => err,
            Ok(_) => panic!("a terminal usage limit must fail the call"),
        };
        match err {
            Error::Api { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, BODY, "limit/reset detail must survive verbatim");
            }
            other => panic!("expected the raw provider API error, got {other}"),
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "terminal quota exhaustion must make zero retry attempts"
        );
        assert_eq!(
            retries.0.load(Ordering::SeqCst),
            0,
            "terminal quota exhaustion must not enter RetryReason::Status(429)"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn bare_429_remains_transient_and_retries() {
        let (url, handle, hits) = transient_429_server().await;
        let provider = http_native(Arc::new(StubTokens), &url).with_max_retries(1);
        let retries = Arc::new(RetryCounter::default());

        let result = with_retry_observer(
            retries.clone(),
            provider.stream(Request::new("gpt-5.6-sol", "hi")),
        )
        .await;
        assert!(result.is_ok(), "the retry should recover the bare throttle");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "one transient 429 must make exactly one retry attempt"
        );
        assert_eq!(retries.0.load(Ordering::SeqCst), 1);
        handle.abort();
    }

    /// The codex provider with the WS transport attached unconditionally — what `oauth_at` builds
    /// by default. The WS tests below go through this rather than `oauth_at` so they exercise the
    /// transport regardless of the ambient `FLUX_CODEX_WS` value.
    fn ws_at(tokens: Arc<dyn TokenSource>, endpoint: &str, ws_url: &str) -> NativeProvider {
        oauth_at_timeout(tokens, endpoint, ws_url, DEFAULT_FIRST_FRAME_TIMEOUT)
    }

    async fn collect_chunks(provider: &NativeProvider) -> Vec<Chunk> {
        let mut stream = provider
            .stream(Request::new("gpt-5.5", "hi"))
            .await
            .expect("stream should open");
        let mut out = Vec::new();
        while let Some(c) = stream.next().await {
            out.push(c.expect("chunk"));
        }
        out
    }

    /// With the WS transport attached (the default), WS is dialed and HTTP stays cold. The default
    /// itself is pinned by `ws_transport_is_the_default_with_an_off_switch`, which does not depend
    /// on ambient env.
    #[tokio::test]
    async fn ws_transport_dials_ws_and_leaves_http_cold() {
        let (ws_url, ws_handle, ws_hits, _, _) = ws_stub_server(live_ws_frames()).await;
        // An HTTP stub that must stay cold: WS takes precedence when it is attached.
        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;

        let provider = ws_at(Arc::new(StubTokens), &http_url, &ws_url);
        let chunks = collect_chunks(&provider).await;

        assert_eq!(ws_hits.load(Ordering::SeqCst), 1, "WS must be dialed first");
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            0,
            "HTTP must not be touched when WS succeeds"
        );
        assert!(
            chunks.contains(&Chunk::TextDelta("Hi".to_string())),
            "the WS turn must stream the fixture text"
        );
        assert_eq!(
            chunks.last(),
            Some(&Chunk::Done {
                stop_reason: Some(StopReason::ToolUse)
            })
        );
        ws_handle.abort();
        http_handle.abort();
    }

    #[tokio::test]
    async fn ws_frames_map_to_same_chunks_as_sse() {
        // WS side: the live frame sequence (preamble + fixture events) — the SSE side never sees
        // the preamble, so equality also proves the preamble is transparent.
        let (ws_url, ws_handle, _, _, _) = ws_stub_server(live_ws_frames()).await;
        // A dead HTTP endpoint proves the WS side never falls back mid-test.
        let ws_provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);
        let ws_chunks = collect_chunks(&ws_provider).await;

        // SSE side: the *same* events over plain HTTP+SSE through the same codec.
        let (http_url, http_handle, _) = sse_server(fixture_sse()).await;
        let sse_provider = http_native(Arc::new(StubTokens), &http_url);
        let sse_chunks = collect_chunks(&sse_provider).await;

        assert!(!sse_chunks.is_empty(), "fixture must produce chunks");
        assert_eq!(
            ws_chunks, sse_chunks,
            "WS frames must map to the identical Chunk sequence as the SSE path"
        );
        // Sanity: the sequence carries the assembled tool call.
        assert!(ws_chunks.iter().any(|c| matches!(
            c,
            Chunk::Block(ContentBlock::ToolUse { name, .. }) if name == "read"
        )));
        ws_handle.abort();
        http_handle.abort();
    }

    #[tokio::test]
    async fn ws_handshake_carries_auth_headers() {
        let (ws_url, ws_handle, _, headers, _) = ws_stub_server(live_ws_frames()).await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);
        let _ = collect_chunks(&provider).await;

        let h = headers.lock().unwrap();
        assert_eq!(
            h.get("authorization").map(String::as_str),
            Some("Bearer tok"),
            "Bearer token must be on the handshake"
        );
        assert_eq!(
            h.get("chatgpt-account-id").map(String::as_str),
            Some("acct_123"),
            "chatgpt-account-id must be on the handshake"
        );
        assert_eq!(
            h.get("openai-beta").map(String::as_str),
            Some("responses=experimental"),
            "OpenAI-Beta must be on the handshake"
        );
        assert_eq!(
            h.get("originator").map(String::as_str),
            Some("codex_cli_rs"),
            "originator must be on the handshake"
        );
        ws_handle.abort();
    }

    #[tokio::test]
    async fn ws_failure_falls_back_to_http() {
        // The WS side accepts the handshake then closes 1008 (policy) before any data frame.
        let (ws_url, ws_handle, ws_hits) = ws_policy_close_server().await;
        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;

        let provider = ws_at(Arc::new(StubTokens), &http_url, &ws_url);
        let chunks = collect_chunks(&provider).await;

        assert_eq!(ws_hits.load(Ordering::SeqCst), 1, "WS must be attempted");
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            1,
            "the 1008 policy close must fall back to HTTP-SSE"
        );
        assert!(
            chunks.contains(&Chunk::TextDelta("Hi".to_string())),
            "the turn must still complete over HTTP"
        );
        assert_eq!(
            chunks.last(),
            Some(&Chunk::Done {
                stop_reason: Some(StopReason::ToolUse)
            })
        );
        ws_handle.abort();
        http_handle.abort();
    }

    #[tokio::test]
    async fn ws_request_is_a_response_create_envelope() {
        // Live contract (verified 2026-07-02): the first websocket event must be a
        // `response.create` message with the Responses body fields INLINE (a bare body is
        // rejected with "Expected a 'response.create' message as the first websocket event";
        // nesting the body under `response` loses the model).
        let (ws_url, ws_handle, _, _, request) = ws_stub_server(live_ws_frames()).await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);
        let _ = collect_chunks(&provider).await;

        let sent = request
            .lock()
            .unwrap()
            .clone()
            .expect("the stub must have received a request frame");
        let v: serde_json::Value = serde_json::from_str(&sent).expect("request frame is JSON");
        assert_eq!(
            v["type"].as_str(),
            Some("response.create"),
            "the request must be enveloped as a response.create event"
        );
        assert_eq!(
            v["model"].as_str(),
            Some("gpt-5.5"),
            "the Responses body fields must ride inline in the event"
        );
        assert!(
            v["input"].is_array(),
            "the Responses input must ride inline in the event"
        );
        ws_handle.abort();
    }

    #[tokio::test]
    async fn ws_reset_after_terminal_event_ends_the_turn_cleanly() {
        // Live behavior (observed 2026-07-02): after `response.completed` the backend resets the
        // socket instead of performing a close handshake. That must end the turn cleanly — a
        // reset BEFORE the terminal event still surfaces as a stream error (real truncation).
        let (ws_url, ws_handle, _) = ws_reset_after_frames_server(live_ws_frames()).await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);
        let chunks = collect_chunks(&provider).await;

        assert!(
            chunks.contains(&Chunk::TextDelta("Hi".to_string())),
            "the turn must stream the fixture text"
        );
        assert_eq!(
            chunks.last(),
            Some(&Chunk::Done {
                stop_reason: Some(StopReason::ToolUse)
            }),
            "the turn must end cleanly despite the reset"
        );
        ws_handle.abort();
    }

    #[tokio::test]
    async fn ws_error_event_before_data_falls_back_to_http() {
        // The live backend rejects a bad request with an `error` EVENT (a data frame), not a
        // close — observed live 2026-07-02. Committing to WS on it would kill the turn; it must
        // fall back like any other connect-time failure.
        let error_event = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"nope"}}"#;
        let (ws_url, ws_handle, ws_hits, _, _) =
            ws_stub_server(vec![error_event.to_string()]).await;
        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;

        let provider = ws_at(Arc::new(StubTokens), &http_url, &ws_url);
        let chunks = collect_chunks(&provider).await;

        assert_eq!(ws_hits.load(Ordering::SeqCst), 1, "WS must be attempted");
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            1,
            "an error event before data must fall back to HTTP-SSE"
        );
        assert!(
            chunks.contains(&Chunk::TextDelta("Hi".to_string())),
            "the turn must still complete over HTTP"
        );
        ws_handle.abort();
        http_handle.abort();
    }

    #[tokio::test]
    async fn ws_connection_refused_falls_back_to_http() {
        // A connect-time WS failure: the listener stays bound for the whole test and slams every
        // accepted socket shut before the handshake, so the dial fails deterministically. (The
        // previous fixture reserved a port and dropped it — on a busy runner the ephemeral-port
        // allocator could hand that exact port to the sse_server bound next, so the "refused" dial
        // reached the SSE server and http_hits counted the failed upgrade too: 2 connections.)
        let refuser = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url = format!("ws://{}", refuser.local_addr().unwrap());
        let refuse_handle = tokio::spawn(async move {
            while let Ok((sock, _)) = refuser.accept().await {
                drop(sock);
            }
        });

        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;
        let provider = ws_at(Arc::new(StubTokens), &http_url, &ws_url);
        let chunks = collect_chunks(&provider).await;

        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            1,
            "a refused WS connection must fall back to HTTP-SSE"
        );
        assert!(chunks.contains(&Chunk::TextDelta("Hi".to_string())));
        refuse_handle.abort();
        http_handle.abort();
    }

    // --- C-28: hardening the fail-fast contract (panic / hang / silent truncation)

    #[tokio::test]
    async fn ws_error_event_over_300_bytes_with_multibyte_char_does_not_panic() {
        // The pre-data `error` event truncation used to slice `&t[..t.len().min(300)]` — a raw
        // byte range that panics when byte 300 isn't a char boundary. Build a >300-byte error
        // event with a multibyte char straddling exactly that offset to reproduce it: 299 bytes
        // of ASCII, then a 3-byte UTF-8 char occupying bytes 299/300/301, so byte 300 sits mid-
        // codepoint.
        let prefix =
            r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":""#;
        let pad = 299usize
            .checked_sub(prefix.len())
            .expect("fixture prefix must be shorter than the 299-byte pad target");
        let mut payload = String::new();
        payload.push_str(prefix);
        payload.push_str(&"a".repeat(pad));
        payload.push('€'); // 3-byte UTF-8 char starting at byte 299, straddling byte 300
        payload.push_str(&"a".repeat(50));
        payload.push_str(r#""}}"#);
        assert!(
            payload.len() > 300,
            "fixture must exceed the 300-byte truncation window"
        );
        assert!(
            !payload.is_char_boundary(300),
            "fixture must straddle byte 300 with a multibyte char to reproduce the panic"
        );
        serde_json::from_str::<serde_json::Value>(&payload).expect("fixture must be valid JSON");

        let (ws_url, ws_handle, ws_hits, _, _) = ws_stub_server(vec![payload]).await;
        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;

        let provider = ws_at(Arc::new(StubTokens), &http_url, &ws_url);
        let chunks = collect_chunks(&provider).await;

        assert_eq!(ws_hits.load(Ordering::SeqCst), 1, "WS must be attempted");
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            1,
            "a >300-byte error event must fall back to HTTP-SSE instead of panicking"
        );
        assert!(
            chunks.contains(&Chunk::TextDelta("Hi".to_string())),
            "the turn must still complete over HTTP"
        );
        ws_handle.abort();
        http_handle.abort();
    }

    #[tokio::test]
    async fn ws_first_frame_timeout_falls_back_to_http() {
        // A proxy that accepts the WS upgrade and then blackholes the socket (never sends a frame,
        // never closes) must not pend the turn forever — connect() needs a bounded first-frame
        // timeout so the HTTP fallback engages instead.
        let (ws_url, ws_handle, ws_hits) = ws_blackhole_server().await;
        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;

        let provider = oauth_at_timeout(
            Arc::new(StubTokens),
            &http_url,
            &ws_url,
            Duration::from_millis(200),
        );

        // Guard with a generous outer timeout: pre-fix this pends forever, so without the guard a
        // still-broken transport would hang the test suite instead of failing this test.
        let chunks = tokio::time::timeout(Duration::from_secs(10), collect_chunks(&provider))
            .await
            .expect("connect must time out and fall back rather than hang the turn indefinitely");

        assert_eq!(ws_hits.load(Ordering::SeqCst), 1, "WS must be attempted");
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            1,
            "a blackholed WS connect must time out and fall back to HTTP-SSE"
        );
        assert!(
            chunks.contains(&Chunk::TextDelta("Hi".to_string())),
            "the turn must still complete over HTTP"
        );
        ws_handle.abort();
        http_handle.abort();
    }

    // --- session-scoped WS (C-159): connection reuse, incremental input, sticky routing

    type FrameLog = Arc<Mutex<Vec<String>>>;

    /// One scripted response: a text delta followed by a clean `response.completed` carrying `id`.
    fn scripted_response(id: &str, text: &str) -> Vec<String> {
        vec![
            format!(r#"{{"type":"response.output_text.delta","delta":"{text}"}}"#),
            format!(
                r#"{{"type":"response.completed","response":{{"id":"{id}","usage":{{"input_tokens":9,"output_tokens":4}}}}}}"#
            ),
        ]
    }

    /// A session-capable WS stub (C-159): every accepted connection serves any number of request
    /// frames, answering the i-th request *globally* (across connections) with `scripts[i]` and
    /// keeping the socket open between requests — the live backend behaviour connection reuse
    /// depends on. Records every request frame and the latest handshake's headers; when
    /// `issue_turn_state` is set, every upgrade response carries it as `x-codex-turn-state`.
    // The tungstenite handshake callback's `Err` type (http::Response) is fixed by the API and
    // happens to be large; this is a test stub, not a hot Result path.
    #[allow(clippy::result_large_err)]
    async fn ws_session_server(
        scripts: Vec<Vec<String>>,
        issue_turn_state: Option<String>,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        Arc<AtomicUsize>,
        FrameLog,
        HeaderLog,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let requests: FrameLog = Arc::new(Mutex::new(Vec::new()));
        let headers: HeaderLog = Arc::new(Mutex::new(HashMap::new()));
        let scripts = Arc::new(scripts);
        let served = Arc::new(AtomicUsize::new(0));
        let (hits2, requests2, headers2) = (hits.clone(), requests.clone(), headers.clone());
        let handle = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                hits2.fetch_add(1, Ordering::SeqCst);
                let (log, issue) = (headers2.clone(), issue_turn_state.clone());
                let cb = move |req: &WsRequest, mut resp: WsResponse| {
                    let mut m = log.lock().unwrap();
                    m.clear();
                    for (k, v) in req.headers() {
                        m.insert(k.as_str().to_string(), v.to_str().unwrap_or("").to_string());
                    }
                    if let Some(token) = &issue {
                        resp.headers_mut()
                            .insert("x-codex-turn-state", token.parse().unwrap());
                    }
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(sock, cb).await else {
                    continue;
                };
                // Serve this connection concurrently with the accept loop, so a fresh connection
                // can be dialed while an old one still idles (the predicate-mismatch path).
                let (requests3, scripts3, served3) =
                    (requests2.clone(), scripts.clone(), served.clone());
                tokio::spawn(async move {
                    while let Some(Ok(WsMessage::Text(t))) = ws.next().await {
                        requests3.lock().unwrap().push(t.to_string());
                        let i = served3.fetch_add(1, Ordering::SeqCst);
                        let Some(frames) = scripts3.get(i) else { break };
                        for f in frames {
                            if ws.send(WsMessage::Text(f.clone().into())).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        (format!("ws://{addr}"), handle, hits, requests, headers)
    }

    /// Drain a stream, returning the concatenated text.
    async fn drain_text(provider: &NativeProvider, req: Request) -> String {
        let mut stream = provider.stream(req).await.expect("stream should open");
        let mut text = String::new();
        while let Some(c) = stream.next().await {
            if let Chunk::Block(ContentBlock::Text { text: t }) = c.expect("chunk") {
                text.push_str(&t);
            }
        }
        text
    }

    fn parse_request(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw).expect("request frame is JSON")
    }

    /// The follow-up request extending the conversation of `Request::new(model, "hi")` by one
    /// assistant reply and one user message — the shape whose wire `input` strictly extends the
    /// first request's.
    fn follow_up(model: &str) -> Request {
        let mut req = Request::new(model, "hi");
        req.messages.push(flux_core::Message::assistant_text("Hi"));
        req.messages.push(flux_core::Message::user_text("next"));
        req
    }

    #[tokio::test]
    async fn ws_reuses_the_connection_and_sends_only_the_delta() {
        // C-159's core: round 2 rides the SAME socket (one upgrade), names the previous response,
        // and sends only the items the server has not seen.
        let (ws_url, handle, hits, requests, _) = ws_session_server(
            vec![
                scripted_response("resp_1", "Hi"),
                scripted_response("resp_2", "Sure"),
            ],
            None,
        )
        .await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);

        assert_eq!(
            drain_text(&provider, Request::new("gpt-5.5", "hi")).await,
            "Hi"
        );
        assert_eq!(drain_text(&provider, follow_up("gpt-5.5")).await, "Sure");

        assert_eq!(hits.load(Ordering::SeqCst), 1, "one upgrade for two rounds");
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        let (r1, r2) = (parse_request(&reqs[0]), parse_request(&reqs[1]));
        assert_eq!(r1["input"].as_array().unwrap().len(), 1);
        assert!(r1.get("previous_response_id").is_none());
        assert_eq!(
            r2["previous_response_id"], "resp_1",
            "the follow-up names the previous response: {r2}"
        );
        let delta = r2["input"].as_array().unwrap();
        assert_eq!(delta.len(), 2, "only the unseen items ride: {r2}");
        assert_eq!(delta[0]["role"], "assistant");
        assert_eq!(delta[1]["role"], "user");
        drop(reqs);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_conversation_rewrite_resends_full_input_on_the_warm_socket() {
        // A rewritten conversation (compaction, fork) is not a prefix extension: incremental send
        // would desync, so the full input rides — but still on the cached connection.
        let (ws_url, handle, hits, requests, _) = ws_session_server(
            vec![
                scripted_response("resp_1", "Hi"),
                scripted_response("resp_2", "Fresh"),
            ],
            None,
        )
        .await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);

        drain_text(&provider, Request::new("gpt-5.5", "hi")).await;
        drain_text(&provider, Request::new("gpt-5.5", "rewritten")).await;

        assert_eq!(hits.load(Ordering::SeqCst), 1, "still one connection");
        let reqs = requests.lock().unwrap();
        let r2 = parse_request(&reqs[1]);
        assert!(
            r2.get("previous_response_id").is_none(),
            "a rewrite must not claim continuity: {r2}"
        );
        assert_eq!(r2["input"].as_array().unwrap().len(), 1, "full resend");
        drop(reqs);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_reuse_predicate_mismatch_opens_a_fresh_connection() {
        // Everything but `input` must match for reuse — a model switch changes the properties the
        // connection was opened for, so it gets a fresh one (and no continuity claim).
        let (ws_url, handle, hits, requests, _) = ws_session_server(
            vec![
                scripted_response("resp_1", "Hi"),
                scripted_response("resp_2", "Other"),
            ],
            None,
        )
        .await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);

        drain_text(&provider, Request::new("gpt-5.5", "hi")).await;
        drain_text(&provider, follow_up("gpt-5")).await;

        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "model switch → new connection"
        );
        let reqs = requests.lock().unwrap();
        let r2 = parse_request(&reqs[1]);
        assert!(
            r2.get("previous_response_id").is_none(),
            "cross-connection continuity is impossible with store:false: {r2}"
        );
        assert_eq!(r2["input"].as_array().unwrap().len(), 3, "full input: {r2}");
        drop(reqs);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_dead_cached_connection_reconnects_without_http_fallback() {
        // The live backend resets sockets liberally, so a cached connection that died is an
        // EXPECTED state: the transport must dial fresh, not surface an HTTP fallback.
        let (ws_url, handle, ws_hits) = ws_reset_after_frames_server(live_ws_frames()).await;
        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;
        let provider = ws_at(Arc::new(StubTokens), &http_url, &ws_url);

        collect_chunks(&provider).await;
        collect_chunks(&provider).await;

        assert_eq!(
            ws_hits.load(Ordering::SeqCst),
            2,
            "the dead cached socket must be replaced by a fresh WS connection"
        );
        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            0,
            "a stale cache entry must never cost the turn its WS path"
        );
        handle.abort();
        http_handle.abort();
    }

    #[tokio::test]
    async fn ws_turn_state_token_replays_on_the_next_upgrade() {
        // The upgrade response issues a sticky-routing token; the next upgrade (forced here by a
        // model switch) must replay it so the fresh connection lands on the same node.
        let (ws_url, handle, _, _, headers) = ws_session_server(
            vec![
                scripted_response("resp_1", "Hi"),
                scripted_response("resp_2", "Other"),
            ],
            Some("ts-abc".to_string()),
        )
        .await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);

        drain_text(&provider, Request::new("gpt-5.5", "hi")).await;
        drain_text(&provider, Request::new("gpt-5", "hi")).await;

        let h = headers.lock().unwrap();
        assert_eq!(
            h.get("x-codex-turn-state").map(String::as_str),
            Some("ts-abc"),
            "the second upgrade must replay the issued token; headers: {h:?}"
        );
        drop(h);
        handle.abort();
    }

    /// An SSE stub that issues `x-codex-turn-state` on every response and records each raw
    /// request head, so the replay of the token as a request header is assertable (C-159).
    async fn sse_server_with_turn_state(
        body: String,
        token: String,
    ) -> (String, tokio::task::JoinHandle<()>, FrameLog) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let raw_requests: FrameLog = Arc::new(Mutex::new(Vec::new()));
        let raw2 = raw_requests.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                raw2.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nx-codex-turn-state: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    token,
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}/"), handle, raw_requests)
    }

    #[tokio::test]
    async fn http_turn_state_token_replays_on_the_next_request() {
        // The sticky-routing echo applies to plain HTTP too (C-159): the first response issues the
        // token, the second request must carry it.
        let (http_url, handle, raw_requests) =
            sse_server_with_turn_state(fixture_sse(), "ts-http".to_string()).await;
        let provider = http_native(Arc::new(StubTokens), &http_url);

        collect_chunks(&provider).await;
        collect_chunks(&provider).await;

        let reqs = raw_requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(
            !reqs[0].contains("x-codex-turn-state"),
            "nothing to replay on the first request"
        );
        assert!(
            reqs[1].contains("x-codex-turn-state: ts-http"),
            "the second request must replay the issued token; got:\n{}",
            reqs[1]
        );
        drop(reqs);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_close_before_terminal_event_surfaces_as_stream_error() {
        // A Close received before the terminal event is real truncation (a policy-close or reset
        // mid-turn) and must surface as a stream error, not a clean end-of-turn that silently
        // ships whatever partial text arrived.
        let non_terminal = fixture_events()[0].clone(); // "response.output_text.delta"
        let (ws_url, ws_handle, _) = ws_close_before_terminal_server(vec![non_terminal]).await;
        let provider = ws_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);

        let mut stream = provider
            .stream(Request::new("gpt-5.5", "hi"))
            .await
            .expect("stream should open — the first frame is a normal data frame");
        let mut saw_error = false;
        while let Some(c) = stream.next().await {
            if c.is_err() {
                saw_error = true;
                break;
            }
        }
        assert!(
            saw_error,
            "a Close before the terminal event must surface as a stream error, not a clean \
             end-of-turn"
        );
        ws_handle.abort();
    }
}

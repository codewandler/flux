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
//! serves the `gpt-5.5` family and rejects the legacy `*-codex`-suffixed ids (`gpt-5-codex`, …)
//! with HTTP 400 ("not supported when using Codex with a ChatGPT account"). [`resolve_model`]
//! encodes that knowledge once so every surface — CLI, SDK, server, TUI, the sub-agent spawner —
//! reaches it as `flux_providers::codex::resolve_model` instead of each carrying its own table.

use std::sync::Arc;

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

use crate::openai::{OpenAiCred, OpenAiResponses, Secret, CODEX_ENDPOINT};

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
/// the `codex` provider with no model (bare `codex`) or with a legacy `*-codex` id.
pub const DEFAULT_MODEL: &str = "gpt-5.5";

/// Resolve a codex model id to what the live ChatGPT-subscription backend accepts.
///
/// - An empty model (the bare `codex` shorthand) → [`DEFAULT_MODEL`] (`gpt-5.5`).
/// - A legacy `*-codex`-suffixed id (`gpt-5-codex`, `o3-codex`, …) → [`DEFAULT_MODEL`]; the
///   backend rejects these with HTTP 400.
/// - Any other id is passed through verbatim, so an explicit current id (`gpt-5.5`, `gpt-5`, …)
///   is sent as-is and a future model is honoured without a flux release.
pub fn resolve_model(model: &str) -> String {
    if model.is_empty() || model.ends_with("-codex") {
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
fn oauth_at(tokens: Arc<dyn TokenSource>, endpoint: &str, ws_url: &str) -> NativeProvider {
    http_native(tokens.clone(), endpoint).with_transport(Arc::new(CodexWsTransport {
        url: ws_url.to_string(),
        tokens,
    }))
}

/// The codex provider on the plain HTTP+SSE path (no WS transport) — the fallback half of
/// [`oauth_at`], and the reference side of the WS/SSE equivalence test.
fn http_native(tokens: Arc<dyn TokenSource>, endpoint: &str) -> NativeProvider {
    NativeProvider::new(
        "codex",
        Arc::new(OpenAiResponses { codex: true }),
        Arc::new(OpenAiCred {
            endpoint: endpoint.to_string(),
            secret: Secret::OAuth(tokens),
            extra: codex_headers(),
            send_account_id: true,
        }),
    )
}

// ---------------------------------------------------------------------------
// WebSocket transport (C-07)
// ---------------------------------------------------------------------------

/// The codex WebSocket transport — the **primary** path for the `codex` provider, mirroring the
/// upstream codex Rust client. It opens `wss://…/codex/responses` with the auth/gating headers on
/// the tungstenite handshake (the reqwest-bound [`OpenAiCred`] cannot serve a WS — same precedent
/// as the realtime provider), sends the Responses body as one text frame, and yields the
/// response-event frames re-enveloped as SSE bytes so the **existing** Responses codec
/// (`map_responses_stream`) parses them — guaranteeing WS and HTTP produce identical chunks.
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

#[async_trait]
impl StreamTransport for CodexWsTransport {
    async fn connect(&self, body: &Value) -> Result<ByteStream> {
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
        }

        let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| Error::Http(format!("ws connect: {e}")))?;
        ws.send(Message::Text(body.to_string()))
            .await
            .map_err(|e| Error::Http(format!("ws send: {e}")))?;

        // Policy failures (the upstream 1008 close) arrive AFTER a successful upgrade — wait for
        // the first data frame before committing to the WS path, so such closes still trigger the
        // HTTP fallback instead of yielding an empty turn.
        let first = loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => break t,
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
        };

        // From here on the turn is committed to WS: mid-stream failures surface as stream errors
        // (matching the HTTP path, which also never retries mid-stream).
        let stream = try_stream! {
            yield sse_frame(&first);
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Text(t)) => yield sse_frame(&t),
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue,
                    Err(e) => Err(Error::Provider(format!("ws stream: {e}")))?,
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_defaults_empty_to_gpt55() {
        assert_eq!(resolve_model(""), DEFAULT_MODEL);
        assert_eq!(resolve_model(""), "gpt-5.5");
    }

    #[test]
    fn resolve_model_rewrites_legacy_codex_suffix() {
        // The ChatGPT-subscription backend rejects `*-codex` ids with HTTP 400.
        assert_eq!(resolve_model("gpt-5-codex"), "gpt-5.5");
        assert_eq!(resolve_model("o3-codex"), "gpt-5.5");
    }

    #[test]
    fn resolve_model_passes_current_ids_through_verbatim() {
        assert_eq!(resolve_model("gpt-5.5"), "gpt-5.5");
        assert_eq!(resolve_model("gpt-5"), "gpt-5");
        // A future id is honoured without a flux release.
        assert_eq!(resolve_model("gpt-6"), "gpt-6");
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

    use flux_core::{Chunk, ContentBlock, Result, StopReason};
    use flux_provider::{Provider, Request};

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

    type HeaderLog = Arc<Mutex<HashMap<String, String>>>;

    /// A local WS stub: accepts connections, records each handshake's headers, reads the client's
    /// request frame, streams `frames` as text messages, then closes cleanly. Returns
    /// (ws url, accept-loop handle, connection counter, recorded handshake headers).
    async fn ws_stub_server(
        frames: Vec<String>,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        Arc<AtomicUsize>,
        HeaderLog,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let headers: HeaderLog = Arc::new(Mutex::new(HashMap::new()));
        let (hits2, headers2) = (hits.clone(), headers.clone());
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
                let _ = ws.next().await; // the client's Responses request body frame
                for f in &frames {
                    if ws.send(WsMessage::Text(f.clone())).await.is_err() {
                        break;
                    }
                }
                let _ = ws.close(None).await;
                // Drain until the close handshake completes.
                while let Some(Ok(_)) = ws.next().await {}
            }
        });
        (format!("ws://{addr}"), handle, hits, headers)
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

    #[tokio::test]
    async fn codex_uses_ws_transport_by_default() {
        let (ws_url, ws_handle, ws_hits, _) = ws_stub_server(fixture_events()).await;
        // An HTTP stub that must stay cold: WS is the primary transport.
        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;

        let provider = oauth_at(Arc::new(StubTokens), &http_url, &ws_url);
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
        // WS side: the fixture events as individual frames.
        let (ws_url, ws_handle, _, _) = ws_stub_server(fixture_events()).await;
        // A dead HTTP endpoint proves the WS side never falls back mid-test.
        let ws_provider = oauth_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);
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
        let (ws_url, ws_handle, _, headers) = ws_stub_server(fixture_events()).await;
        let provider = oauth_at(Arc::new(StubTokens), "http://127.0.0.1:1/", &ws_url);
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

        let provider = oauth_at(Arc::new(StubTokens), &http_url, &ws_url);
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
    async fn ws_connection_refused_falls_back_to_http() {
        // Reserve a port, then drop the listener so the WS dial is refused outright.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url = format!("ws://{}", dead.local_addr().unwrap());
        drop(dead);

        let (http_url, http_handle, http_hits) = sse_server(fixture_sse()).await;
        let provider = oauth_at(Arc::new(StubTokens), &http_url, &ws_url);
        let chunks = collect_chunks(&provider).await;

        assert_eq!(
            http_hits.load(Ordering::SeqCst),
            1,
            "a refused WS connection must fall back to HTTP-SSE"
        );
        assert!(chunks.contains(&Chunk::TextDelta("Hi".to_string())));
        http_handle.abort();
    }
}

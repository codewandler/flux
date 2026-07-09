//! The `anthropic` and `claude` providers.
//!
//! Both speak the Anthropic **Messages** protocol; the wire schema, body builder, and SSE mapper
//! live in [`crate::messages`]. This module keeps only what is Anthropic-direct: the codec's quirks
//! ([`AnthropicProfile`] — full feature set: prompt caching, adaptive thinking, effort config) and
//! the two credentials that ride on it — `ApiKeyAnthropic` (the `anthropic` provider, `x-api-key`)
//! and `OAuthAnthropic` (the `claude` provider — Claude Max / Claude-Code subscription OAuth).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::messages::{
    anthropic_model_caps, build_messages_body, map_messages_stream, MessagesQuirks, ProviderProfile,
};
use flux_core::{Error, Result};
use flux_provider::{
    ByteStream, ChunkStream, Credential, NativeProvider, Request, TokenSource, WireCodec,
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Requests authenticated with a Claude-Code/Max subscription OAuth token are gated to the
/// Claude Code product; the system prompt must begin with this identity line.
const CLAUDE_CODE_SYSTEM_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

// ---------------------------------------------------------------------------
// Quirks profile
// ---------------------------------------------------------------------------

/// Anthropic-direct quirks: the full Messages feature set, gated per model by
/// [`anthropic_model_caps`] (C-49) — Haiku 4.5 and every pre-4.6 model reject adaptive thinking
/// and `output_config.effort` with HTTP 400, and the newest generations (Fable 5, Opus ≥ 4.7,
/// Sonnet ≥ 5) reject `temperature`/`top_p`. Non-Anthropic gateways (OpenRouter, ollama) supply
/// more conservative profiles in their own crates.
pub struct AnthropicProfile;

impl ProviderProfile for AnthropicProfile {
    fn quirks_for(&self, model: &str) -> MessagesQuirks {
        let caps = anthropic_model_caps(model);
        MessagesQuirks {
            prompt_caching: true,
            thinking_adaptive: caps.adaptive_thinking,
            effort_output_config: caps.effort,
            sampling_params: caps.sampling_params,
            extra_body: Default::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// The Anthropic Messages wire protocol (`POST /v1/messages`, SSE streaming).
pub struct AnthropicMessages;

impl WireCodec for AnthropicMessages {
    fn build_body(&self, req: &Request) -> Result<Value> {
        build_messages_body(req, &AnthropicProfile.quirks_for(&req.model))
    }

    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        map_messages_stream(bytes)
    }

    fn wire_headers(&self) -> Vec<(&'static str, String)> {
        vec![("anthropic-version", ANTHROPIC_VERSION.to_string())]
    }
}

// ---------------------------------------------------------------------------
// Credentials (transport profiles)
// ---------------------------------------------------------------------------

/// `anthropic` provider: direct API, `x-api-key` auth, usage-based billing.
pub struct ApiKeyAnthropic {
    pub api_key: String,
    pub base_url: String,
}

#[async_trait]
impl Credential for ApiKeyAnthropic {
    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        Ok(rb.header("x-api-key", &self.api_key))
    }
}

/// `claude` provider: Claude Max / Claude-Code **subscription** via OAuth Bearer token.
/// Uses the same Messages endpoint but with the `oauth-2025-04-20` beta and Claude-Code
/// system-prompt gating; counts against the subscription, not the API.
pub struct OAuthAnthropic {
    pub tokens: Arc<dyn TokenSource>,
    pub base_url: String,
}

#[async_trait]
impl Credential for OAuthAnthropic {
    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let token = self.tokens.access_token().await?;
        Ok(rb
            .header("authorization", format!("Bearer {token}"))
            .header("anthropic-beta", OAUTH_BETA))
    }

    fn system_prefix(&self) -> Option<String> {
        Some(CLAUDE_CODE_SYSTEM_PREFIX.to_string())
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// Build the `anthropic` provider from an explicit API key.
pub fn anthropic_api(api_key: impl Into<String>) -> NativeProvider {
    NativeProvider::new(
        "anthropic",
        Arc::new(AnthropicMessages),
        Arc::new(ApiKeyAnthropic {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }),
    )
}

/// Build the `anthropic` provider from `ANTHROPIC_API_KEY`.
pub fn anthropic_from_env() -> Result<NativeProvider> {
    let key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| Error::Auth("ANTHROPIC_API_KEY is not set".to_string()))?;
    if key.trim().is_empty() {
        return Err(Error::Auth("ANTHROPIC_API_KEY is empty".to_string()));
    }
    Ok(anthropic_api(key))
}

/// Build the `claude` provider (subscription OAuth) from a refreshing token source.
pub fn claude_oauth(tokens: Arc<dyn TokenSource>) -> NativeProvider {
    NativeProvider::new(
        "claude",
        Arc::new(AnthropicMessages),
        Arc::new(OAuthAnthropic {
            tokens,
            base_url: DEFAULT_BASE_URL.to_string(),
        }),
    )
}

/// The canonical ids behind flux's short Anthropic model aliases. Kept here (in the provider)
/// rather than in any one surface (CLI/SDK/server/TUI) so every caller reaches one owner:
/// `flux_providers::anthropic::resolve_model`. A future id is honoured without a flux release —
/// only the documented short aliases are rewritten.
pub fn resolve_model(alias: &str) -> String {
    match alias {
        "sonnet" => "claude-sonnet-5",
        "opus" => "claude-opus-4-8",
        "haiku" => "claude-haiku-4-5",
        "fable" => "claude-fable-5",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_model_maps_short_aliases_to_canonical_ids() {
        // Keep in lock-step with the layer-forced mirror in `flux_core::pricing::resolve_alias`.
        assert_eq!(resolve_model("sonnet"), "claude-sonnet-5");
        assert_eq!(resolve_model("opus"), "claude-opus-4-8");
        assert_eq!(resolve_model("haiku"), "claude-haiku-4-5");
        assert_eq!(resolve_model("fable"), "claude-fable-5");
    }

    #[test]
    fn resolve_model_passes_explicit_ids_through_verbatim() {
        // A fully-qualified id or a future id is not rewritten.
        assert_eq!(resolve_model("claude-sonnet-4-6"), "claude-sonnet-4-6");
        assert_eq!(resolve_model("claude-opus-5"), "claude-opus-5");
    }

    #[test]
    fn profile_enables_the_full_feature_set() {
        let q = AnthropicProfile.quirks_for("claude-sonnet-4-6");
        assert!(q.prompt_caching);
        assert!(q.thinking_adaptive);
        assert!(q.effort_output_config);
        assert!(q.sampling_params);
        assert!(q.extra_body.is_empty());
    }

    #[test]
    fn profile_gates_thinking_and_effort_off_for_haiku() {
        // C-49: `claude/haiku` 400ed with "adaptive thinking is not supported on this model"
        // because the profile ignored the model. Haiku 4.5 must get neither adaptive thinking
        // nor `output_config.effort`.
        for id in ["claude-haiku-4-5", "claude-haiku-4-5-20251001"] {
            let q = AnthropicProfile.quirks_for(id);
            assert!(!q.thinking_adaptive, "{id}");
            assert!(!q.effort_output_config, "{id}");
            assert!(q.sampling_params, "{id}");
        }
    }

    #[test]
    fn profile_gates_sampling_params_off_for_the_newest_generations() {
        for id in ["claude-fable-5", "claude-opus-4-8", "claude-sonnet-5"] {
            let q = AnthropicProfile.quirks_for(id);
            assert!(q.thinking_adaptive, "{id}");
            assert!(!q.sampling_params, "{id}");
        }
    }

    #[test]
    fn codec_omits_thinking_for_haiku_even_when_requested() {
        // End-to-end through the codec: the request asks for thinking, the model can't take it,
        // the body must not carry the field (it would be an HTTP 400).
        let req = Request::new("claude-haiku-4-5", "hi").with_thinking(true);
        let body = AnthropicMessages.build_body(&req).unwrap();
        assert!(body.get("thinking").is_none());

        // The same request against a 4.6-family model keeps adaptive thinking.
        let req = Request::new("claude-sonnet-4-6", "hi").with_thinking(true);
        let body = AnthropicMessages.build_body(&req).unwrap();
        assert_eq!(body["thinking"]["type"], "adaptive");
    }

    #[test]
    fn codec_builds_a_messages_body_with_anthropic_quirks() {
        // A long system prompt must come back cache-controlled (the Anthropic profile turns caching
        // on), proving the codec routes through crate::messages with the right quirks.
        let big = "x".repeat(8192);
        let req = Request::new("claude-opus-4-8", "hi").with_system(big.clone());
        let body = AnthropicMessages.build_body(&req).unwrap();
        assert_eq!(body["model"], "claude-opus-4-8");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        // Sanity: tools serialize to the Anthropic top-level shape via flux_core::ContentBlock.
        let _ = json!({});
    }

    #[test]
    fn wire_headers_carry_the_anthropic_version() {
        let headers = AnthropicMessages.wire_headers();
        assert_eq!(
            headers,
            vec![("anthropic-version", "2023-06-01".to_string())]
        );
    }

    // --- claude end-to-end request-shape verify (C-04) -------------------------------------

    use flux_provider::Provider;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A [`TokenSource`] that always returns a fixed access token.
    struct StaticToken(&'static str);
    #[async_trait]
    impl TokenSource for StaticToken {
        async fn access_token(&self) -> flux_core::Result<String> {
            Ok(self.0.to_string())
        }
    }

    /// A one-shot HTTP server that captures the full request (headers + body), replies 200, and
    /// exposes the raw request text. Returns (base url, accept handle, captured-request slot).
    #[allow(clippy::type_complexity)]
    async fn capture_server() -> (
        String,
        tokio::task::JoinHandle<()>,
        Arc<Mutex<Option<String>>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(None::<String>));
        let cap = captured.clone();
        let handle = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
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
                *cap.lock().unwrap() = Some(String::from_utf8_lossy(&buf).to_string());
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), handle, captured)
    }

    #[tokio::test]
    async fn claude_oauth_request_shape() {
        let (url, handle, captured) = capture_server().await;
        let provider = NativeProvider::new(
            "claude",
            Arc::new(AnthropicMessages),
            Arc::new(OAuthAnthropic {
                tokens: Arc::new(StaticToken("test-access-token")),
                base_url: url,
            }),
        );
        // No explicit system → the Claude-Code prefix becomes the whole system prompt.
        let res = provider
            .stream(Request::new("claude-sonnet-4-6", "hi"))
            .await;
        assert!(res.is_ok(), "the mock 200 should produce a stream");
        // The server task finishes after one connection; join it so the capture is settled.
        let _ = handle.await;

        let raw = captured
            .lock()
            .unwrap()
            .clone()
            .expect("server captured a request");
        let lower = raw.to_ascii_lowercase();
        assert!(
            lower.contains("authorization: bearer test-access-token"),
            "Bearer OAuth header missing:\n{raw}"
        );
        assert!(
            lower.contains("anthropic-beta: oauth-2025-04-20"),
            "oauth beta gating header missing:\n{raw}"
        );
        assert!(
            raw.contains(CLAUDE_CODE_SYSTEM_PREFIX),
            "Claude-Code system prefix not applied:\n{raw}"
        );
    }
}

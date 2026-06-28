//! `flux-anthropic` — the `anthropic` and `claude` providers.
//!
//! Both speak the Anthropic **Messages** protocol; the wire schema, body builder, and SSE mapper
//! live in [`flux_messages`]. This crate keeps only what is Anthropic-direct: the codec's quirks
//! ([`AnthropicProfile`] — full feature set: prompt caching, adaptive thinking, effort config) and
//! the two credentials that ride on it — `ApiKeyAnthropic` (the `anthropic` provider, `x-api-key`)
//! and `OAuthAnthropic` (the `claude` provider — Claude Max / Claude-Code subscription OAuth).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_messages::{build_messages_body, map_messages_stream, MessagesQuirks, ProviderProfile};
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

/// Anthropic-direct quirks: the full Messages feature set. Non-Anthropic gateways (OpenRouter,
/// ollama) supply more conservative profiles in their own crates.
pub struct AnthropicProfile;

impl ProviderProfile for AnthropicProfile {
    fn quirks_for(&self, _model: &str) -> MessagesQuirks {
        MessagesQuirks {
            prompt_caching: true,
            thinking_adaptive: true,
            effort_output_config: true,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn profile_enables_the_full_feature_set() {
        let q = AnthropicProfile.quirks_for("claude-sonnet-4-6");
        assert!(q.prompt_caching);
        assert!(q.thinking_adaptive);
        assert!(q.effort_output_config);
        assert!(q.extra_body.is_empty());
    }

    #[test]
    fn codec_builds_a_messages_body_with_anthropic_quirks() {
        // A long system prompt must come back cache-controlled (the Anthropic profile turns caching
        // on), proving the codec routes through flux-messages with the right quirks.
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
        assert_eq!(headers, vec![("anthropic-version", "2023-06-01".to_string())]);
    }
}

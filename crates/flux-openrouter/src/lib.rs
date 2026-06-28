//! `flux-openrouter` — the `openrouter-anthropic` provider.
//!
//! OpenRouter speaks the Anthropic **Messages** protocol at `/api/v1/messages` (model-agnostic:
//! `model: "z-ai/glm-4.6"`, `model: "openai/gpt-4o"`, …). Routing tool calls through it yields
//! native `tool_use` content blocks that can't leak as inline text — unlike the OpenAI Chat path
//! (the `openrouter` provider in `flux-openai`), which some models corrupt by emitting
//! `<tool_call>` markup. The shared wire/body/stream live in [`flux_messages`]; this crate adds the
//! OpenRouter quirks profile and a Bearer credential with OpenRouter's attribution headers.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_messages::{build_messages_body, map_messages_stream, MessagesQuirks, ProviderProfile};
use flux_provider::{ByteStream, ChunkStream, Credential, NativeProvider, Request, WireCodec};

const OPENROUTER_MESSAGES_ENDPOINT: &str = "https://openrouter.ai/api/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ---------------------------------------------------------------------------
// Quirks profile
// ---------------------------------------------------------------------------

/// OpenRouter quirks. Conservative across the gateway's many non-Claude models: prompt caching and
/// the Anthropic `output_config.effort` are off (not all upstreams accept them); adaptive thinking
/// stays on. `provider.require_parameters` makes OpenRouter route tool requests only to upstreams
/// that actually support `tools`. Per-model refinements (e.g. caching on for Claude slugs) belong
/// in `quirks_for`, which currently ignores the model.
pub struct OpenRouterProfile;

impl ProviderProfile for OpenRouterProfile {
    fn quirks_for(&self, _model: &str) -> MessagesQuirks {
        let mut extra_body = serde_json::Map::new();
        extra_body.insert("provider".into(), json!({ "require_parameters": true }));
        MessagesQuirks {
            prompt_caching: false,
            thinking_adaptive: true,
            effort_output_config: false,
            extra_body,
        }
    }
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// OpenRouter's Anthropic-Messages-compatible wire (`POST /api/v1/messages`, SSE streaming).
pub struct OpenRouterMessages;

impl WireCodec for OpenRouterMessages {
    fn build_body(&self, req: &Request) -> Result<Value> {
        build_messages_body(req, &OpenRouterProfile.quirks_for(&req.model))
    }

    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        map_messages_stream(bytes)
    }

    fn wire_headers(&self) -> Vec<(&'static str, String)> {
        // OpenRouter's Messages endpoint mirrors Anthropic and accepts the version header (the same
        // one Claude Code sends when pointed at OpenRouter via ANTHROPIC_BASE_URL).
        vec![("anthropic-version", ANTHROPIC_VERSION.to_string())]
    }
}

// ---------------------------------------------------------------------------
// Credential
// ---------------------------------------------------------------------------

/// Bearer-token transport for OpenRouter, with the optional `HTTP-Referer` / `X-Title` attribution
/// headers used for app ranking.
pub struct BearerOpenRouter {
    pub api_key: String,
    pub extra: Vec<(&'static str, String)>,
}

#[async_trait]
impl Credential for BearerOpenRouter {
    fn endpoint(&self) -> String {
        OPENROUTER_MESSAGES_ENDPOINT.to_string()
    }

    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let mut rb = rb.header("authorization", format!("Bearer {}", self.api_key));
        for (k, v) in &self.extra {
            rb = rb.header(*k, v);
        }
        Ok(rb)
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// Build the `openrouter-anthropic` provider via API key. `referer`/`title` are OpenRouter's
/// optional attribution headers; pass empty strings to omit.
pub fn openrouter_anthropic_api(
    api_key: impl Into<String>,
    referer: impl Into<String>,
    title: impl Into<String>,
) -> NativeProvider {
    let mut extra = Vec::new();
    let referer = referer.into();
    let title = title.into();
    if !referer.is_empty() {
        extra.push(("HTTP-Referer", referer));
    }
    if !title.is_empty() {
        extra.push(("X-Title", title));
    }
    NativeProvider::new(
        "openrouter-anthropic",
        Arc::new(OpenRouterMessages),
        Arc::new(BearerOpenRouter {
            api_key: api_key.into(),
            extra,
        }),
    )
}

/// Build the `openrouter-anthropic` provider from `OPENROUTER_API_KEY`.
pub fn openrouter_anthropic_from_env() -> Result<NativeProvider> {
    let key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| Error::Auth("OPENROUTER_API_KEY is not set".to_string()))?;
    if key.trim().is_empty() {
        return Err(Error::Auth("OPENROUTER_API_KEY is empty".to_string()));
    }
    Ok(openrouter_anthropic_api(
        key,
        "https://github.com/codewandler/flux",
        "flux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_is_conservative_and_routes_tool_capable() {
        let q = OpenRouterProfile.quirks_for("z-ai/glm-4.6");
        assert!(!q.prompt_caching);
        assert!(!q.effort_output_config);
        assert!(q.thinking_adaptive);
        assert_eq!(q.extra_body["provider"]["require_parameters"], true);
    }

    #[test]
    fn codec_body_carries_require_parameters_and_no_anthropic_extras() {
        // Long system prompt + effort set: under the OpenRouter profile neither should produce the
        // Anthropic-only fields, but the routing directive must be present.
        let big = "x".repeat(8192);
        let req = Request::new("z-ai/glm-4.6", "hi")
            .with_system(big)
            .with_effort(flux_provider::Effort::High);
        let body = OpenRouterMessages.build_body(&req).unwrap();
        assert_eq!(body["provider"]["require_parameters"], true);
        assert!(body["system"].is_string()); // caching off → plain string, not a cache_control array
        assert!(body.get("output_config").is_none()); // effort off
    }

    #[test]
    fn credential_targets_the_messages_endpoint_with_attribution() {
        let cred = BearerOpenRouter {
            api_key: "sk-or-test".into(),
            extra: vec![("X-Title", "flux".into())],
        };
        assert_eq!(cred.endpoint(), OPENROUTER_MESSAGES_ENDPOINT);
    }
}

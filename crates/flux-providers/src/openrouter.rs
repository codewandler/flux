//! The `openrouter` gateway on its Anthropic **Messages** endpoint.
//!
//! OpenRouter speaks the Messages protocol at `/api/v1/messages` (model-agnostic:
//! `model: "z-ai/glm-4.6"`, `model: "openai/gpt-4o"`, …). Routing tool calls through it yields
//! native `tool_use` content blocks that can't leak as inline text — unlike the OpenAI Chat path
//! (the same `openrouter` provider on [`crate::openai`]'s codec), which some models corrupt by
//! emitting `<tool_call>` markup. The shared wire/body/stream live in [`crate::messages`] and the
//! codec in [`crate::anthropic`]; this module adds the OpenRouter quirks profile and a Bearer
//! credential with OpenRouter's attribution headers.
//!
//! Until C-169 this was reachable only as a separate provider name, `openrouter-anthropic`, so the
//! obvious spelling `openrouter/anthropic/<model>` silently took the Chat path — which emits no
//! `cache_control` and so ran at 0% cache, and which leaks tool calls as text for every other
//! vendor. This is now the only wire flux uses for `openrouter`: it is model-agnostic, so there was
//! never a reason to make users choose, and the Chat codec for OpenRouter is gone.
//!
//! A spec reads `openrouter/<vendor>/<model_id>` — a triple — because the vendor prefix is part of
//! OpenRouter's own model id, not a flux-side selector. [`OpenRouterProfile`] still keys prompt
//! caching on that prefix, since only Anthropic-served models honour `cache_control`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::anthropic::AnthropicMessages;
use crate::messages::{anthropic_model_caps, MessagesQuirks, ProviderProfile};
use flux_core::{Error, Result};
use flux_provider::{Credential, NativeProvider};

use crate::schema::openrouter_tools;

const OPENROUTER_MESSAGES_ENDPOINT: &str = "https://openrouter.ai/api/v1/messages";

// ---------------------------------------------------------------------------
// Quirks profile
// ---------------------------------------------------------------------------

/// OpenRouter quirks. Conservative across the gateway's many non-Claude models: the Anthropic
/// `output_config.effort` is off (not all upstreams accept it); adaptive thinking stays on for
/// non-Anthropic vendors. `provider.require_parameters` makes OpenRouter route tool requests only
/// to upstreams that actually support `tools`. Prompt caching is the first model-keyed refinement
/// (C-35): OpenRouter passes `cache_control` through to Anthropic-served models — where I-03
/// measured gather-shaped turns billing the ~20k prefix fully uncached (+35% corpus spend) — so
/// `anthropic/…` slugs cache and every other vendor stays conservative (an upstream that rejects
/// the field would 4xx). Anthropic-served slugs additionally take the per-model capability gating
/// (C-49): OpenRouter forwards `thinking`/`temperature`/`top_p` verbatim, so a slug like
/// `anthropic/claude-3.5-haiku` 400s on adaptive thinking exactly as Anthropic-direct does.
pub struct OpenRouterProfile;

impl ProviderProfile for OpenRouterProfile {
    fn quirks_for(&self, model: &str) -> MessagesQuirks {
        let mut extra_body = serde_json::Map::new();
        extra_body.insert("provider".into(), json!({ "require_parameters": true }));
        // Vendor-prefix match, not substring: `anthropic/claude-…` is Anthropic-served by
        // construction; a third-party slug that merely mentions "claude" is not.
        let anthropic_served = model.starts_with("anthropic/");
        let caps = anthropic_model_caps(model);
        MessagesQuirks {
            prompt_caching: anthropic_served,
            // C-170, live-verified 2026-07-28 against `/api/v1/messages`: a `ttl: "1h"` breakpoint
            // is accepted (no 4xx) and lands in the right tier — the response's per-TTL split
            // reported `ephemeral_1h_input_tokens: 7725` where the plain ephemeral form reported
            // `ephemeral_5m_input_tokens: 7725` for the identical prompt. Verified even with
            // OpenRouter routing the call to Amazon Bedrock upstream. Scoped to anthropic-served
            // slugs for the same reason `prompt_caching` is: no other vendor honours the member.
            extended_cache_ttl: anthropic_served,
            thinking_adaptive: if anthropic_served {
                caps.adaptive_thinking
            } else {
                true
            },
            effort_output_config: false,
            sampling_params: if anthropic_served {
                caps.sampling_params
            } else {
                true
            },
            extra_body,
        }
    }
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// OpenRouter's Anthropic-Messages-compatible wire (`POST /api/v1/messages`, SSE streaming).
///
/// No codec of its own since C-168 — it is the shared [`AnthropicMessages`] under
/// [`OpenRouterProfile`], plus the Gemini tool-schema view. OpenRouter's Messages endpoint mirrors
/// Anthropic and accepts the `anthropic-version` header (the same one Claude Code sends when
/// pointed at OpenRouter via `ANTHROPIC_BASE_URL`), which the shared codec already emits.
pub fn openrouter_messages_codec() -> AnthropicMessages {
    AnthropicMessages::new(Arc::new(OpenRouterProfile)).with_tool_projection(openrouter_tools)
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

/// Build the `openrouter` provider on its Messages endpoint via API key. `referer`/`title` are
/// OpenRouter's optional attribution headers; pass empty strings to omit.
///
/// The provider name is plain `openrouter` (C-169) — there is no second name to choose, so usage
/// rows and role specs read `openrouter/<vendor>/<model>` for every model the gateway proxies.
pub fn openrouter_messages_api(
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
        "openrouter",
        Arc::new(openrouter_messages_codec()),
        Arc::new(BearerOpenRouter {
            api_key: api_key.into(),
            extra,
        }),
    )
}

/// Build the `openrouter` Messages provider from `OPENROUTER_API_KEY`.
pub fn openrouter_messages_from_env() -> Result<NativeProvider> {
    let key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| Error::Auth("OPENROUTER_API_KEY is not set".to_string()))?;
    if key.trim().is_empty() {
        return Err(Error::Auth("OPENROUTER_API_KEY is empty".to_string()));
    }
    Ok(openrouter_messages_api(
        key,
        "https://github.com/codewandler/flux",
        "flux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_provider::{Request, ToolDef, WireCodec};

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
        let body = openrouter_messages_codec().build_body(&req).unwrap();
        assert_eq!(body["provider"]["require_parameters"], true);
        assert!(body["system"].is_string()); // caching off → plain string, not a cache_control array
        assert!(body.get("output_config").is_none()); // effort off
    }

    #[test]
    fn profile_enables_prompt_caching_for_anthropic_slugs_only() {
        // C-35: OpenRouter passes `cache_control` through to Anthropic-served models, so the
        // caching flip is scoped by the vendor prefix — every other upstream stays conservative.
        let q = OpenRouterProfile.quirks_for("anthropic/claude-sonnet-4.6");
        assert!(q.prompt_caching);
        assert!(q.thinking_adaptive);
        assert_eq!(q.extra_body["provider"]["require_parameters"], true);
        // A vendor whose slug merely mentions claude must NOT flip (prefix match, not substring).
        assert!(!OpenRouterProfile.quirks_for("z-ai/glm-4.6").prompt_caching);
        assert!(
            !OpenRouterProfile
                .quirks_for("someone/claude-clone")
                .prompt_caching
        );
    }

    #[test]
    fn anthropic_slugs_take_the_per_model_capability_gating() {
        // C-49: OpenRouter forwards `thinking` verbatim, so an Anthropic-served pre-4.6 slug
        // must not get adaptive thinking — while non-Anthropic vendors keep the flat default.
        let q = OpenRouterProfile.quirks_for("anthropic/claude-3.5-haiku");
        assert!(!q.thinking_adaptive);
        let q = OpenRouterProfile.quirks_for("anthropic/claude-haiku-4.5");
        assert!(!q.thinking_adaptive);
        assert!(q.sampling_params);
        // The newest Anthropic generations reject sampling params through the gateway too.
        let q = OpenRouterProfile.quirks_for("anthropic/claude-opus-4.8");
        assert!(q.thinking_adaptive);
        assert!(!q.sampling_params);
        // Non-Anthropic vendors: unchanged flat profile.
        let q = OpenRouterProfile.quirks_for("z-ai/glm-4.6");
        assert!(q.thinking_adaptive);
        assert!(q.sampling_params);
    }

    #[test]
    fn codec_body_carries_cache_control_for_anthropic_slugs() {
        // Mirrors anthropic.rs's codec test (C-35): a long system prompt on an `anthropic/` slug
        // comes back cache-controlled through the same crate::messages path; the routing
        // directive still rides along.
        let big = "x".repeat(8192);
        let req = Request::new("anthropic/claude-sonnet-4.6", "hi").with_system(big);
        let body = openrouter_messages_codec().build_body(&req).unwrap();
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["provider"]["require_parameters"], true);
    }

    #[test]
    fn credential_targets_the_messages_endpoint_with_attribution() {
        let cred = BearerOpenRouter {
            api_key: "sk-or-test".into(),
            extra: vec![("X-Title", "flux".into())],
        };
        assert_eq!(cred.endpoint(), OPENROUTER_MESSAGES_ENDPOINT);
    }

    #[test]
    fn gemini_codec_materializes_portable_array_and_required_schemas_without_mutating_request() {
        let mut req = Request::new("google/gemini-3.5-flash", "hi");
        req.tools.push(ToolDef {
            name: "records.merge".into(),
            description: "Merge structured records".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "records": {"type": "array"},
                    "batches": {
                        "type": "array",
                        "items": {"type": "array"}
                    }
                },
                "required": ["records", "batches", "label"]
            }),
        });
        let original = req.clone();

        let body = openrouter_messages_codec().build_body(&req).unwrap();
        let schema = &body["tools"][0]["input_schema"];

        assert_eq!(schema["properties"]["records"]["items"], json!({}));
        assert_eq!(schema["properties"]["batches"]["items"]["items"], json!({}));
        assert_eq!(schema["properties"]["label"], json!({}));
        assert_eq!(req.tools, original.tools);
    }

    #[test]
    fn gemini_codec_rejects_unrepresentable_required_property_with_operation_and_path() {
        let mut req = Request::new("google/gemini-3.5-flash", "hi");
        req.tools.push(ToolDef {
            name: "closed.create".into(),
            description: "Create a closed record".into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": ["missing"],
                "additionalProperties": false
            }),
        });
        let original = req.clone();

        let error = openrouter_messages_codec()
            .build_body(&req)
            .unwrap_err()
            .to_string();

        assert!(error.contains("closed.create"), "error was: {error}");
        assert!(error.contains("/required/0"), "error was: {error}");
        assert_eq!(req.tools, original.tools);
    }
}

//! The `ollama-anthropic` provider (local models over the Messages protocol).
//!
//! Ollama (latest) serves an Anthropic **Messages** compatible endpoint with tool calling at
//! `{OLLAMA_HOST}/v1/messages`, so local models (Qwen, Hermes, …) can return native `tool_use`
//! blocks instead of leaking `<tool_call>` markup through the OpenAI Chat path (the `ollama`
//! provider in the [`crate::openai`] module). The shared wire/body/stream live in
//! [`crate::messages`]; this module adds the ollama quirks profile and a no-auth transport that
//! honours `OLLAMA_HOST`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::anthropic::AnthropicMessages;
use crate::messages::{MessagesQuirks, ProviderProfile};
use flux_core::Result;
use flux_provider::{Credential, NativeProvider};

const DEFAULT_MESSAGES_ENDPOINT: &str = "http://localhost:11434/v1/messages";

// ---------------------------------------------------------------------------
// Quirks profile
// ---------------------------------------------------------------------------

/// Ollama quirks: local models don't implement Anthropic prompt caching or `output_config.effort`,
/// so both are off; adaptive thinking stays on (ollama supports thinking blocks). No `extra_body`.
pub struct OllamaProfile;

impl ProviderProfile for OllamaProfile {
    fn quirks_for(&self, _model: &str) -> MessagesQuirks {
        MessagesQuirks {
            prompt_caching: false,
            extended_cache_ttl: false,
            thinking_adaptive: true,
            effort_output_config: false,
            // Local models take the classic sampling params; nothing to gate.
            sampling_params: true,
            extra_body: Default::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// Ollama's Anthropic-Messages-compatible wire (`POST {host}/v1/messages`, SSE streaming).
///
/// No codec of its own since C-168 — the shared [`AnthropicMessages`] under [`OllamaProfile`], with
/// tool schemas forwarded verbatim (local models take the registered schema as-is).
pub fn ollama_messages_codec() -> AnthropicMessages {
    AnthropicMessages::new(Arc::new(OllamaProfile))
}

// ---------------------------------------------------------------------------
// Credential
// ---------------------------------------------------------------------------

/// No-auth transport: ollama ignores credentials. The endpoint is resolved from `OLLAMA_HOST`.
pub struct NoAuthOllama {
    pub endpoint: String,
}

#[async_trait]
impl Credential for NoAuthOllama {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        Ok(rb)
    }

    /// Ollama has no account credit/quota contract; an unclassified 429 remains retryable.
    fn is_terminal_http_error(&self, _status: u16, _body: &str) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// Resolve the Messages endpoint from `OLLAMA_HOST` (e.g. `http://192.168.1.10:11434` for a remote
/// box). A scheme is added if the host is bare; defaults to `localhost:11434`.
fn ollama_messages_endpoint() -> String {
    match std::env::var("OLLAMA_HOST") {
        Ok(h) if !h.trim().is_empty() => {
            let h = h.trim().trim_end_matches('/');
            let h = if h.contains("://") {
                h.to_string()
            } else {
                format!("http://{h}")
            };
            format!("{h}/v1/messages")
        }
        _ => DEFAULT_MESSAGES_ENDPOINT.to_string(),
    }
}

/// Build the `ollama-anthropic` provider (no credential needed; honours `OLLAMA_HOST`).
pub fn ollama_anthropic_api() -> NativeProvider {
    NativeProvider::new(
        "ollama-anthropic",
        Arc::new(ollama_messages_codec()),
        Arc::new(NoAuthOllama {
            endpoint: ollama_messages_endpoint(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_provider::{Request, WireCodec};

    #[test]
    fn profile_omits_anthropic_only_fields() {
        let q = OllamaProfile.quirks_for("qwen2.5-coder:7b");
        assert!(!q.prompt_caching);
        assert!(!q.effort_output_config);
        assert!(q.thinking_adaptive);
        assert!(q.extra_body.is_empty());
    }

    #[test]
    fn codec_body_has_no_anthropic_extras() {
        let big = "x".repeat(8192);
        let req = Request::new("qwen2.5-coder:7b", "hi")
            .with_system(big)
            .with_effort(flux_provider::Effort::High);
        let body = ollama_messages_codec().build_body(&req).unwrap();
        assert!(body["system"].is_string()); // no cache_control array
        assert!(body.get("output_config").is_none());
        assert!(body.get("provider").is_none());
    }

    #[test]
    fn endpoint_targets_messages_path() {
        // Robust regardless of whether OLLAMA_HOST is set in the environment.
        assert!(ollama_messages_endpoint().ends_with("/v1/messages"));
    }

    #[test]
    fn local_429_is_unclassified_and_remains_transient() {
        let cred = NoAuthOllama {
            endpoint: ollama_messages_endpoint(),
        };
        assert!(!cred.is_terminal_http_error(429, "quota exceeded"));
    }
}

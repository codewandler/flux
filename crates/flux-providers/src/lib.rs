//! `flux-providers` — flux's concrete LLM providers.
//!
//! This crate sits on top of the [`flux_provider`] abstraction (the `Provider`/`WireCodec`/
//! `Credential` traits and the generic `NativeProvider`) and supplies the implementations the CLI
//! wires up by name. It was consolidated from what used to be five separate crates so the
//! tightly-coupled provider layer lives behind a single dependency edge:
//!
//! - [`messages`] — the shared **Anthropic Messages** protocol core (wire schema, body builder, SSE
//!   mapper). Anthropic-direct, OpenRouter, and Ollama all speak this shape; each supplies its own
//!   `ProviderProfile` describing its quirks.
//! - [`anthropic`] — the `anthropic` (API key) and `claude` (subscription OAuth) providers.
//! - [`openrouter`] — the `openrouter` gateway, on its Messages endpoint for every model it proxies
//!   (native tool calling, prompt caching for `anthropic/…` slugs).
//! - [`ollama`] — the `ollama-anthropic` provider (local models over the Messages protocol).
//! - [`openai`] — the API-key OpenAI Chat / Responses wire codecs and the unified Bearer
//!   credential shared by the OpenAI-family providers (`openai`, `ollama`).
//! - [`codex`] — the `codex` provider (ChatGPT/Codex subscription over the Responses wire on the
//!   ChatGPT backend). It reuses the [`openai`] codec but owns its own surface and model
//!   resolution.
//!
//! Provider **credentials/OAuth** (token sources, PKCE login, CLI-credential import) deliberately
//! stay in the separate `flux-credentials` crate — it is destined to back all integrations, not
//! just LLM providers.

pub mod messages;

mod schema;

pub mod anthropic;
pub mod bedrock;
pub mod codex;
pub mod ollama;
pub mod openai;
pub mod openrouter;

/// Model-spec → provider resolution: parse a `provider/model` spec and build the concrete provider
/// from environment credentials (including the `claude`/`codex` subscription token sources). The
/// one place the CLI and every embedder share, so a spec resolves identically everywhere.
pub mod spec;

/// The OpenAI Realtime (full-duplex, voice-to-voice) provider — WebSocket, behind the `realtime`
/// feature. See [`flux_provider::realtime`] for the seam it implements.
#[cfg(feature = "realtime")]
pub mod realtime;

/// Malformed-envelope corpus (A-37): systematically corrupts each codec's valid fixture streams
/// (truncation / junk-frame injection / single-frame corruption) and asserts the stream-resilience
/// invariant holds. See the module doc for the "add your codec here" registry.
#[cfg(test)]
mod envelope_corpus;

/// OpenRouter's reported-cost rule (C-34), shared by the two wires it proxies (`openai::ChatUsage`
/// on the chat-completions wire, `messages::wire::WireUsage` on the Anthropic-compatible wire).
/// `cost` is the total USD charged. For a BYOK (bring-your-own-key) call, `cost_details
/// .upstream_inference_cost` is the *additional* upstream inference share not folded into `cost`
/// and must be added. **Live-probe finding (2026-07-04): for non-BYOK calls
/// `upstream_inference_cost` DUPLICATES `cost`** — summing it unconditionally would double-count —
/// so the surcharge is added only when `is_byok` is `true`. `cost: None` (the field absent/`null`,
/// i.e. a non-reporting provider) yields `None`: the static pricing table stays the fallback.
pub(crate) fn openrouter_reported_cost(
    cost: Option<f64>,
    is_byok: Option<bool>,
    upstream_inference_cost: Option<f64>,
) -> Option<f64> {
    let cost = cost?;
    let byok_surcharge = if is_byok.unwrap_or(false) {
        upstream_inference_cost.unwrap_or(0.0)
    } else {
        0.0
    };
    Some(cost + byok_surcharge)
}

#[cfg(test)]
mod schema_portability_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::StreamExt;
    use serde_json::{json, Value};

    use flux_core::Result;
    use flux_provider::{Credential, NativeProvider, Provider, Request, ToolDef, WireCodec};

    use crate::anthropic::AnthropicMessages;
    use crate::ollama::ollama_messages_codec;
    use crate::openai::{OpenAiChat, OpenAiResponses};
    use crate::openrouter::openrouter_messages_codec;

    fn adversarial_request(model: &str) -> Request {
        let mut request = Request::new(model, "Reply OK without calling any tool.");
        request.max_tokens = 16;
        request.tools.push(ToolDef {
            name: "inert_portability_probe".into(),
            description: "An inert schema probe. Never call it.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "rows": {"type": "array"},
                    "matrix": {"type": "array", "items": {"type": "array"}},
                    "reference": {"$ref": "#/$defs/Probe"}
                },
                "required": ["rows", "matrix", "label", "reference"],
                "$defs": {
                    "Probe": {
                        "type": "object",
                        "properties": {"value": {"type": "string"}},
                        "required": ["value"]
                    }
                }
            }),
        });
        request
    }

    fn assert_unprojected(schema: &Value) {
        assert!(schema["properties"]["rows"].get("items").is_none());
        assert!(schema["properties"]["matrix"]["items"]
            .get("items")
            .is_none());
        assert!(schema["properties"].get("label").is_none());
    }

    fn assert_projected(schema: &Value) {
        assert_eq!(schema["properties"]["rows"]["items"], json!({}));
        assert_eq!(schema["properties"]["matrix"]["items"]["items"], json!({}));
        assert_eq!(schema["properties"]["label"], json!({}));
    }

    #[test]
    fn provider_codecs_project_only_openrouter_gemini_tool_schemas() {
        let request = adversarial_request("google/gemini-3.5-flash");
        let original = request.tools.clone();

        let anthropic = AnthropicMessages::direct().build_body(&request).unwrap();
        assert_unprojected(&anthropic["tools"][0]["input_schema"]);

        // C-168: the projection is now a codec field, so pin it per transport — ollama shares the
        // codec but must keep forwarding the registered schema verbatim.
        let ollama = ollama_messages_codec().build_body(&request).unwrap();
        assert_unprojected(&ollama["tools"][0]["input_schema"]);

        let openai = OpenAiChat.build_body(&request).unwrap();
        assert_unprojected(&openai["tools"][0]["function"]["parameters"]);

        let codex = OpenAiResponses { codex: true }
            .build_body(&request)
            .unwrap();
        assert_unprojected(&codex["tools"][0]["parameters"]);

        let openrouter_messages = openrouter_messages_codec().build_body(&request).unwrap();
        assert_projected(&openrouter_messages["tools"][0]["input_schema"]);

        // The projection is keyed on the model, not the transport: a non-Gemini OpenRouter slug on
        // the same codec is forwarded verbatim.
        let non_gemini = adversarial_request("deepseek/deepseek-v4-flash");
        let openrouter_non_gemini = openrouter_messages_codec().build_body(&non_gemini).unwrap();
        assert_unprojected(&openrouter_non_gemini["tools"][0]["input_schema"]);
        assert_eq!(request.tools, original);
    }

    struct EndpointCountingCredential {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Credential for EndpointCountingCredential {
        fn endpoint(&self) -> String {
            self.calls.fetch_add(1, Ordering::SeqCst);
            "http://127.0.0.1:9/should-not-connect".into()
        }

        async fn apply(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
            Ok(request)
        }
    }

    #[tokio::test]
    async fn incompatible_gemini_schema_fails_before_either_transport_reaches_endpoint() {
        let fixtures = vec![
            (
                ToolDef {
                    name: "rows.replace".into(),
                    description: "replace".into(),
                    input_schema: json!({
                        "type": "array",
                        "items": {"type": "string"}
                    }),
                },
                "/type",
            ),
            (
                ToolDef {
                    name: "closed.create".into(),
                    description: "create".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {},
                        "required": ["missing"],
                        "additionalProperties": false
                    }),
                },
                "/required/0",
            ),
            (
                ToolDef {
                    name: "patterns.search".into(),
                    description: "search".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "filters": {
                                "type": "object",
                                "patternProperties": {".*": {"type": "string"}}
                            }
                        }
                    }),
                },
                "/properties/filters/patternProperties",
            ),
            (
                ToolDef {
                    name: "unions.put".into(),
                    description: "put".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "value": {
                                "type": ["string", "number"],
                                "anyOf": [{"type": "string"}]
                            }
                        }
                    }),
                },
                "/properties/value/type",
            ),
            (
                ToolDef {
                    name: "enums.put".into(),
                    description: "put".into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "value": {"type": "string", "enum": ["ok", 1]}
                        }
                    }),
                },
                "/properties/value/enum/1",
            ),
        ];

        for (tool, expected_path) in fixtures {
            for codec in [Arc::new(openrouter_messages_codec()) as Arc<dyn WireCodec>] {
                let mut request = Request::new("google/gemini-3.5-flash", "hi");
                request.tools.push(tool.clone());
                let endpoint_calls = Arc::new(AtomicUsize::new(0));
                let provider = NativeProvider::new(
                    "openrouter-test",
                    codec,
                    Arc::new(EndpointCountingCredential {
                        calls: endpoint_calls.clone(),
                    }),
                )
                .with_max_retries(0);

                let error = match provider.stream(request).await {
                    Ok(_) => panic!("incompatible schema reached transport"),
                    Err(error) => error.to_string(),
                };

                assert!(error.contains(&tool.name), "error was: {error}");
                assert!(error.contains(expected_path), "error was: {error}");
                assert_eq!(endpoint_calls.load(Ordering::SeqCst), 0);
            }
        }
    }

    /// Credentialed A-81 smoke. The declaration is inert and no Flux operation is registered or
    /// executed; consuming the stream proves the OpenRouter wire reached Gemini after codec
    /// projection instead of failing request validation.
    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY and makes one low-token Gemini request"]
    async fn live_openrouter_gemini_accepts_projected_schema() -> Result<()> {
        let key = std::env::var("OPENROUTER_API_KEY")
            .map_err(|_| flux_core::Error::Auth("OPENROUTER_API_KEY is not set".into()))?;
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(
            crate::openrouter::openrouter_messages_api(&key, "", ""),
        )];

        for provider in providers {
            let mut stream = provider
                .stream(adversarial_request("google/gemini-3.5-flash"))
                .await?;
            while let Some(chunk) = stream.next().await {
                chunk?;
            }
        }
        Ok(())
    }
}

//! `flux-anthropic` — the Anthropic **Messages** wire codec plus the two credentials that
//! ride on it: `ApiKeyAnthropic` (the `anthropic` provider, `x-api-key`) and `OAuthAnthropic`
//! (the `claude` provider — Claude Max / Claude-Code subscription OAuth).
//!
//! The codec serializes [`flux_core::ContentBlock`] directly (its serde shape matches the wire)
//! and assembles the SSE stream (text, thinking + signature, tool_use input JSON) into
//! normalized [`Chunk`]s.

use std::collections::HashMap;
use std::sync::Arc;

use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};

use flux_core::{Chunk, ContentBlock, Error, Result, Role};
use flux_provider::{
    ByteStream, ChunkStream, Credential, NativeProvider, Request, TokenSource, WireCodec,
};

mod wire;
use wire::{StreamEvent, WireBlock, WireDelta};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Requests authenticated with a Claude-Code/Max subscription OAuth token are gated to the
/// Claude Code product; the system prompt must begin with this identity line.
const CLAUDE_CODE_SYSTEM_PREFIX: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// The Anthropic Messages wire protocol (`POST /v1/messages`, SSE streaming).
pub struct AnthropicMessages;

impl WireCodec for AnthropicMessages {
    fn build_body(&self, req: &Request) -> Result<Value> {
        build_anthropic_body(req)
    }

    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        Box::pin(map_anthropic_stream(bytes))
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

// ---------------------------------------------------------------------------
// Body builder
// ---------------------------------------------------------------------------

/// Build the Anthropic request body from a provider [`Request`].
fn build_anthropic_body(req: &Request) -> Result<Value> {
    let mut system = req.system.clone();
    let mut messages = Vec::new();

    for m in &req.messages {
        match m.role {
            // Anthropic carries the system prompt out of band; fold any system message in.
            Role::System => {
                let text = m.text();
                system = Some(match system {
                    Some(s) => format!("{s}\n\n{text}"),
                    None => text,
                });
            }
            Role::User | Role::Assistant => {
                messages.push(json!({
                    "role": role_str(m.role),
                    "content": serde_json::to_value(&m.content)?,
                }));
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "stream": true,
    });

    if let Some(s) = system {
        body["system"] = system_field(&s);
    }
    if !req.tools.is_empty() {
        body["tools"] = serde_json::to_value(&req.tools)?;
    }
    if req.thinking {
        // Current Anthropic models (Opus 4.8/4.7, Sonnet 4.6) only accept adaptive
        // thinking; the older `{type:"enabled",budget_tokens}` shape now returns 400.
        body["thinking"] = json!({ "type": "adaptive" });
    } else if let Some(t) = req.temperature {
        // Temperature is rejected when thinking is on, so only send it when off.
        body["temperature"] = json!(t);
    }
    if let Some(effort) = req.effort {
        // Depth/cost control. Opt-in: some models (e.g. Haiku) reject it.
        body["output_config"] = json!({ "effort": effort.as_str() });
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if !req.stop_sequences.is_empty() {
        body["stop_sequences"] = json!(req.stop_sequences);
    }
    if !req.metadata.is_empty() {
        body["metadata"] = Value::Object(req.metadata.clone());
    }

    Ok(body)
}

/// Anthropic prompt caching: a system prompt long enough to be worth caching (≈512+ tokens) is sent
/// as a single text block marked `cache_control: ephemeral`, so it's reused across turns at a
/// discount; shorter prompts stay a plain string (below the cache minimum, marking would be wasted).
const CACHE_MIN_CHARS: usize = 4096;

fn system_field(s: &str) -> Value {
    if s.len() >= CACHE_MIN_CHARS {
        json!([{ "type": "text", "text": s, "cache_control": { "type": "ephemeral" } }])
    } else {
        json!(s)
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Assistant => "assistant",
        // System is handled out of band above; default everything else to user.
        _ => "user",
    }
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Per-index accumulator for a content block being streamed.
enum BlockAcc {
    Text(String),
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
}

impl BlockAcc {
    fn from_wire(w: WireBlock) -> Self {
        match w {
            WireBlock::Text { text } => BlockAcc::Text(text),
            WireBlock::Thinking {
                thinking,
                signature,
            } => BlockAcc::Thinking {
                thinking,
                signature,
            },
            WireBlock::RedactedThinking { data } => BlockAcc::RedactedThinking(data),
            WireBlock::ToolUse { id, name, .. } => BlockAcc::ToolUse {
                id,
                name,
                json: String::new(),
            },
        }
    }

    fn finish(self) -> Result<ContentBlock> {
        Ok(match self {
            BlockAcc::Text(text) => ContentBlock::Text { text },
            BlockAcc::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking,
                signature,
            },
            BlockAcc::RedactedThinking(data) => ContentBlock::RedactedThinking { data },
            BlockAcc::ToolUse { id, name, json } => {
                let input = if json.trim().is_empty() {
                    Value::Object(Default::default())
                } else {
                    serde_json::from_str(&json)?
                };
                ContentBlock::ToolUse { id, name, input }
            }
        })
    }
}

/// Map a raw Anthropic SSE byte stream into normalized [`Chunk`]s.
fn map_anthropic_stream(byte_stream: ByteStream) -> impl futures::Stream<Item = Result<Chunk>> {
    try_stream! {
        let mut events = byte_stream.eventsource();
        let mut blocks: HashMap<usize, BlockAcc> = HashMap::new();

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::Provider(format!("sse stream: {e}")))?;
            if event.data.is_empty() {
                continue;
            }

            let parsed: StreamEvent = serde_json::from_str(&event.data)?;
            match parsed {
                StreamEvent::MessageStart { message } => {
                    yield Chunk::MessageStart { model: message.model };
                    yield Chunk::Usage(message.usage.into());
                }
                StreamEvent::ContentBlockStart { index, content_block } => {
                    blocks.insert(index, BlockAcc::from_wire(content_block));
                }
                StreamEvent::ContentBlockDelta { index, delta } => match delta {
                    WireDelta::TextDelta { text } => {
                        if let Some(BlockAcc::Text(t)) = blocks.get_mut(&index) {
                            t.push_str(&text);
                        }
                        yield Chunk::TextDelta(text);
                    }
                    WireDelta::ThinkingDelta { thinking } => {
                        if let Some(BlockAcc::Thinking { thinking: acc, .. }) = blocks.get_mut(&index) {
                            acc.push_str(&thinking);
                        }
                        yield Chunk::ThinkingDelta(thinking);
                    }
                    WireDelta::SignatureDelta { signature } => {
                        if let Some(BlockAcc::Thinking { signature: sig, .. }) = blocks.get_mut(&index) {
                            sig.push_str(&signature);
                        }
                    }
                    WireDelta::InputJsonDelta { partial_json } => {
                        if let Some(BlockAcc::ToolUse { json, .. }) = blocks.get_mut(&index) {
                            json.push_str(&partial_json);
                        }
                    }
                },
                StreamEvent::ContentBlockStop { index } => {
                    if let Some(acc) = blocks.remove(&index) {
                        yield Chunk::Block(acc.finish()?);
                    }
                }
                StreamEvent::MessageDelta { delta, usage } => {
                    if let Some(u) = usage {
                        yield Chunk::Usage(u.into());
                    }
                    if let Some(reason) = delta.stop_reason {
                        yield Chunk::Done { stop_reason: Some(wire::map_stop_reason(&reason)) };
                    }
                }
                StreamEvent::MessageStop => {}
                StreamEvent::Ping => {}
                StreamEvent::Error { error } => {
                    Err(Error::Provider(format!("{}: {}", error.kind, error.message)))?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::{Message, StopReason};
    use flux_provider::{Effort, Request, ToolDef};

    #[test]
    fn body_includes_system_and_tools() {
        let mut req = Request::new("claude-sonnet-4-6", "hi").with_system("be terse");
        req.tools.push(ToolDef {
            name: "read".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        });
        let body = build_anthropic_body(&req).unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["system"], "be terse"); // short → plain string, no cache marker
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["name"], "read");
    }

    #[test]
    fn long_system_prompt_is_cache_controlled() {
        let big = "x".repeat(CACHE_MIN_CHARS + 1);
        let req = Request::new("claude-opus-4-8", "hi").with_system(big.clone());
        let body = build_anthropic_body(&req).unwrap();
        assert_eq!(body["system"][0]["type"], "text");
        assert_eq!(body["system"][0]["text"], big);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn body_folds_system_message_and_enables_thinking() {
        let req = Request {
            messages: vec![Message::system_text("policy"), Message::user_text("go")],
            ..Request::new("m", "ignored")
                .with_thinking(true)
                .with_effort(Effort::High)
        };
        let body = build_anthropic_body(&req).unwrap();
        // system message folded into the system field; only the user message remains.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        // temperature must be omitted when thinking is on.
        assert!(body.get("temperature").is_none());
    }

    #[tokio::test]
    async fn parses_a_full_sse_turn() {
        // A representative Anthropic stream: text + a tool_use whose input arrives in JSON deltas.
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.txt\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":15}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );

        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut text = String::new();
        let mut blocks = Vec::new();
        let mut stop = None;
        let mut last_usage = None;

        let stream = map_anthropic_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                Chunk::TextDelta(t) => text.push_str(&t),
                Chunk::Block(b) => blocks.push(b),
                Chunk::Usage(u) => last_usage = Some(u),
                Chunk::Done { stop_reason } => stop = stop_reason,
                Chunk::MessageStart { .. } => {}
                Chunk::ThinkingDelta(_) => {}
            }
        }

        assert_eq!(text, "Hello world");
        assert_eq!(stop, Some(StopReason::ToolUse));
        assert_eq!(last_usage.unwrap().output_tokens, 15);
        assert_eq!(blocks.len(), 2);
        match &blocks[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "read");
                assert_eq!(input["path"], "a.txt");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }
}

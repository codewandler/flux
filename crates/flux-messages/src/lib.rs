//! `flux-messages` — the shared **Anthropic Messages** protocol core.
//!
//! Anthropic-direct, OpenRouter, and ollama all speak the Messages wire (`POST /v1/messages`, SSE
//! streaming, native `tool_use` content blocks). This crate owns the parts they share — the wire
//! schema ([`wire`]), the request-body builder ([`build_messages_body`]), and the SSE→[`Chunk`]
//! mapper ([`map_messages_stream`]) — while each provider crate supplies its own `WireCodec` +
//! `Credential` and a [`ProviderProfile`] describing its quirks. There is no credential, endpoint,
//! or provider identity here.
//!
//! `flux_core::ContentBlock` already serializes to the Messages content shape, so request content
//! and streamed tool-use blocks round-trip through serde without a translation layer.

use std::collections::HashMap;

use async_stream::try_stream;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};

use flux_core::{Chunk, ContentBlock, Error, Result, Role};
use flux_provider::{ByteStream, ChunkStream};

mod quirks;
mod wire;

pub use quirks::{MessagesQuirks, ProviderProfile};
use wire::{StreamEvent, WireBlock, WireDelta};

// Convenience re-exports so provider crates can depend on `flux-messages` alone for the request
// shape they build against.
pub use flux_provider::{Effort, Request, ToolDef};

// ---------------------------------------------------------------------------
// Body builder
// ---------------------------------------------------------------------------

/// Build a Messages request body from a provider [`Request`], gating the divergent fields on the
/// resolved [`MessagesQuirks`]. The core fields (model/messages/max_tokens/stream/tools/top_p/
/// stop_sequences/metadata) are always emitted; caching, thinking, effort, and any provider-
/// specific `extra_body` are quirk-controlled.
pub fn build_messages_body(req: &Request, q: &MessagesQuirks) -> Result<Value> {
    let mut system = req.system.clone();
    let mut messages = Vec::new();

    for m in &req.messages {
        match m.role {
            // The Messages protocol carries the system prompt out of band; fold any system message in.
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
        body["system"] = system_field(&s, q.prompt_caching);
    }
    if !req.tools.is_empty() {
        body["tools"] = serde_json::to_value(&req.tools)?;
    }
    if req.thinking && q.thinking_adaptive {
        // Current Anthropic models accept only adaptive thinking; the older
        // `{type:"enabled",budget_tokens}` shape now 400s. Temperature is rejected alongside it.
        body["thinking"] = json!({ "type": "adaptive" });
    } else if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if q.effort_output_config {
        if let Some(effort) = req.effort {
            body["output_config"] = json!({ "effort": effort.as_str() });
        }
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
    // Provider-specific top-level fields (e.g. OpenRouter routing). Merged last so a profile can
    // intentionally override a core field if it ever needs to.
    for (k, v) in &q.extra_body {
        body[k.as_str()] = v.clone();
    }

    Ok(body)
}

/// Anthropic prompt caching: a system prompt long enough to be worth caching (≈512+ tokens) is sent
/// as a single text block marked `cache_control: ephemeral`, reused across turns at a discount;
/// shorter prompts (or providers without caching) stay a plain string.
const CACHE_MIN_CHARS: usize = 4096;

fn system_field(s: &str, caching: bool) -> Value {
    if caching && s.len() >= CACHE_MIN_CHARS {
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
// Tool-input parsing (robust against model JSON quirks)
// ---------------------------------------------------------------------------

/// Parse a model's tool-call input JSON, tolerating the two malformations real models emit when
/// streaming structured arguments:
///   - **trailing junk** after a complete value — e.g. an extra `}` (deepseek-v4-flash via OpenRouter)
///   - **an unterminated tail** — a missing trailing `}`/`]` or an open string (glm-5.2 via OpenRouter)
/// Reads the first JSON value (ignoring anything after it); if that fails, balances the unclosed
/// brackets/strings once and retries.
fn parse_tool_input(json: &str) -> std::result::Result<Value, serde_json::Error> {
    fn first_value(s: &str) -> std::result::Result<Value, serde_json::Error> {
        match serde_json::Deserializer::from_str(s)
            .into_iter::<Value>()
            .next()
        {
            Some(r) => r,
            None => Ok(Value::Object(Default::default())), // whitespace only
        }
    }
    match first_value(json) {
        Ok(v) => Ok(v),
        Err(e) => match close_unbalanced_json(json.trim()) {
            Some(repaired) => first_value(&repaired),
            None => Err(e),
        },
    }
}

/// Best-effort close of a JSON value a model left unterminated: append a `"` for an open string and
/// the matching `}`/`]` for every still-open `{`/`[`, in reverse order. Returns `None` when the input
/// is already balanced (so the parse error was something we can't fix by closing brackets).
fn close_unbalanced_json(s: &str) -> Option<String> {
    let mut stack: Vec<char> = Vec::new();
    let mut in_str = false;
    let mut escaped = false;
    for c in s.chars() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    if !in_str && stack.is_empty() {
        return None; // balanced already — not a truncation we can repair
    }
    let mut out = s.to_string();
    if in_str {
        out.push('"');
    }
    while let Some(close) = stack.pop() {
        out.push(close);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Per-index accumulator for a content block being streamed.
enum BlockAcc {
    Text(String),
    Thinking { thinking: String, signature: String },
    RedactedThinking(String),
    ToolUse { id: String, name: String, json: String },
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
                    parse_tool_input(&json).map_err(|e| {
                        Error::Provider(format!(
                            "tool `{name}` input was not valid JSON ({e}); raw: {}",
                            json.chars().take(300).collect::<String>()
                        ))
                    })?
                };
                ContentBlock::ToolUse { id, name, input }
            }
        })
    }
}

/// Map a raw Messages SSE byte stream into normalized [`Chunk`]s (boxed for `WireCodec::map_stream`).
pub fn map_messages_stream(byte_stream: ByteStream) -> ChunkStream {
    Box::pin(map_messages_stream_inner(byte_stream))
}

fn map_messages_stream_inner(byte_stream: ByteStream) -> impl futures::Stream<Item = Result<Chunk>> {
    try_stream! {
        let mut events = byte_stream.eventsource();
        let mut blocks: HashMap<usize, BlockAcc> = HashMap::new();
        // The `message_start` event carries the input/cache token counts; `message_delta` carries
        // only the running output count (its input fields default to 0). Remember the input side so
        // the final usage chunk keeps it, instead of consumers' last-wins assignment zeroing it.
        let mut prior_usage = flux_core::Usage::default();

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::Provider(format!("sse stream: {e}")))?;
            let data = event.data.trim();
            // Anthropic-direct ends the stream with `message_stop`; OpenRouter and ollama (which
            // proxy the Messages shape through OpenAI-compatible plumbing) also append an OpenAI-style
            // `[DONE]` sentinel that isn't JSON. Skip it — and any blank keepalive — before parsing.
            if data.is_empty() || data == "[DONE]" {
                continue;
            }

            let parsed: StreamEvent = serde_json::from_str(data).map_err(|e| {
                Error::Provider(format!(
                    "messages SSE: bad event JSON ({e}); raw: {}",
                    data.chars().take(200).collect::<String>()
                ))
            })?;
            match parsed {
                StreamEvent::MessageStart { message } => {
                    yield Chunk::MessageStart { model: message.model };
                    let u: flux_core::Usage = message.usage.into();
                    prior_usage = u.clone();
                    yield Chunk::Usage(u);
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
                        // Carry the input/cache counts forward from message_start so they aren't
                        // clobbered to 0 by the delta (which only reports output tokens).
                        let mut u: flux_core::Usage = u.into();
                        if u.input_tokens == 0 {
                            u.input_tokens = prior_usage.input_tokens;
                        }
                        if u.cache_creation_input_tokens == 0 {
                            u.cache_creation_input_tokens = prior_usage.cache_creation_input_tokens;
                        }
                        if u.cache_read_input_tokens == 0 {
                            u.cache_read_input_tokens = prior_usage.cache_read_input_tokens;
                        }
                        yield Chunk::Usage(u);
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

    /// Quirks matching a full-featured Anthropic-direct request (the strictest profile).
    fn anthropic_quirks() -> MessagesQuirks {
        MessagesQuirks {
            prompt_caching: true,
            thinking_adaptive: true,
            effort_output_config: true,
            extra_body: Default::default(),
        }
    }

    #[test]
    fn body_includes_system_and_tools() {
        let mut req = Request::new("claude-sonnet-4-6", "hi").with_system("be terse");
        req.tools.push(ToolDef {
            name: "read".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        });
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["system"], "be terse"); // short → plain string, no cache marker
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["name"], "read");
    }

    #[test]
    fn long_system_prompt_is_cache_controlled() {
        let big = "x".repeat(CACHE_MIN_CHARS + 1);
        let req = Request::new("claude-opus-4-8", "hi").with_system(big.clone());
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        assert_eq!(body["system"][0]["type"], "text");
        assert_eq!(body["system"][0]["text"], big);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn caching_off_keeps_long_system_a_plain_string() {
        let big = "x".repeat(CACHE_MIN_CHARS + 1);
        let req = Request::new("z-ai/glm-4.6", "hi").with_system(big.clone());
        let q = MessagesQuirks {
            prompt_caching: false,
            ..anthropic_quirks()
        };
        let body = build_messages_body(&req, &q).unwrap();
        assert_eq!(body["system"], big); // no cache_control array when caching is off
    }

    #[test]
    fn body_folds_system_message_and_enables_thinking() {
        let req = Request {
            messages: vec![Message::system_text("policy"), Message::user_text("go")],
            ..Request::new("m", "ignored")
                .with_thinking(true)
                .with_effort(Effort::High)
        };
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        // system message folded into the system field; only the user message remains.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
        // temperature must be omitted when thinking is on.
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn extra_body_is_merged_at_top_level() {
        let req = Request::new("z-ai/glm-4.6", "hi");
        let mut q = MessagesQuirks::default();
        q.extra_body.insert(
            "provider".into(),
            json!({ "require_parameters": true }),
        );
        let body = build_messages_body(&req, &q).unwrap();
        assert_eq!(body["provider"]["require_parameters"], true);
    }

    #[tokio::test]
    async fn parses_a_full_sse_turn() {
        // A representative Messages stream: text + a tool_use whose input arrives in JSON deltas.
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

        let mut stream = map_messages_stream(byte_stream);
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
        let usage = last_usage.unwrap();
        assert_eq!(usage.output_tokens, 15);
        // The final (message_delta) usage must preserve message_start's input_tokens, not zero it.
        assert_eq!(
            usage.input_tokens, 10,
            "input tokens from message_start must be carried into the final usage"
        );
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

    #[tokio::test]
    async fn parses_openrouter_stream_with_null_usage_and_done_sentinel() {
        // OpenRouter proxies the Messages shape but (unlike Anthropic-direct) sends `null` usage
        // counters and terminates with an OpenAI-style `[DONE]`. Both must parse cleanly.
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"moonshotai/kimi-k2\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0,\"cache_creation_input_tokens\":null,\"cache_read_input_tokens\":null}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"functions.read:0\",\"name\":\"read\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"README.md\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":28,\"cache_read_input_tokens\":null}}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut blocks = Vec::new();
        let mut stop = None;
        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            // The whole point: no chunk is an Err (a stray `[DONE]` or `null` usage must not fail).
            match chunk.expect("stream must not error on [DONE] / null usage") {
                Chunk::Block(b) => blocks.push(b),
                Chunk::Done { stop_reason } => stop = stop_reason,
                _ => {}
            }
        }
        assert_eq!(stop, Some(StopReason::ToolUse));
        match blocks.last().unwrap() {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "read");
                assert_eq!(input["path"], "README.md");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_input_tolerates_trailing_characters() {
        // Some models (e.g. deepseek-v4-flash via OpenRouter) emit a stray `}` after an otherwise
        // complete tool-input object. Parse the first value and ignore the trailing junk.
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"x\",\"name\":\"read\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"probe.txt\\\"}\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));
        let mut blocks = Vec::new();
        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Block(b) = chunk.expect("trailing junk must not fail the stream") {
                blocks.push(b);
            }
        }
        match blocks.last().unwrap() {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "read");
                assert_eq!(input["path"], "probe.txt");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_input_handles_model_json_quirks() {
        // Clean.
        assert_eq!(parse_tool_input(r#"{"path":"a.txt"}"#).unwrap()["path"], "a.txt");
        // Trailing junk (deepseek-v4-flash): the extra brace is ignored.
        assert_eq!(parse_tool_input(r#"{"path":"a.txt"}}"#).unwrap()["path"], "a.txt");
        // Truncated object (glm-5.2): the missing final brace is repaired.
        assert_eq!(
            parse_tool_input(r#"{"ast":{"body":[]}"#).unwrap()["ast"]["body"],
            json!([])
        );
        // Truncated mid-string: closed best-effort (keeps the partial value).
        assert_eq!(parse_tool_input(r#"{"path":"a.tx"#).unwrap()["path"], "a.tx");
        // Genuinely broken (a missing value, not just unbalanced) still errors.
        assert!(parse_tool_input(r#"{"path": }"#).is_err());
    }
}

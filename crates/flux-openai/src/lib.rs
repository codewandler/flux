//! `flux-openai` — OpenAI-family wire codecs and credentials.
//!
//! This crate implements the **Chat Completions** wire codec (used by the `openai` and
//! `openrouter` providers) and a single generic Bearer-token credential (`OpenAiCred`) that
//! covers API-key and OAuth-subscription transports. The **Responses** codec and the `codex`
//! ChatGPT-subscription provider land in the next increment alongside `flux-credentials`
//! (which supplies the `TokenSource` the OAuth path needs).

use std::sync::Arc;

use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use flux_core::{Chunk, ContentBlock, Error, Result, Role, StopReason, ToolResultContent, Usage};
use flux_provider::{
    ByteStream, ChunkStream, Credential, Effort, NativeProvider, Request, TokenSource, WireCodec,
};

const OPENAI_CHAT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const OLLAMA_CHAT_ENDPOINT: &str = "http://localhost:11434/v1/chat/completions";

// ---------------------------------------------------------------------------
// Chat Completions wire codec
// ---------------------------------------------------------------------------

/// The OpenAI Chat Completions wire protocol (`POST /v1/chat/completions`, SSE streaming).
/// Also the wire OpenRouter speaks.
pub struct OpenAiChat;

impl WireCodec for OpenAiChat {
    fn build_body(&self, req: &Request) -> Result<Value> {
        build_chat_body(req)
    }

    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        Box::pin(map_chat_stream(bytes))
    }
}

/// Map flux [`Effort`] to OpenAI's `reasoning_effort` (which tops out at `high`).
fn map_effort(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::Xhigh | Effort::Max => "high",
    }
}

fn tool_result_text(content: &[ToolResultContent]) -> String {
    let mut out = String::new();
    for c in content {
        if let ToolResultContent::Text { text } = c {
            out.push_str(text);
        }
    }
    out
}

/// Build the Chat Completions request body. Anthropic-style content blocks are flattened to
/// OpenAI's message model: tool results become `role:"tool"` messages, assistant tool_use
/// becomes `tool_calls`.
fn build_chat_body(req: &Request) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(sys) = &req.system {
        messages.push(json!({ "role": "system", "content": sys }));
    }

    for m in &req.messages {
        match m.role {
            Role::System => messages.push(json!({ "role": "system", "content": m.text() })),
            Role::User => {
                let mut text = String::new();
                for b in &m.content {
                    match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => messages.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": tool_result_text(content),
                        })),
                        ContentBlock::Text { text: t } => text.push_str(t),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    messages.push(json!({ "role": "user", "content": text }));
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                for b in &m.content {
                    match b {
                        ContentBlock::Text { text: t } => text.push_str(t),
                        ContentBlock::ToolUse { id, name, input } => tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": input.to_string() },
                        })),
                        _ => {}
                    }
                }
                let mut msg = json!({ "role": "assistant" });
                msg["content"] = if text.is_empty() {
                    Value::Null
                } else {
                    json!(text)
                };
                if !tool_calls.is_empty() {
                    msg["tool_calls"] = json!(tool_calls);
                }
                messages.push(msg);
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if req.max_tokens > 0 {
        body["max_tokens"] = json!(req.max_tokens);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    },
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if !req.stop_sequences.is_empty() {
        body["stop"] = json!(req.stop_sequences);
    }
    if let Some(e) = req.effort {
        body["reasoning_effort"] = json!(map_effort(e));
    }
    if !req.metadata.is_empty() {
        for (k, v) in &req.metadata {
            body[k] = v.clone();
        }
    }

    Ok(body)
}

fn map_chat_stop(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
}

// Chat SSE wire types --------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    #[serde(default)]
    delta: ChatDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FnDelta>,
}

#[derive(Debug, Deserialize)]
struct FnDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

/// Parse the OpenAI Chat Completions SSE stream into normalized [`Chunk`]s. Tool-call argument
/// fragments arrive across many deltas keyed by index; we accumulate and emit each as an
/// Upper bound on concurrent tool-call slots, so a malicious/buggy server can't drive the
/// wire-supplied `index` arbitrarily high and OOM us by growing the accumulator vector.
const MAX_TOOL_CALLS: usize = 256;

/// assembled `tool_use` block at stream end (matching the Anthropic codec's contract).
fn map_chat_stream(byte_stream: ByteStream) -> impl futures::Stream<Item = Result<Chunk>> {
    try_stream! {
        let mut events = byte_stream.eventsource();
        let mut text = String::new();
        let mut calls: Vec<(String, String, String)> = Vec::new(); // (id, name, args) by index
        let mut stop: Option<StopReason> = None;

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::Provider(format!("sse stream: {e}")))?;
            let data = event.data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let chunk: ChatChunk = serde_json::from_str(data)?;

            if let Some(u) = chunk.usage {
                yield Chunk::Usage(Usage {
                    input_tokens: u.prompt_tokens,
                    output_tokens: u.completion_tokens,
                    ..Default::default()
                });
            }

            for choice in chunk.choices {
                if let Some(c) = choice.delta.content {
                    if !c.is_empty() {
                        text.push_str(&c);
                        yield Chunk::TextDelta(c);
                    }
                }
                if let Some(r) = choice.delta.reasoning_content {
                    if !r.is_empty() {
                        yield Chunk::ThinkingDelta(r);
                    }
                }
                for tc in choice.delta.tool_calls {
                    if tc.index >= MAX_TOOL_CALLS {
                        continue; // ignore an absurd index rather than growing the vec unboundedly
                    }
                    while calls.len() <= tc.index {
                        calls.push((String::new(), String::new(), String::new()));
                    }
                    let slot = &mut calls[tc.index];
                    if let Some(id) = tc.id {
                        if !id.is_empty() {
                            slot.0 = id;
                        }
                    }
                    if let Some(f) = tc.function {
                        if let Some(n) = f.name {
                            if !n.is_empty() {
                                slot.1 = n;
                            }
                        }
                        if let Some(a) = f.arguments {
                            slot.2.push_str(&a);
                        }
                    }
                }
                if let Some(fr) = choice.finish_reason {
                    stop = Some(map_chat_stop(&fr));
                }
            }
        }

        if !text.is_empty() {
            yield Chunk::Block(ContentBlock::Text { text });
        }
        for (id, name, args) in calls {
            if name.is_empty() {
                continue;
            }
            let input = if args.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&args)?
            };
            yield Chunk::Block(ContentBlock::ToolUse { id, name, input });
        }
        yield Chunk::Done { stop_reason: stop };
    }
}

// ---------------------------------------------------------------------------
// Credential (one generic Bearer transport for the whole OpenAI family)
// ---------------------------------------------------------------------------

enum Secret {
    ApiKey(String),
    // Used by the `codex` provider once `flux-credentials` supplies a `TokenSource`.
    #[allow(dead_code)]
    OAuth(Arc<dyn TokenSource>),
}

/// A Bearer-token credential covering `openai`, `openrouter`, and (later) `codex` — they differ
/// only in endpoint, extra gating headers, and whether a `chatgpt-account-id` header is sent.
pub struct OpenAiCred {
    endpoint: String,
    secret: Secret,
    extra: Vec<(&'static str, String)>,
    send_account_id: bool,
}

#[async_trait]
impl Credential for OpenAiCred {
    fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let (token, account) = match &self.secret {
            Secret::ApiKey(k) => (k.clone(), None),
            Secret::OAuth(ts) => (ts.access_token().await?, ts.account_id()),
        };
        let mut rb = rb.header("authorization", format!("Bearer {token}"));
        for (k, v) in &self.extra {
            rb = rb.header(*k, v);
        }
        if self.send_account_id {
            if let Some(a) = account {
                rb = rb.header("chatgpt-account-id", a);
            }
        }
        Ok(rb)
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// `openai` provider via API key (Chat Completions for now; Responses planned).
pub fn openai_api(api_key: impl Into<String>) -> NativeProvider {
    NativeProvider::new(
        "openai",
        Arc::new(OpenAiChat),
        Arc::new(OpenAiCred {
            endpoint: OPENAI_CHAT_ENDPOINT.to_string(),
            secret: Secret::ApiKey(api_key.into()),
            extra: Vec::new(),
            send_account_id: false,
        }),
    )
}

/// `openai` provider from `OPENAI_API_KEY`.
pub fn openai_from_env() -> Result<NativeProvider> {
    let key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| Error::Auth("OPENAI_API_KEY is not set".to_string()))?;
    if key.trim().is_empty() {
        return Err(Error::Auth("OPENAI_API_KEY is empty".to_string()));
    }
    Ok(openai_api(key))
}

/// `openrouter` provider via API key. `referer`/`title` are OpenRouter's optional attribution
/// headers (used for app ranking); pass empty strings to omit.
pub fn openrouter_api(
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
        Arc::new(OpenAiChat),
        Arc::new(OpenAiCred {
            endpoint: OPENROUTER_ENDPOINT.to_string(),
            secret: Secret::ApiKey(api_key.into()),
            extra,
            send_account_id: false,
        }),
    )
}

/// `ollama` — a local [Ollama](https://ollama.com) server, which speaks the OpenAI Chat
/// Completions wire (so it reuses [`OpenAiChat`] verbatim). Ollama requires no credential; the
/// Bearer token is a placeholder it ignores. The endpoint defaults to `localhost:11434` and
/// honours `OLLAMA_HOST` (e.g. `http://192.168.1.10:11434` for a remote box). A scheme is added
/// if the host is bare, and a trailing `/v1/chat/completions` is appended.
pub fn ollama_api() -> NativeProvider {
    let endpoint = match std::env::var("OLLAMA_HOST") {
        Ok(h) if !h.trim().is_empty() => {
            let h = h.trim().trim_end_matches('/');
            let h = if h.contains("://") {
                h.to_string()
            } else {
                format!("http://{h}")
            };
            format!("{h}/v1/chat/completions")
        }
        _ => OLLAMA_CHAT_ENDPOINT.to_string(),
    };
    NativeProvider::new(
        "ollama",
        Arc::new(OpenAiChat),
        Arc::new(OpenAiCred {
            endpoint,
            secret: Secret::ApiKey("ollama".to_string()),
            extra: Vec::new(),
            send_account_id: false,
        }),
    )
}

/// `openrouter` provider from `OPENROUTER_API_KEY` (with a default flux attribution title).
pub fn openrouter_from_env() -> Result<NativeProvider> {
    let key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| Error::Auth("OPENROUTER_API_KEY is not set".to_string()))?;
    if key.trim().is_empty() {
        return Err(Error::Auth("OPENROUTER_API_KEY is empty".to_string()));
    }
    Ok(openrouter_api(
        key,
        "https://github.com/codewandler/flux",
        "flux",
    ))
}

// ===========================================================================
// Responses wire codec (used by `openai` later and by `codex` now)
// ===========================================================================

const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";

/// The OpenAI Responses wire protocol (`POST .../responses`, typed SSE events). `codex` toggles
/// the ChatGPT-backend quirks (no `max_output_tokens`, `store:false`, `xhigh` effort, forced
/// reasoning summary).
pub struct OpenAiResponses {
    pub codex: bool,
}

impl WireCodec for OpenAiResponses {
    fn build_body(&self, req: &Request) -> Result<Value> {
        build_responses_body(req, self.codex)
    }

    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        Box::pin(map_responses_stream(bytes))
    }
}

fn map_effort_responses(e: Effort, codex: bool) -> &'static str {
    match (e, codex) {
        (Effort::Low, _) => "low",
        (Effort::Medium, _) => "medium",
        (Effort::High, _) => "high",
        (Effort::Xhigh, true) | (Effort::Max, true) => "xhigh",
        (Effort::Xhigh, false) | (Effort::Max, false) => "high",
    }
}

/// Build the Responses request body. Content blocks map to typed `input` items; tool results
/// become `function_call_output`, assistant tool_use becomes `function_call`.
fn build_responses_body(req: &Request, codex: bool) -> Result<Value> {
    let mut instructions = req.system.clone();
    let mut input: Vec<Value> = Vec::new();

    for m in &req.messages {
        match m.role {
            Role::System => {
                let text = m.text();
                instructions = Some(match instructions {
                    Some(s) => format!("{s}\n\n{text}"),
                    None => text,
                });
            }
            Role::User => {
                let mut text = String::new();
                for b in &m.content {
                    match b {
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => input.push(json!({
                            "type": "function_call_output",
                            "call_id": tool_use_id,
                            "output": tool_result_text(content),
                        })),
                        ContentBlock::Text { text: t } => text.push_str(t),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": text }],
                    }));
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                for b in &m.content {
                    match b {
                        ContentBlock::Text { text: t } => text.push_str(t),
                        ContentBlock::ToolUse {
                            id,
                            name,
                            input: args,
                        } => input.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": args.to_string(),
                        })),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
    });
    if let Some(s) = instructions {
        body["instructions"] = json!(s);
    }
    if !req.tools.is_empty() {
        // Responses tools are flat (name/description/parameters at top level).
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }
    if let Some(e) = req.effort {
        // Codex must be told to emit a reasoning summary to stream thinking text.
        body["reasoning"] = json!({ "effort": map_effort_responses(e, codex), "summary": "auto" });
    }
    if codex {
        body["store"] = json!(false);
        // Codex rejects max_*tokens / sampling params — omit them entirely.
    } else {
        if req.max_tokens > 0 {
            body["max_output_tokens"] = json!(req.max_tokens);
        }
        if req.effort.is_none() {
            if let Some(t) = req.temperature {
                body["temperature"] = json!(t);
            }
            if let Some(p) = req.top_p {
                body["top_p"] = json!(p);
            }
        }
    }

    Ok(body)
}

/// Parse the OpenAI Responses typed-SSE event stream into normalized [`Chunk`]s. Unknown event
/// types are ignored, so the parser tolerates backend variation.
fn map_responses_stream(byte_stream: ByteStream) -> impl futures::Stream<Item = Result<Chunk>> {
    try_stream! {
        let mut events = byte_stream.eventsource();
        let mut text = String::new();
        let mut tool_blocks: Vec<ContentBlock> = Vec::new();
        let mut stop: Option<StopReason> = None;

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::Provider(format!("sse stream: {e}")))?;
            let data = event.data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let v: Value = serde_json::from_str(data)?;
            match v["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => {
                    let d = v["delta"].as_str().unwrap_or("");
                    if !d.is_empty() {
                        text.push_str(d);
                        yield Chunk::TextDelta(d.to_string());
                    }
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    let d = v["delta"].as_str().unwrap_or("");
                    if !d.is_empty() {
                        yield Chunk::ThinkingDelta(d.to_string());
                    }
                }
                "response.output_item.done" => {
                    let item = &v["item"];
                    if item["type"] == "function_call" {
                        let id = item["call_id"]
                            .as_str()
                            .or_else(|| item["id"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item["name"].as_str().unwrap_or("").to_string();
                        let args = item["arguments"].as_str().unwrap_or("");
                        let input = if args.trim().is_empty() {
                            Value::Object(Default::default())
                        } else {
                            serde_json::from_str(args)?
                        };
                        tool_blocks.push(ContentBlock::ToolUse { id, name, input });
                    }
                }
                "response.completed" => {
                    let u = &v["response"]["usage"];
                    yield Chunk::Usage(Usage {
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        ..Default::default()
                    });
                    if stop.is_none() {
                        stop = Some(StopReason::EndTurn);
                    }
                }
                "response.incomplete" => {
                    // The response was truncated (e.g. hit `max_output_tokens`) — surface it as a
                    // MaxTokens stop rather than letting `response.completed` report a clean EndTurn.
                    let reason = v["response"]["incomplete_details"]["reason"]
                        .as_str()
                        .unwrap_or("");
                    stop = Some(if reason == "max_output_tokens" {
                        StopReason::MaxTokens
                    } else {
                        StopReason::EndTurn
                    });
                }
                "response.failed" => {
                    let msg = v["response"]["error"]["message"]
                        .as_str()
                        .unwrap_or("response failed");
                    Err(Error::Provider(msg.to_string()))?;
                }
                "error" => {
                    let msg = v["message"]
                        .as_str()
                        .or_else(|| v["error"]["message"].as_str())
                        .unwrap_or("error");
                    Err(Error::Provider(msg.to_string()))?;
                }
                _ => {}
            }
        }

        if !text.is_empty() {
            yield Chunk::Block(ContentBlock::Text { text });
        }
        let had_tools = !tool_blocks.is_empty();
        for b in tool_blocks {
            yield Chunk::Block(b);
        }
        yield Chunk::Done {
            stop_reason: if had_tools { Some(StopReason::ToolUse) } else { stop },
        };
    }
}

/// `codex` provider: ChatGPT/Codex subscription via OAuth, OpenAI Responses wire on the ChatGPT
/// backend. Needs a [`TokenSource`] (from `flux-credentials`).
pub fn codex_oauth(tokens: Arc<dyn TokenSource>) -> NativeProvider {
    NativeProvider::new(
        "codex",
        Arc::new(OpenAiResponses { codex: true }),
        Arc::new(OpenAiCred {
            endpoint: CODEX_ENDPOINT.to_string(),
            secret: Secret::OAuth(tokens),
            extra: vec![
                ("OpenAI-Beta", "responses=experimental".to_string()),
                ("originator", "codex_cli_rs".to_string()),
            ],
            send_account_id: true,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::Message;

    #[test]
    fn chat_body_maps_system_tools_and_tool_results() {
        let mut req = Request::new("gpt-4o", "hi").with_system("be terse");
        req.tools.push(flux_provider::ToolDef {
            name: "read".into(),
            description: "read a file".into(),
            input_schema: json!({"type": "object"}),
        });
        // an assistant tool_use turn followed by a user tool_result turn
        req.messages
            .push(Message::assistant(vec![ContentBlock::ToolUse {
                id: "tc_1".into(),
                name: "read".into(),
                input: json!({"path": "a.txt"}),
            }]));
        req.messages
            .push(Message::user(vec![ContentBlock::tool_result_text(
                "tc_1",
                "file body",
                false,
            )]));

        let body = build_chat_body(&req).unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["stream"], true);
        // system prompt is the first message
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be terse");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        // assistant tool_use → tool_calls; user tool_result → role:"tool"
        let msgs = body["messages"].as_array().unwrap();
        let assistant = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
        assert_eq!(assistant["tool_calls"][0]["id"], "tc_1");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "read");
        let tool = msgs.iter().find(|m| m["role"] == "tool").unwrap();
        assert_eq!(tool["tool_call_id"], "tc_1");
        assert_eq!(tool["content"], "file body");
    }

    #[tokio::test]
    async fn parses_a_chat_sse_turn_with_tool_call() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.txt\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut text = String::new();
        let mut blocks = Vec::new();
        let mut stop = None;
        let mut usage = None;

        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                Chunk::TextDelta(t) => text.push_str(&t),
                Chunk::Block(b) => blocks.push(b),
                Chunk::Usage(u) => usage = Some(u),
                Chunk::Done { stop_reason } => stop = stop_reason,
                _ => {}
            }
        }

        assert_eq!(text, "Hello");
        assert_eq!(stop, Some(StopReason::ToolUse));
        assert_eq!(usage.unwrap().input_tokens, 11);
        assert_eq!(blocks.len(), 2); // text block + tool_use block
        match &blocks[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "read");
                assert_eq!(input["path"], "a.txt");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[test]
    fn responses_body_uses_instructions_and_codex_quirks() {
        let req = Request::new("gpt-5-codex", "go")
            .with_system("be terse")
            .with_effort(Effort::Max);
        let body = build_responses_body(&req, true).unwrap();
        assert_eq!(body["instructions"], "be terse");
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["store"], false); // codex
        assert_eq!(body["reasoning"]["effort"], "xhigh"); // Max → xhigh on codex
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert!(body.get("max_output_tokens").is_none()); // omitted for codex
    }

    #[tokio::test]
    async fn parses_a_responses_sse_turn_with_tool_call() {
        let sse = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":9,\"output_tokens\":4}}}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut text = String::new();
        let mut blocks = Vec::new();
        let mut stop = None;
        let mut usage = None;

        let stream = map_responses_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(c) = stream.next().await {
            match c.unwrap() {
                Chunk::TextDelta(t) => text.push_str(&t),
                Chunk::Block(b) => blocks.push(b),
                Chunk::Usage(u) => usage = Some(u),
                Chunk::Done { stop_reason } => stop = stop_reason,
                _ => {}
            }
        }

        assert_eq!(text, "Hi");
        assert_eq!(stop, Some(StopReason::ToolUse));
        assert_eq!(usage.unwrap().input_tokens, 9);
        assert_eq!(blocks.len(), 2);
        match &blocks[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "fc_1");
                assert_eq!(name, "read");
                assert_eq!(input["path"], "a.txt");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }
}

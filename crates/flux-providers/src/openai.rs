//! OpenAI-family wire codecs and credentials.
//!
//! This module implements the **Chat Completions** wire codec (used by the `openai` and
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

use crate::schema::openrouter_tools;

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

/// OpenRouter's Chat Completions codec. It shares the OpenAI wire shape while deriving a
/// Gemini-compatible view of operation schemas for `google/gemini-*` model ids.
pub struct OpenRouterChat;

impl WireCodec for OpenRouterChat {
    fn build_body(&self, req: &Request) -> Result<Value> {
        let mut projected = req.clone();
        projected.tools = openrouter_tools(&req.model, &req.tools)?;
        build_chat_body(&projected)
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

/// GPT-5-family Chat Completions requests use the replacement token-limit field. Model specs may
/// arrive as a bare id (`gpt-5-mini`) or with a routing prefix (`openai/gpt-5`), so inspect the
/// final path segment.
fn uses_max_completion_tokens(model: &str) -> bool {
    let model = model.rsplit('/').next().unwrap_or(model);
    model == "gpt-5"
        || model
            .strip_prefix("gpt-5")
            .is_some_and(|suffix| suffix.starts_with(['-', '.']))
}

/// Build the Chat Completions request body. Anthropic-style content blocks are flattened to
/// OpenAI's message model: tool results become `role:"tool"` messages, assistant tool_use
/// becomes `tool_calls`.
fn build_chat_body(req: &Request) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::new();

    // `system_text` joins segmented prompts in order (OpenAI has no cache-breakpoint notion; its
    // implicit prefix caching still benefits from the segments' stable-first layout).
    if let Some(sys) = req.system_text() {
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
        let field = if uses_max_completion_tokens(&req.model) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[field] = json!(req.max_tokens);
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
    /// `arguments` is spec'd as a JSON string but some models (e.g. GLM via OpenRouter)
    /// send it as a pre-parsed JSON object. Accept either and normalise below.
    #[serde(default)]
    arguments: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
    /// OpenRouter's reported total USD cost for this call (C-34). `serde(default)` + `Option`
    /// tolerates both an absent field (every non-OpenRouter backend) and an explicit `null`.
    #[serde(default)]
    cost: Option<f64>,
    /// Whether this call billed against the caller's own upstream key (bring-your-own-key) — flips
    /// how `cost_details.upstream_inference_cost` combines with `cost` (see
    /// [`crate::openrouter_reported_cost`]).
    #[serde(default)]
    is_byok: Option<bool>,
    #[serde(default)]
    cost_details: Option<ChatCostDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatCostDetails {
    #[serde(default)]
    upstream_inference_cost: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

/// Parse the OpenAI Chat Completions SSE stream into normalized [`Chunk`]s. Tool-call argument
/// fragments arrive across many deltas keyed by index; we accumulate and emit each as an
/// Upper bound on concurrent tool-call slots, so a malicious/buggy server can't drive the
/// wire-supplied `index` arbitrarily high and OOM us by growing the accumulator vector.
const MAX_TOOL_CALLS: usize = 256;

/// assembled `tool_use` block at stream end (matching the Anthropic codec's contract).
///
/// `pub(crate)` (not private) so the crate-wide malformed-envelope corpus (A-37,
/// `envelope_corpus.rs`) can drive it directly with corrupted fixture streams.
// A-37: this fn's one `serde_json::from_str` call is the tolerant skip+count envelope parse
// (matched, never `?`-propagated) that the crate-local `clippy.toml` disallowed-methods ban would
// otherwise flag — allowed at this tight scope; `envelope_corpus.rs` proves the tolerance holds.
#[allow(clippy::disallowed_methods)]
pub(crate) fn map_chat_stream(
    byte_stream: ByteStream,
) -> impl futures::Stream<Item = Result<Chunk>> {
    try_stream! {
        let mut events = byte_stream.eventsource();
        let mut text = String::new();
        let mut calls: Vec<(String, String, String)> = Vec::new(); // (id, name, args) by index
        let mut stop: Option<StopReason> = None;
        // A-34: an unparseable envelope frame is skipped and counted, never fatal — a real API
        // outage stays fatal via the declared-error paths below, which run on the parsed value.
        let mut dropped_frames: u32 = 0;
        let mut first_drop_detail: Option<String> = None;

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::Provider(format!("sse stream: {e}")))?;
            let data = event.data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let chunk: ChatChunk = match serde_json::from_str(data) {
                Ok(c) => c,
                Err(e) => {
                    dropped_frames += 1;
                    if first_drop_detail.is_none() {
                        first_drop_detail = Some(e.to_string());
                    }
                    tracing::warn!(
                        error = %e,
                        frame = %data.chars().take(200).collect::<String>(),
                        "openai chat SSE: skipping unparseable envelope frame"
                    );
                    continue;
                }
            };

            if let Some(u) = chunk.usage {
                // OpenAI's `prompt_tokens` is the *whole* prompt incl. the cached prefix. Normalize
                // to the cache-aware Usage shape (fresh input separate from cache reads) so cost and
                // context figures are comparable with the Anthropic codec; OpenAI has no cache-write
                // tier, so cache_creation stays 0.
                let cached = u
                    .prompt_tokens_details
                    .as_ref()
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0);
                let reported_cost_usd = crate::openrouter_reported_cost(
                    u.cost,
                    u.is_byok,
                    u.cost_details.as_ref().and_then(|d| d.upstream_inference_cost),
                );
                yield Chunk::Usage(Usage {
                    input_tokens: u.prompt_tokens.saturating_sub(cached),
                    output_tokens: u.completion_tokens,
                    cache_read_input_tokens: cached,
                    reported_cost_usd,
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
                            let fragment = match a {
                                // Normal OpenAI path: arguments arrive as a JSON string fragment.
                                serde_json::Value::String(s) => Some(s),
                                // Some models (e.g. GLM via OpenRouter) send arguments as a
                                // pre-parsed JSON object instead of a string.  Serialise it back
                                // so the existing accumulator / parse path works unchanged.
                                other if !other.is_null() => Some(other.to_string()),
                                _ => None,
                            };
                            if let Some(fragment) = fragment {
                                slot.2.push_str(&fragment);
                                // A-43: surface the fragment as a `ToolInputDelta` too (in addition
                                // to the existing accumulation this codec always did), mirroring
                                // the Messages codec's L-23 wiring, so a caller can trace or render
                                // a large native tool input progressively on this wire too — purely
                                // additive, the completed `Chunk::Block` below remains the sole
                                // source of truth for the final call. `name` carries forward from
                                // `slot.1` since only the first fragment's delta typically carries
                                // the function name.
                                yield Chunk::ToolInputDelta {
                                    name: slot.1.clone(),
                                    partial_json: fragment,
                                };
                            }
                        }
                    }
                }
                if let Some(fr) = choice.finish_reason {
                    stop = Some(map_chat_stop(&fr));
                }
            }
        }

        if dropped_frames > 0 {
            yield Chunk::StreamDiagnostic {
                dropped_frames,
                detail: format!(
                    "{dropped_frames} unparseable chat SSE frame(s) dropped; first error: {}",
                    first_drop_detail.unwrap_or_default(),
                ),
            };
        }

        // Recovery: if no native tool calls arrived but the assistant text carries inline tool-call
        // markup (some local/gateway models emit `<tool_call>…` instead of structured `tool_calls`,
        // especially on multi-call turns), parse it back into tool_use blocks so the agent loop sees
        // a real tool turn instead of stalling on what looks like prose.
        let recovered = if calls.is_empty() && has_inline_tool_markup(&text) {
            parse_inline_tool_calls(&text)
        } else {
            Vec::new()
        };

        if !recovered.is_empty() {
            let cleaned = strip_inline_tool_markup(&text);
            if !cleaned.is_empty() {
                yield Chunk::Block(ContentBlock::Text { text: cleaned });
            }
            for (id, name, input) in recovered {
                yield Chunk::Block(ContentBlock::ToolUse { id, name, input });
            }
            // A recovered turn is a tool-use turn regardless of the reported finish_reason (usually
            // "stop", since the calls were just text to the endpoint).
            yield Chunk::Done { stop_reason: Some(StopReason::ToolUse) };
        } else {
            if !text.is_empty() {
                yield Chunk::Block(ContentBlock::Text { text });
            }
            for (id, name, args) in calls {
                if name.is_empty() {
                    continue;
                }
                // Repair-then-sentinel, never a stream error (A-32): deepseek-v4-flash emits
                // malformed/truncated args on this wire too (s_368 lost a turn to a 19KB blob
                // cut mid-list), and the Messages wire's hardening never covered this path.
                let input = crate::messages::tool_input_or_sentinel(&args);
                yield Chunk::Block(ContentBlock::ToolUse { id, name, input });
            }
            yield Chunk::Done { stop_reason: stop };
        }
    }
}

// ---------------------------------------------------------------------------
// Inline tool-call recovery
//
// Some OpenAI-compatible endpoints (GLM via OpenRouter, local models via ollama) sometimes emit
// tool calls as text in the assistant `content` instead of the structured `tool_calls` field —
// most often on multi-call turns. We recover them at stream end (only when no native calls arrived,
// so well-behaved providers are untouched). NOTE: the raw markup was already streamed live as
// `TextDelta`s during accumulation; this only cleans the final transcript block, not the live view.
// ---------------------------------------------------------------------------

const INLINE_TOOL_OPEN: &str = "<tool_call>";
const INLINE_TOOL_CLOSE: &str = "</tool_call>";

/// True if `text` looks like it carries inline tool-call markup worth attempting to recover.
fn has_inline_tool_markup(text: &str) -> bool {
    text.contains(INLINE_TOOL_OPEN) || text.contains("<function=")
}

/// Inner content of each non-overlapping `open … close` span, left to right.
fn span_inners(text: &str, open: &str, close: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(s) = rest.find(open) {
        let after = &rest[s + open.len()..];
        let Some(e) = after.find(close) else { break };
        out.push(after[..e].to_string());
        rest = &after[e + close.len()..];
    }
    out
}

/// Parse one inline call body into `(name, input)`. Handles the GLM XML form
/// (`<function=NAME><parameter=KEY>VALUE</parameter>…`) and the Qwen/Hermes JSON form
/// (`{"name":…,"arguments":{…}|"…"}`). Returns `None` for anything unrecognized.
// A-37: the three `serde_json::from_str` calls in this fn are the GLM/Qwen inline-recovery
// repair — already tolerant (`.unwrap_or_else`/`?` on an `Option`, never a fatal stream error) —
// allowed at this tight scope; see clippy.toml.
#[allow(clippy::disallowed_methods)]
fn parse_inline_call_body(body: &str) -> Option<(String, Value)> {
    let t = body.trim();
    if let Some(fpos) = t.find("<function=") {
        // GLM XML form.
        let after_fn = &t[fpos + "<function=".len()..];
        let name_end = after_fn.find('>')?;
        let name = after_fn[..name_end].trim().to_string();
        if name.is_empty() {
            return None;
        }
        let mut input = serde_json::Map::new();
        for pinner in span_inners(after_fn, "<parameter=", "</parameter>") {
            // pinner = "KEY>VALUE"
            if let Some(k_end) = pinner.find('>') {
                let key = pinner[..k_end].trim().to_string();
                let raw = pinner[k_end + 1..].trim();
                if !key.is_empty() {
                    // Coerce JSON scalars (15, true, [..]) but keep bare strings (paths, prose).
                    let val = serde_json::from_str::<Value>(raw)
                        .unwrap_or_else(|_| Value::String(raw.to_string()));
                    input.insert(key, val);
                }
            }
        }
        Some((name, Value::Object(input)))
    } else {
        // Qwen/Hermes JSON form: the first {…last} object in the body.
        let start = t.find('{')?;
        let end = t.rfind('}')?;
        if start > end {
            return None;
        }
        let obj = match serde_json::from_str::<Value>(&t[start..=end]) {
            Ok(Value::Object(o)) => o,
            _ => return None,
        };
        let name = obj.get("name").and_then(|v| v.as_str())?.to_string();
        let input = match obj.get("arguments") {
            Some(Value::Object(m)) => Value::Object(m.clone()),
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({})),
            _ => json!({}),
        };
        Some((name, input))
    }
}

/// Recover tool calls a model emitted as inline text. Returns `(id, name, input)` with synthetic
/// ids, capped at [`MAX_TOOL_CALLS`].
fn parse_inline_tool_calls(text: &str) -> Vec<(String, String, Value)> {
    let bodies: Vec<String> = if text.contains(INLINE_TOOL_OPEN) {
        span_inners(text, INLINE_TOOL_OPEN, INLINE_TOOL_CLOSE)
    } else {
        // No <tool_call> wrapper: treat each <function=…></function> element as its own body.
        span_inners(text, "<function=", "</function>")
            .into_iter()
            .map(|inner| format!("<function={inner}</function>"))
            .collect()
    };
    bodies
        .into_iter()
        .filter_map(|b| parse_inline_call_body(&b))
        .take(MAX_TOOL_CALLS)
        .enumerate()
        .map(|(i, (name, input))| (format!("call_{i}"), name, input))
        .collect()
}

/// Strip inline tool-call markup, leaving the model's surrounding prose (trimmed).
fn strip_inline_tool_markup(text: &str) -> String {
    let (open, close) = if text.contains(INLINE_TOOL_OPEN) {
        (INLINE_TOOL_OPEN, INLINE_TOOL_CLOSE)
    } else {
        ("<function=", "</function>")
    };
    let mut out = String::new();
    let mut rest = text;
    while let Some(s) = rest.find(open) {
        out.push_str(&rest[..s]);
        let after = &rest[s + open.len()..];
        match after.find(close) {
            Some(e) => rest = &after[e + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

// ---------------------------------------------------------------------------
// Credential (one generic Bearer transport for the whole OpenAI family)
// ---------------------------------------------------------------------------

/// `pub(crate)` so the sibling [`crate::codex`] module can build the OAuth variant; it is an
/// internal building block of this crate's credentials, not part of the provider surface.
pub(crate) enum Secret {
    ApiKey(String),
    #[allow(dead_code)]
    OAuth(Arc<dyn TokenSource>),
}

/// A Bearer-token credential covering `openai`, `openrouter`, and `codex` — they differ only in
/// endpoint, extra gating headers, and whether a `chatgpt-account-id` header is sent. Fields are
/// `pub(crate)` so the sibling [`crate::codex`] module can assemble the codex variant without
/// re-implementing the credential; external crates build it through the provider constructors
/// (`openai_from_env`, [`crate::codex::oauth`], …).
pub struct OpenAiCred {
    pub(crate) endpoint: String,
    pub(crate) secret: Secret,
    pub(crate) extra: Vec<(&'static str, String)>,
    pub(crate) send_account_id: bool,
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
            // The ChatGPT backend rejects the request with a bare 401 if this header is missing.
            // Fail with a clear, typed error instead of letting that opaque 401 surface.
            let account = account.ok_or_else(|| {
                Error::Auth(
                    "codex: no ChatGPT account id — re-login to the Codex CLI so flux can read it \
                     from `~/.codex/auth.json` (top-level `tokens.account_id` or the `id_token` \
                     claims)"
                        .to_string(),
                )
            })?;
            rb = rb.header("chatgpt-account-id", account);
        }
        Ok(rb)
    }

    // C-04: expose the OAuth token source (codex) so the generic HTTP path can force-refresh on a
    // 401; API-key secrets have nothing to refresh.
    fn token_source(&self) -> Option<Arc<dyn TokenSource>> {
        match &self.secret {
            Secret::OAuth(ts) => Some(ts.clone()),
            Secret::ApiKey(_) => None,
        }
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

/// Resolve the OpenAI API key from the environment: `OPENAI_API_KEY`, falling back to the common
/// `OPENAI_KEY` alias when it's unset. `OPENAI_API_KEY` wins when both are set. Split out from
/// [`openai_from_env`] so the precedence itself (not just "some key was found") is directly
/// testable, the same way `bedrock::region_prefix` is split out from `resolve_model`.
fn openai_key_from_env() -> Result<String> {
    std::env::var("OPENAI_API_KEY")
        .or_else(|_| std::env::var("OPENAI_KEY"))
        .map_err(|_| Error::Auth("OPENAI_API_KEY is not set".to_string()))
}

/// `openai` provider from `OPENAI_API_KEY` (falling back to the common `OPENAI_KEY` alias when
/// `OPENAI_API_KEY` is unset; `OPENAI_API_KEY` wins when both are set).
pub fn openai_from_env() -> Result<NativeProvider> {
    let key = openai_key_from_env()?;
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
        Arc::new(OpenRouterChat),
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

/// The ChatGPT-backend Responses endpoint used by the `codex` provider. `pub(crate)` so
/// [`crate::codex`] can reference it without re-declaring the URL.
pub(crate) const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";

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
    let mut instructions = req.system_text();
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
                        // Reasoning continuity (codex / store:false): the ChatGPT backend keeps no
                        // server-side reasoning state, so echo prior assistant reasoning back into
                        // the next request's `input` to preserve the chain across a multi-turn tool
                        // loop. The encrypted/redacted payload is the load-bearing field (opted into
                        // via include:["reasoning.encrypted_content"] below); the summary is best
                        // effort. Pushed inline so the reasoning item precedes the function_call it
                        // belongs to, as the API expects.
                        ContentBlock::Thinking {
                            thinking,
                            signature,
                        } if codex => {
                            input.push(json!({
                                "type": "reasoning",
                                "summary": if thinking.is_empty() {
                                    json!([])
                                } else {
                                    json!([{ "type": "summary_text", "text": thinking }])
                                },
                                "encrypted_content": signature,
                            }));
                        }
                        ContentBlock::RedactedThinking { data } if codex => {
                            input.push(json!({
                                "type": "reasoning",
                                "summary": [],
                                "encrypted_content": data,
                            }));
                        }
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
        // With store:false there is no server-side reasoning state, so ask the backend to return
        // the encrypted reasoning content; `build_responses_body` echoes those items back across
        // turns (see the assistant `reasoning` mapping above) to keep reasoning continuity.
        body["include"] = json!(["reasoning.encrypted_content"]);
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
///
/// `pub(crate)` so the malformed-envelope corpus (A-37, `envelope_corpus.rs`) can drive it directly.
// A-37: same tolerant skip+count envelope parse as `map_chat_stream` — allowed at this tight scope.
#[allow(clippy::disallowed_methods)]
pub(crate) fn map_responses_stream(
    byte_stream: ByteStream,
) -> impl futures::Stream<Item = Result<Chunk>> {
    try_stream! {
        let mut events = byte_stream.eventsource();
        let mut text = String::new();
        let mut tool_blocks: Vec<ContentBlock> = Vec::new();
        let mut stop: Option<StopReason> = None;
        // A-34: same skip+count tolerance as the chat codec. Declared errors (`response.failed`,
        // `error`) are handled below on the parsed value and stay fatal — this only guards
        // syntactically-unparseable bytes.
        let mut dropped_frames: u32 = 0;
        let mut first_drop_detail: Option<String> = None;

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::Provider(format!("sse stream: {e}")))?;
            let data = event.data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let v: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(e) => {
                    dropped_frames += 1;
                    if first_drop_detail.is_none() {
                        first_drop_detail = Some(e.to_string());
                    }
                    tracing::warn!(
                        error = %e,
                        frame = %data.chars().take(200).collect::<String>(),
                        "openai responses SSE: skipping unparseable envelope frame"
                    );
                    continue;
                }
            };
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
                        // Same repair-then-sentinel treatment as the chat path (A-32).
                        let input = crate::messages::tool_input_or_sentinel(args);
                        tool_blocks.push(ContentBlock::ToolUse { id, name, input });
                    }
                }
                "response.completed" => {
                    let u = &v["response"]["usage"];
                    // The Responses API reports `input_tokens` as the *whole* prompt incl. the cached
                    // prefix, and surfaces the cached count under `input_tokens_details.cached_tokens`
                    // plus reasoning under `output_tokens_details.reasoning_tokens`. Normalize to the
                    // cache-aware Usage shape (fresh input separate from cache reads, reasoning as a
                    // subset of output) so cost is comparable across providers; there is no cache-write
                    // tier, so cache_creation stays 0.
                    let input = u["input_tokens"].as_u64().unwrap_or(0);
                    let cached = u["input_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    let reasoning = u["output_tokens_details"]["reasoning_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    yield Chunk::Usage(Usage {
                        input_tokens: input.saturating_sub(cached),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        cache_read_input_tokens: cached,
                        reasoning_tokens: reasoning,
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

        if dropped_frames > 0 {
            yield Chunk::StreamDiagnostic {
                dropped_frames,
                detail: format!(
                    "{dropped_frames} unparseable responses SSE frame(s) dropped; first error: {}",
                    first_drop_detail.unwrap_or_default(),
                ),
            };
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

/// `codex` provider construction now lives in [`crate::codex`] (its own provider module, like
/// `anthropic`/`openrouter`/`ollama`). The Responses wire codec (`OpenAiResponses`) and
/// `build_responses_body` stay here because the `openai` and `codex` providers share them.
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

    #[test]
    fn openrouter_chat_projects_gemini_schemas_without_affecting_openai_chat() {
        let mut req = Request::new("google/gemini-3.5-flash", "hi");
        req.tools.push(flux_provider::ToolDef {
            name: "records.merge".into(),
            description: "merge records".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"records": {"type": "array"}},
                "required": ["records", "label"]
            }),
        });
        let original = req.clone();

        let gemini = OpenRouterChat.build_body(&req).unwrap();
        let openai = OpenAiChat.build_body(&req).unwrap();

        assert_eq!(
            gemini["tools"][0]["function"]["parameters"]["properties"]["records"]["items"],
            json!({})
        );
        assert_eq!(
            gemini["tools"][0]["function"]["parameters"]["properties"]["label"],
            json!({})
        );
        assert!(
            openai["tools"][0]["function"]["parameters"]["properties"]["records"]
                .get("items")
                .is_none()
        );
        assert!(openai["tools"][0]["function"]["parameters"]["properties"]
            .get("label")
            .is_none());
        assert_eq!(req.tools, original.tools);
    }

    /// OpenAI's GPT-5 Chat Completions models reject the legacy `max_tokens` field and require
    /// `max_completion_tokens`. Keep the legacy field for models such as GPT-4o that still speak
    /// the older request shape.
    #[test]
    fn chat_body_uses_gpt5_completion_token_field() {
        let gpt5 = build_chat_body(&Request::new("gpt-5", "hi").with_max_tokens(123)).unwrap();
        assert_eq!(gpt5["max_completion_tokens"], 123);
        assert!(
            gpt5.get("max_tokens").is_none(),
            "GPT-5 rejects the legacy max_tokens field: {gpt5}"
        );

        let gpt4o = build_chat_body(&Request::new("gpt-4o", "hi").with_max_tokens(123)).unwrap();
        assert_eq!(gpt4o["max_tokens"], 123);
        assert!(gpt4o.get("max_completion_tokens").is_none());
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

    /// A-43: mirrors the Messages codec's L-23 `ToolInputDelta` surfacing — as `arguments`
    /// fragments arrive across multiple chat-completions deltas, the codec must yield a
    /// `Chunk::ToolInputDelta` for each fragment (in order, ahead of the completed tool_use
    /// block) so a plan skeleton can render progressively on this wire too. Only the first
    /// fragment's delta carries the function `name`; later fragments must carry the name forward
    /// from that call's index, exactly as the OpenAI wire actually streams it.
    #[tokio::test]
    async fn chat_tool_call_args_stream_as_tool_input_delta() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\".txt\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        let chunks: Vec<Chunk> = stream.map(|c| c.unwrap()).collect().await;

        let deltas: Vec<&Chunk> = chunks
            .iter()
            .filter(|c| matches!(c, Chunk::ToolInputDelta { .. }))
            .collect();
        assert_eq!(
            deltas.len(),
            3,
            "expected one delta per arguments fragment, got {chunks:?}"
        );
        let fragments = ["{\"path\":", "\"a", ".txt\"}"];
        for (d, expected_fragment) in deltas.iter().zip(fragments) {
            match d {
                Chunk::ToolInputDelta { name, partial_json } => {
                    assert_eq!(name, "read", "name must carry forward to every fragment");
                    assert_eq!(partial_json, expected_fragment);
                }
                other => panic!("expected ToolInputDelta, got {other:?}"),
            }
        }

        // All deltas must precede the completed tool_use block — a consumer streaming the
        // skeleton must never see it after the call is already known complete.
        let delta_pos = chunks
            .iter()
            .position(|c| matches!(c, Chunk::ToolInputDelta { .. }))
            .unwrap();
        let block_pos = chunks
            .iter()
            .position(|c| matches!(c, Chunk::Block(ContentBlock::ToolUse { .. })))
            .unwrap();
        assert!(
            delta_pos < block_pos,
            "deltas must stream ahead of the completed block"
        );

        // The final completed block is unaffected — the accumulation this codec always did.
        match &chunks[block_pos] {
            Chunk::Block(ContentBlock::ToolUse { id, name, input }) => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "read");
                assert_eq!(input["path"], "a.txt");
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_usage_captures_cached_tokens() {
        // prompt_tokens is the whole prompt (incl. the cached prefix); cached_tokens is the cached
        // portion. The codec must split fresh input from cache reads.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":50,\
             \"prompt_tokens_details\":{\"cached_tokens\":800}}}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut usage = None;
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Usage(u) = chunk.unwrap() {
                usage = Some(u);
            }
        }

        let u = usage.expect("usage chunk");
        assert_eq!(u.input_tokens, 200); // 1000 prompt - 800 cached
        assert_eq!(u.cache_read_input_tokens, 800);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_creation_input_tokens, 0);
        assert_eq!(u.context_tokens(), 1000);
    }

    /// C-34: the final chat-completions SSE usage frame carries OpenRouter's `cost` field — the
    /// live probe (2026-07-04, deepseek-v4-flash:nitro) shape: `cost`, `is_byok:false`, and
    /// `cost_details.upstream_inference_cost` echoing the SAME figure as `cost` for a non-BYOK
    /// response. The codec must take `cost` as-is here, NOT `cost + upstream_inference_cost` — that
    /// would double-count, since for non-BYOK the two fields are the same number.
    #[tokio::test]
    async fn chat_usage_captures_openrouter_reported_cost() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":50,\
             \"cost\":0.0000020643,\"is_byok\":false,\
             \"cost_details\":{\"upstream_inference_cost\":0.0000020643}}}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut usage = None;
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Usage(u) = chunk.unwrap() {
                usage = Some(u);
            }
        }

        let u = usage.expect("usage chunk");
        // Token normalization is unaffected by the new fields.
        assert_eq!(u.input_tokens, 1000);
        assert_eq!(u.output_tokens, 50);
        assert!(
            (u.reported_cost_usd.expect("reported cost must be captured") - 0.0000020643).abs()
                < 1e-12,
            "must equal `cost` exactly — summing upstream_inference_cost would double-count for \
             non-BYOK, got {:?}",
            u.reported_cost_usd
        );

        // A provider that doesn't report cost (`"cost":null`, or the field simply absent) yields
        // `None` — table pricing stays the fallback.
        let sse_no_cost = concat!(
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\
             \"cost\":null}}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream = Box::pin(futures::stream::once(async move {
            Ok(bytes::Bytes::from(sse_no_cost))
        }));
        let mut usage = None;
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Usage(u) = chunk.unwrap() {
                usage = Some(u);
            }
        }
        assert_eq!(usage.expect("usage chunk").reported_cost_usd, None);
    }

    /// A-32 (s_368): deepseek-v4-flash:nitro stopped emitting mid-args — a large native tool input
    /// cut off inside a list ("EOF while parsing a list at line 1 column 19189"). The codec
    /// must balance-close the truncation and yield a usable tool_use block instead of failing the
    /// stream, which killed the whole turn.
    #[tokio::test]
    async fn chat_tool_args_truncated_mid_list_are_repaired() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\",\\\"lines\\\":[1,2\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut blocks = Vec::new();
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Block(b) = chunk.unwrap() {
                blocks.push(b);
            }
        }

        match &blocks[0] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "write");
                assert_eq!(input["lines"], json!([1, 2]));
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    /// A-32: the trailing-junk shape (an extra `}` after a complete value — the malformation
    /// `parse_tool_input`'s doc attributes to deepseek-v4-flash) must be repaired on the
    /// chat-completions wire too, not just the Messages wire.
    #[tokio::test]
    async fn chat_tool_args_with_trailing_junk_are_repaired() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut blocks = Vec::new();
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Block(b) = chunk.unwrap() {
                blocks.push(b);
            }
        }

        match &blocks[0] {
            ContentBlock::ToolUse { input, .. } => assert_eq!(input["path"], "a.txt"),
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    /// A-32: args that even repair cannot fix (balanced but syntactically broken) must yield a
    /// tool_use block carrying the parse-error sentinel — the stream completes and the consumer
    /// rejects the call with repairable feedback instead of the turn dying.
    #[tokio::test]
    async fn chat_tool_args_unrepairable_yield_parse_error_sentinel() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"a\\\" \\\"b\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut blocks = Vec::new();
        let mut done = false;
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            match chunk.expect("the stream must survive unparseable args") {
                Chunk::Block(b) => blocks.push(b),
                Chunk::Done { .. } => done = true,
                _ => {}
            }
        }

        assert!(done, "the stream completes");
        match &blocks[0] {
            ContentBlock::ToolUse { input, .. } => {
                assert!(
                    input[flux_core::ARGS_PARSE_ERROR_KEY].is_string(),
                    "sentinel carries the serde message: {input}"
                );
                assert!(
                    input[flux_core::ARGS_RAW_PREFIX_KEY]
                        .as_str()
                        .unwrap()
                        .contains("{\"a\""),
                    "sentinel carries the raw prefix: {input}"
                );
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    /// A-32: the Responses-API path repairs truncated args through the same helper.
    #[tokio::test]
    async fn responses_tool_args_truncated_are_repaired() {
        let sse = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.tx\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":9,\"output_tokens\":4}}}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut blocks = Vec::new();
        let stream = map_responses_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(c) = stream.next().await {
            if let Chunk::Block(b) = c.unwrap() {
                blocks.push(b);
            }
        }

        match &blocks[0] {
            ContentBlock::ToolUse { input, .. } => assert_eq!(input["path"], "a.tx"),
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[test]
    fn parse_inline_recovers_glm_and_qwen_forms() {
        // GLM XML form, two calls with surrounding prose.
        let glm = "Let me read.<tool_call><function=read> <parameter=path>README.md</parameter>\
                   </function></tool_call><tool_call> <function=read> <parameter=path>next.md\
                   </parameter></function></tool_call>";
        let calls = parse_inline_tool_calls(glm);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1, "read");
        assert_eq!(calls[0].2["path"], "README.md");
        assert_eq!(calls[1].2["path"], "next.md");
        assert_eq!(strip_inline_tool_markup(glm), "Let me read.");

        // Qwen/Hermes JSON form.
        let qwen = "<tool_call>{\"name\":\"write\",\"arguments\":{\"path\":\"tool.txt\",\"content\":\"X\"}}</tool_call>";
        let calls = parse_inline_tool_calls(qwen);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "write");
        assert_eq!(calls[0].2["content"], "X");

        // A GLM numeric parameter is coerced to a JSON number; a path stays a string.
        let numeric =
            "<tool_call><function=git_log> <parameter=limit>15</parameter></function></tool_call>";
        let calls = parse_inline_tool_calls(numeric);
        assert_eq!(calls[0].2["limit"], 15);
    }

    #[tokio::test]
    async fn chat_stream_recovers_inline_tool_calls() {
        // A model that emitted its tool call as text (no structured tool_calls) and finished with
        // "stop" — the recovery path must still surface a tool_use block and a ToolUse stop reason.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Let me read.<tool_call><function=read> <parameter=path>a.txt</parameter></function></tool_call>\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut blocks = Vec::new();
        let mut stop = None;
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                Chunk::Block(b) => blocks.push(b),
                Chunk::Done { stop_reason } => stop = stop_reason,
                _ => {}
            }
        }

        assert_eq!(stop, Some(StopReason::ToolUse));
        assert_eq!(blocks.len(), 2); // cleaned prose + recovered tool_use
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Let me read."),
            other => panic!("expected cleaned text, got {other:?}"),
        }
        match &blocks[1] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "read");
                assert_eq!(input["path"], "a.txt");
            }
            other => panic!("expected recovered tool_use, got {other:?}"),
        }
    }

    #[test]
    fn responses_body_uses_instructions_and_codex_quirks() {
        let req = Request::new("gpt-5.5", "go")
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

    #[test]
    fn codex_body_echoes_encrypted_reasoning() {
        // A prior assistant turn carried a reasoning block + a tool call. Under codex/store:false
        // the body must (a) opt into encrypted reasoning content and (b) echo the reasoning item
        // back into `input` so the multi-turn tool loop keeps its reasoning context.
        let mut req = Request::new("gpt-5.5", "go").with_effort(Effort::Max);
        req.messages.push(Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "let me think".into(),
                signature: "ENC_ABC".into(),
            },
            ContentBlock::ToolUse {
                id: "fc_1".into(),
                name: "read".into(),
                input: json!({ "path": "a.txt" }),
            },
        ]));
        // A redacted reasoning block round-trips too.
        req.messages
            .push(Message::assistant(vec![ContentBlock::RedactedThinking {
                data: "ENC_REDACTED".into(),
            }]));

        let body = build_responses_body(&req, true).unwrap();

        // (a) include opts into the encrypted reasoning content.
        let include = body["include"].as_array().expect("include array");
        assert!(include.iter().any(|v| v == "reasoning.encrypted_content"));

        // (b) a reasoning item carrying the encrypted payload round-trips into input.
        let input = body["input"].as_array().expect("input array");
        let reasoning: Vec<&Value> = input.iter().filter(|i| i["type"] == "reasoning").collect();
        assert_eq!(reasoning.len(), 2, "both reasoning blocks should be echoed");
        assert_eq!(reasoning[0]["encrypted_content"], "ENC_ABC");
        assert_eq!(reasoning[0]["summary"][0]["text"], "let me think");
        assert_eq!(reasoning[1]["encrypted_content"], "ENC_REDACTED");

        // The reasoning item precedes the function_call it belongs to.
        let r_idx = input.iter().position(|i| i["type"] == "reasoning").unwrap();
        let fc_idx = input
            .iter()
            .position(|i| i["type"] == "function_call")
            .unwrap();
        assert!(r_idx < fc_idx, "reasoning must precede its function_call");

        // Non-codex Responses bodies neither include encrypted reasoning nor echo reasoning items.
        let plain = build_responses_body(&req, false).unwrap();
        assert!(plain.get("include").is_none());
        assert!(plain["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["type"] != "reasoning"));
    }

    #[tokio::test]
    async fn codex_requires_account_id() {
        // A codex credential (send_account_id:true) whose token source resolves no account id must
        // fail with a clear typed error on `apply`, not silently send a header-less request that the
        // backend rejects with a bare 401.
        struct NoAccount;
        #[async_trait]
        impl TokenSource for NoAccount {
            async fn access_token(&self) -> Result<String> {
                Ok("tok".to_string())
            }
            // account_id() defaults to None.
        }

        let cred = OpenAiCred {
            endpoint: CODEX_ENDPOINT.to_string(),
            secret: Secret::OAuth(Arc::new(NoAccount)),
            extra: Vec::new(),
            send_account_id: true,
        };
        let rb = reqwest::Client::new().post(cred.endpoint());
        let err = cred
            .apply(rb)
            .await
            .expect_err("missing account id must error");
        assert!(err.to_string().contains("account id"), "got: {err}");
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

    /// A-34: an SSE `data:` frame that isn't valid JSON at all must be skipped, not fatal — the
    /// stream completes without ever yielding an `Err`.
    #[tokio::test]
    async fn chat_malformed_envelope_frame_is_skipped_and_the_stream_survives() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: not json at all {{{\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            assert!(chunk.is_ok(), "a junk frame must not fail the stream");
        }
    }

    /// A-34: content that arrives in frames *after* a junk frame must still reach the collected
    /// output — proves skip+continue, not truncate-on-first-bad-frame.
    #[tokio::test]
    async fn chat_content_after_a_junk_frame_still_arrives() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: not json at all {{{\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut text = String::new();
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::TextDelta(t) = chunk.expect("the stream must survive the junk frame") {
                text.push_str(&t);
            }
        }

        assert_eq!(
            text, "Hello",
            "the tail after the junk frame must not be truncated"
        );
    }

    /// A-34: dropped frames must surface exactly one end-of-stream `StreamDiagnostic` carrying the
    /// count of frames that were skipped.
    #[tokio::test]
    async fn chat_dropped_frames_surface_a_stream_diagnostic() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: not json at all {{{\n\n",
            "data: also not json ]][\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut diagnostics = Vec::new();
        let stream = map_chat_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::StreamDiagnostic {
                dropped_frames,
                detail,
            } = chunk.expect("the stream must survive the junk frames")
            {
                diagnostics.push((dropped_frames, detail));
            }
        }

        assert_eq!(diagnostics.len(), 1, "exactly one diagnostic chunk");
        assert_eq!(diagnostics[0].0, 2, "both bad frames counted");
        assert!(!diagnostics[0].1.is_empty(), "detail carries a summary");
    }

    /// A-34: the Responses-API envelope parse must tolerate the same class of junk frame.
    #[tokio::test]
    async fn responses_malformed_envelope_frame_is_skipped() {
        let sse = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
            "data: not json at all {{{\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":9,\"output_tokens\":4}}}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut text = String::new();
        let stream = map_responses_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::TextDelta(t) = chunk.expect("a junk frame must not fail the stream") {
                text.push_str(&t);
            }
        }

        assert_eq!(text, "Hi");
    }

    /// A-34 guardrail pin: a well-formed `response.failed` event is a *declared* provider error,
    /// not unparseable bytes — it must stay fatal exactly as before this story's change. This test
    /// passes unchanged both before and after the tolerance change; it only pins the behavior.
    #[tokio::test]
    async fn responses_declared_error_events_stay_fatal() {
        let sse =
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n";
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut saw_err = false;
        let stream = map_responses_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            if let Err(e) = chunk {
                saw_err = true;
                assert!(e.to_string().contains("boom"), "got: {e}");
            }
        }

        assert!(saw_err, "a declared response.failed event must stay fatal");
    }

    #[tokio::test]
    async fn responses_usage_captures_cache_and_reasoning() {
        // input_tokens is the whole prompt (incl. cached); cached + reasoning come from the *_details.
        let sse = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\
             \"input_tokens\":1000,\"output_tokens\":300,\
             \"input_tokens_details\":{\"cached_tokens\":600},\
             \"output_tokens_details\":{\"reasoning_tokens\":120}}}}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut usage = None;
        let stream = map_responses_stream(byte_stream);
        futures::pin_mut!(stream);
        while let Some(c) = stream.next().await {
            if let Chunk::Usage(u) = c.unwrap() {
                usage = Some(u);
            }
        }

        let u = usage.expect("usage chunk");
        assert_eq!(u.input_tokens, 400); // 1000 input - 600 cached
        assert_eq!(u.cache_read_input_tokens, 600);
        assert_eq!(u.output_tokens, 300);
        assert_eq!(u.reasoning_tokens, 120); // subset of output_tokens
        assert_eq!(u.cache_creation_input_tokens, 0);
        assert_eq!(u.context_tokens(), 1000);
    }

    // -- openai_from_env / OPENAI_KEY alias (D-60) ----------------------------------------------
    //
    // Serializes the tests that mutate process-global `OPENAI_API_KEY`/`OPENAI_KEY` env — without
    // it they race across test threads (mirrors `bedrock`'s `ENV_LOCK`/`env_guard`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn openai_key_from_env_falls_back_to_openai_key_alias() {
        let _env = env_guard();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::set_var("OPENAI_KEY", "alias-key");
        }
        assert_eq!(openai_key_from_env().unwrap(), "alias-key");
        unsafe {
            std::env::remove_var("OPENAI_KEY");
        }
    }

    #[test]
    fn openai_key_from_env_prefers_openai_api_key_when_both_are_set() {
        let _env = env_guard();
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "primary-key");
            std::env::set_var("OPENAI_KEY", "alias-key");
        }
        assert_eq!(openai_key_from_env().unwrap(), "primary-key");
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("OPENAI_KEY");
        }
    }

    #[test]
    fn openai_key_from_env_errors_when_neither_is_set() {
        let _env = env_guard();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("OPENAI_KEY");
        }
        assert!(openai_key_from_env().is_err());
    }

    #[test]
    fn openai_from_env_succeeds_end_to_end_from_the_openai_key_alias() {
        let _env = env_guard();
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::set_var("OPENAI_KEY", "alias-key");
        }
        assert!(
            openai_from_env().is_ok(),
            "OPENAI_KEY alone must suffice for openai_from_env"
        );
        unsafe {
            std::env::remove_var("OPENAI_KEY");
        }
    }
}

#[cfg(test)]
mod cache_tail_tests {
    use super::*;
    use flux_provider::Request;

    /// C-134: `cache_tail` is an Anthropic-wire concern. The Responses path shares `Request`, so it
    /// must ignore the flag entirely — same bytes with and without it.
    #[test]
    fn responses_body_ignores_the_anthropic_cache_tail_flag() {
        let mut off = Request::new("gpt-5.6-sol", "hi");
        off.messages = vec![flux_core::Message::user_text("hello")];
        let mut on = off.clone();
        on.cache_tail = true;

        for codex in [false, true] {
            let a = build_responses_body(&off, codex).unwrap();
            let b = build_responses_body(&on, codex).unwrap();
            assert_eq!(
                a, b,
                "cache_tail must not reach the Responses wire (codex={codex})"
            );
            assert!(!a.to_string().contains("cache_control"), "{a}");
        }
    }
}

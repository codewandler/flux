//! The shared **Anthropic Messages** protocol core.
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
//!
//! # Cache layout (the invariant)
//!
//! Anthropic renders `tools` → `system` → `messages` and matches on an exact prefix, resuming only
//! at a `cache_control` breakpoint. flux lays that out as:
//!
//! * **the stable prefix** — `tools` plus the `cache: true` system segments — carries breakpoints on
//!   a **1-hour** TTL. It is byte-stable across the turns of a session, and interactive pauses
//!   routinely outlive the 5-minute default (C-135).
//! * **per-turn material** — the `cache: false` segment — rides *after* the last system breakpoint,
//!   so changing it cannot invalidate the cached prefix (A-03).
//! * **the conversation tail** — the last content block of the last message — carries the rolling
//!   breakpoint on the **5-minute** default, because it moves every round (C-134).
//! * **the union stays ≤ [`MAX_CACHE_BREAKPOINTS`]**. The tail claims its slot first; the system
//!   side is then trimmed largest-first by [`cache_breakpoints`]. A fifth breakpoint is an HTTP 400,
//!   not a degradation (A-23).
//!
//! `cache_layout_contract` in this module's tests pins all four. If you add a `cache: true` segment
//! or change the tool set mid-turn, read `docs/designs/llm-cache-review.md` first — both cost cache,
//! and tools changing invalidates the system breakpoints too because tools render before them.

use std::collections::HashMap;

use async_stream::try_stream;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::{json, Value};

use flux_core::{Chunk, ContentBlock, Error, Result, Role};
use flux_provider::{ByteStream, ChunkStream};

mod quirks;
mod wire;

pub use quirks::{anthropic_model_caps, AnthropicModelCaps, MessagesQuirks, ProviderProfile};
use wire::{StreamEvent, WireBlock, WireDelta};

// Convenience re-exports so the sibling provider modules get the request shape they build against
// straight from `crate::messages`.
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

    // C-134: the conversation tail claims a breakpoint before the system segments are laid out, so
    // the union stays within Anthropic's hard maximum of four. It is claimed first because it is
    // worth more than the smallest system segment: the transcript grows every round while the
    // system prefix does not.
    let tail_cached = q.prompt_caching && req.cache_tail && stamp_tail_breakpoint(&mut messages);
    body["messages"] = Value::Array(messages);
    let reserved = usize::from(tail_cached);

    if !req.system_segments.is_empty() {
        body["system"] = segmented_system_field(
            &req.system_segments,
            system.as_deref(),
            q.prompt_caching,
            reserved,
        );
    } else if let Some(s) = system {
        body["system"] = system_field(&s, q.prompt_caching);
    }
    if !req.tools.is_empty() {
        body["tools"] = serde_json::to_value(&req.tools)?;
    }
    if req.thinking && q.thinking_adaptive {
        // 4.6-family+ models accept only adaptive thinking; the older
        // `{type:"enabled",budget_tokens}` shape now 400s. Temperature is rejected alongside it.
        // Models that predate adaptive thinking (`thinking_adaptive: false` — e.g. Haiku 4.5) get
        // NO `thinking` field at all: sending the adaptive shape is a hard 400 there (C-49).
        body["thinking"] = json!({ "type": "adaptive" });
    } else if let Some(t) = req.temperature {
        if q.sampling_params {
            body["temperature"] = json!(t);
        }
    }
    if q.effort_output_config {
        if let Some(effort) = req.effort {
            body["output_config"] = json!({ "effort": effort.as_str() });
        }
    }
    if let Some(p) = req.top_p {
        // Like temperature: rejected outright by the newest Anthropic generations (C-49).
        if q.sampling_params {
            body["top_p"] = json!(p);
        }
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

/// Anthropic accepts at most **4** `cache_control` breakpoints per request; a 5th → HTTP 400. The
/// subscription-claude planner layout already stamps exactly 4 cache:true segments (transport prefix
/// + planner-A + phase + base-B), so any future cache:true segment would tip it over (A-23).
///
/// C-134 made this a **union** budget: the conversation-tail breakpoint competes with the system
/// segments for the same four slots, so [`cache_breakpoints`] is told how many are already spoken
/// for and trims the system side to fit.
const MAX_CACHE_BREAKPOINTS: usize = 4;

/// How far Anthropic walks back from a breakpoint (in content blocks) looking for an existing cache
/// entry. A round that appends more blocks than this leaves the previous round's tail breakpoint out
/// of range, and the tail is re-written instead of read.
///
/// Not worked around, deliberately: with the budget above already full on subscription-claude there
/// is no slot for intermediate breakpoints, and a miss costs one round's tail (the next round
/// re-establishes it). It is made *observable* rather than silent — the model trace reports the
/// per-request block count, and [`tail_breakpoint_out_of_lookback`] pins the shape.
const CACHE_LOOKBACK_BLOCKS: usize = 20;

/// The cache-control value for the **stable** tools+system prefix (C-135).
///
/// A one-hour TTL rather than the five-minute default: this prefix is byte-stable across the turns
/// of a session, and interactive use — a human reading output between turns — routinely outlives
/// five minutes, cold-starting the whole prefix on the next turn. The 1h write premium (2x base vs
/// 1.25x) pays back in three requests, which a single multi-round turn already clears.
fn stable_cache_control() -> Value {
    json!({ "type": "ephemeral", "ttl": "1h" })
}

/// The cache-control value for the **rolling** conversation tail (C-134/C-135).
///
/// Deliberately the five-minute default: the tail moves every round, so a 1h write premium would
/// buy retention the entry never lives to use.
fn rolling_cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

fn system_field(s: &str, caching: bool) -> Value {
    if caching && s.len() >= CACHE_MIN_CHARS {
        json!([{ "type": "text", "text": s, "cache_control": stable_cache_control() }])
    } else {
        json!(s)
    }
}

/// Stamp the rolling breakpoint on the last content block of the last message. Returns whether one
/// was placed — a conversation with no messages, or whose final message has no content blocks, has
/// nothing to cache and must not consume a slot.
fn stamp_tail_breakpoint(messages: &mut [Value]) -> bool {
    let Some(last) = messages.last_mut() else {
        return false;
    };
    // A message whose content serialized to a plain string carries no block to stamp.
    let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) else {
        return false;
    };
    // The final message alone exceeding the window means the previous round's tail entry is
    // certainly out of Anthropic's backward search range, so this tail will be written rather than
    // read. Nothing to do about it inside a full breakpoint budget — but say so, because the whole
    // point of C-134 is that a cache miss should never be silent.
    if blocks.len() > CACHE_LOOKBACK_BLOCKS {
        tracing::debug!(
            blocks = blocks.len(),
            window = CACHE_LOOKBACK_BLOCKS,
            "conversation tail exceeds the prompt-cache lookback window; this round's tail is a \
             cache write, not a read"
        );
    }
    let Some(block) = blocks.last_mut() else {
        return false;
    };
    let Some(object) = block.as_object_mut() else {
        return false;
    };
    object.insert("cache_control".to_string(), rolling_cache_control());
    true
}

/// A segmented system prompt (A-03 cache-first layout): with caching, one text block per segment,
/// `cache_control: ephemeral` on the segments marked as breakpoints — so a change in a later
/// (dynamic) segment can't invalidate the cached prefix. `folded` (system-role conversation
/// messages) lands as a trailing uncached block. Without caching, everything joins to the plain
/// string form, order preserved.
fn segmented_system_field(
    segments: &[flux_provider::SystemSegment],
    folded: Option<&str>,
    caching: bool,
    reserved: usize,
) -> Value {
    if caching {
        let keep = cache_breakpoints(segments, reserved);
        let mut blocks: Vec<Value> = segments
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let mut b = json!({ "type": "text", "text": seg.text });
                if keep.contains(&i) {
                    b["cache_control"] = stable_cache_control();
                }
                b
            })
            .collect();
        if let Some(s) = folded {
            blocks.push(json!({ "type": "text", "text": s }));
        }
        Value::Array(blocks)
    } else {
        let mut parts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        if let Some(s) = folded {
            parts.push(s);
        }
        json!(parts.join("\n\n"))
    }
}

/// Which segment indices should actually carry a `cache_control` breakpoint. Every cache:true
/// segment is stamped while the total stays within Anthropic's [`MAX_CACHE_BREAKPOINTS`] ceiling; if
/// more segments than that ask to be cached, only the largest are stamped and the smaller ones drop
/// their breakpoint (A-23).
///
/// `reserved` is how many of the four slots are already claimed outside the system array — today
/// the conversation-tail breakpoint (C-134), which takes its slot first. Subscription-claude's
/// intent layout stamps exactly four cache:true segments, so on that path the tail's slot always
/// costs the smallest system segment its breakpoint; the dropped segment's bytes still ride inside
/// the next cached segment's prefix, so the cached prefix is unchanged in extent — only the number
/// of resume points shrinks. Keeping the biggest cache:true segments preserves the stable
/// planner prefix (the bulk of the prompt) — so the cache hit isn't regressed — while a dropped
/// small segment's bytes still ride inside a later segment's cached prefix. This makes the ≤4
/// invariant hold no matter how many cache:true segments a future layout adds.
fn cache_breakpoints(segments: &[flux_provider::SystemSegment], reserved: usize) -> Vec<usize> {
    let budget = MAX_CACHE_BREAKPOINTS.saturating_sub(reserved);
    let cached: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.cache)
        .map(|(i, _)| i)
        .collect();
    if cached.len() <= budget {
        return cached;
    }
    // Too many breakpoints: keep the largest `MAX` cache:true segments. Ties break toward the earlier
    // segment, keeping the choice stable and prefix-friendly.
    let mut by_size = cached;
    by_size.sort_by(|&a, &b| {
        segments[b]
            .text
            .len()
            .cmp(&segments[a].text.len())
            .then(a.cmp(&b))
    });
    by_size.truncate(budget);
    by_size.sort_unstable();
    by_size
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
///
/// Reads the first JSON value (ignoring anything after it); if that fails, balances the unclosed
/// brackets/strings once and retries.
pub(crate) fn parse_tool_input(json: &str) -> std::result::Result<Value, serde_json::Error> {
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

/// Parse tool-call arguments with [`parse_tool_input`]'s repairs; when even repair fails, return
/// a parse-error *sentinel* object instead of an error (A-32). Failing the stream over
/// unparseable args cost s_368 a turn after seven accepted planning rounds — carried as data, the
/// failure reaches the planner/tool layer, which rejects the call with feedback the model
/// retries on.
pub(crate) fn tool_input_or_sentinel(json: &str) -> Value {
    if json.trim().is_empty() {
        return Value::Object(Default::default());
    }
    match parse_tool_input(json) {
        Ok(v) => v,
        Err(e) => serde_json::json!({
            (flux_core::ARGS_PARSE_ERROR_KEY): e.to_string(),
            (flux_core::ARGS_RAW_PREFIX_KEY): json.chars().take(200).collect::<String>(),
        }),
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

    fn finish(self) -> ContentBlock {
        match self {
            BlockAcc::Text(text) => ContentBlock::Text { text },
            BlockAcc::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking,
                signature,
            },
            BlockAcc::RedactedThinking(data) => ContentBlock::RedactedThinking { data },
            // Unparseable-even-after-repair input becomes a sentinel, never a stream error
            // (A-32): the consumer rejects the call with repairable feedback instead of the
            // whole turn dying on a codec failure.
            BlockAcc::ToolUse { id, name, json } => ContentBlock::ToolUse {
                id,
                name,
                input: tool_input_or_sentinel(&json),
            },
        }
    }
}

/// Map a raw Messages SSE byte stream into normalized [`Chunk`]s (boxed for `WireCodec::map_stream`).
pub fn map_messages_stream(byte_stream: ByteStream) -> ChunkStream {
    Box::pin(map_messages_stream_inner(byte_stream))
}

// A-37: this fn's one `serde_json::from_str` call is the tolerant skip+count/unrecognized-variant
// envelope parse (matched, never `?`-propagated) — allowed at this tight scope; the crate-local
// `clippy.toml` disallowed-methods ban would otherwise flag it. `envelope_corpus.rs`'s corpus test
// drives the public `map_messages_stream` wrapper to prove the tolerance holds.
#[allow(clippy::disallowed_methods)]
fn map_messages_stream_inner(
    byte_stream: ByteStream,
) -> impl futures::Stream<Item = Result<Chunk>> {
    try_stream! {
        let mut events = byte_stream.eventsource();
        let mut blocks: HashMap<usize, BlockAcc> = HashMap::new();
        // The `message_start` event carries the input/cache token counts; `message_delta` carries
        // only the running output count (its input fields default to 0). Remember the input side so
        // the final usage chunk keeps it, instead of consumers' last-wins assignment zeroing it.
        let mut prior_usage = flux_core::Usage::default();
        // A-35: a `data:` frame that isn't valid JSON, and a well-formed frame whose `type` the
        // enum doesn't recognize (a new vendor extension, a keep-alive-ish event), both surface as
        // the same `serde_json::Error` from this one deserialize call — `StreamEvent`'s internally
        // tagged representation rejects an unknown tag exactly like malformed JSON. Skip + count
        // both instead of killing the stream; a *declared* provider error (`StreamEvent::Error`,
        // matched below) still parses successfully and stays fatal via its own arm.
        let mut dropped_frames: u32 = 0;
        let mut first_drop_detail: Option<String> = None;

        while let Some(event) = events.next().await {
            let event = event.map_err(|e| Error::Provider(format!("sse stream: {e}")))?;
            let data = event.data.trim();
            // Anthropic-direct ends the stream with `message_stop`; OpenRouter and ollama (which
            // proxy the Messages shape through OpenAI-compatible plumbing) also append an OpenAI-style
            // `[DONE]` sentinel that isn't JSON. Skip it — and any blank keepalive — before parsing.
            if data.is_empty() || data == "[DONE]" {
                continue;
            }

            let parsed: StreamEvent = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(e) => {
                    dropped_frames += 1;
                    if first_drop_detail.is_none() {
                        first_drop_detail = Some(e.to_string());
                    }
                    tracing::warn!(
                        error = %e,
                        frame = %data.chars().take(200).collect::<String>(),
                        "messages SSE: skipping unparseable or unrecognized event frame"
                    );
                    continue;
                }
            };
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
                        // L-23: surface the fragment as a `ToolInputDelta` too (in addition to the
                        // existing accumulation this codec always did) so a caller can trace or
                        // render a large native tool input progressively — purely additive, the completed
                        // `Chunk::Block` below remains the sole source of truth for the final call.
                        let name = if let Some(BlockAcc::ToolUse { json, name, .. }) =
                            blocks.get_mut(&index)
                        {
                            json.push_str(&partial_json);
                            Some(name.clone())
                        } else {
                            None
                        };
                        if let Some(name) = name {
                            yield Chunk::ToolInputDelta { name, partial_json };
                        }
                    }
                },
                StreamEvent::ContentBlockStop { index } => {
                    if let Some(acc) = blocks.remove(&index) {
                        yield Chunk::Block(acc.finish());
                    }
                }
                StreamEvent::MessageDelta { delta, usage } => {
                    if let Some(u) = usage {
                        // Carry the input/cache counts forward from the prior usage frame so they
                        // aren't clobbered to 0 by this delta (which only reports output tokens —
                        // and, per C-34, reported cost may or may not repeat on every frame).
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
                        // C-34: OpenRouter's `cost` isn't guaranteed to repeat on every
                        // `message_delta` — once seen it is sticky, carried forward like the
                        // token counts above, so a cost reported on an early frame survives to
                        // the final one even if that final frame's usage omits it.
                        if u.reported_cost_usd.is_none() {
                            u.reported_cost_usd = prior_usage.reported_cost_usd;
                        }
                        // Refresh `prior_usage` after EVERY usage frame (not just message_start) —
                        // otherwise a cost/count seen on an early delta can't survive to a LATER
                        // delta that only carries the next slice.
                        prior_usage = u.clone();
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

        if dropped_frames > 0 {
            yield Chunk::StreamDiagnostic {
                dropped_frames,
                detail: format!(
                    "{dropped_frames} unparseable or unrecognized messages SSE frame(s) dropped; first error: {}",
                    first_drop_detail.unwrap_or_default(),
                ),
            };
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
            sampling_params: true,
            extra_body: Default::default(),
        }
    }

    #[test]
    fn thinking_and_effort_are_omitted_when_the_model_rejects_them() {
        // The C-49 regression shape: a haiku-class model with thinking requested must get a body
        // with NO `thinking` and NO `output_config` — both are hard 400s on that generation.
        let req = Request::new("claude-haiku-4-5", "hi")
            .with_thinking(true)
            .with_effort(Effort::High);
        let q = MessagesQuirks {
            thinking_adaptive: false,
            effort_output_config: false,
            ..anthropic_quirks()
        };
        let body = build_messages_body(&req, &q).unwrap();
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn sampling_params_are_omitted_when_the_model_rejects_them() {
        // Fable/Opus-4.7+/Sonnet-5 reject `temperature`/`top_p` outright; with thinking OFF the
        // builder previously fell back to emitting them.
        let mut req = Request::new("claude-fable-5", "hi");
        req.temperature = Some(0.25);
        req.top_p = Some(0.5);
        let q = MessagesQuirks {
            sampling_params: false,
            ..anthropic_quirks()
        };
        let body = build_messages_body(&req, &q).unwrap();
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());

        // …and a model that accepts them still gets them (thinking off). Dyadic values so the
        // f32 → JSON number round-trip compares exactly.
        let mut req = Request::new("claude-sonnet-4-6", "hi");
        req.temperature = Some(0.25);
        req.top_p = Some(0.5);
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        assert_eq!(body["temperature"], 0.25);
        assert_eq!(body["top_p"], 0.5);
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
        q.extra_body
            .insert("provider".into(), json!({ "require_parameters": true }));
        let body = build_messages_body(&req, &q).unwrap();
        assert_eq!(body["provider"]["require_parameters"], true);
    }

    #[test]
    fn segmented_system_renders_breakpoints_on_cached_segments_only() {
        use flux_provider::SystemSegment;
        let mut req = Request::new("claude-sonnet-4-6", "hi");
        req.system_segments = vec![
            SystemSegment {
                text: "catalog".into(),
                cache: true,
            },
            SystemSegment {
                text: "identity".into(),
                cache: true,
            },
            SystemSegment {
                text: "symbols".into(),
                cache: false,
            },
        ];
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        let sys = body["system"].as_array().expect("block array");
        assert_eq!(sys.len(), 3);
        assert_eq!(sys[0]["text"], "catalog");
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(sys[1]["cache_control"]["type"], "ephemeral");
        assert!(
            sys[2].get("cache_control").is_none(),
            "the dynamic tail must not carry a breakpoint"
        );
    }

    /// Recursively count `cache_control` keys anywhere in the request body (system + tools +
    /// messages) — Anthropic's 4-breakpoint ceiling is across the whole request.
    fn count_cache_control(v: &Value) -> usize {
        match v {
            Value::Object(m) => {
                (m.contains_key("cache_control") as usize)
                    + m.values().map(count_cache_control).sum::<usize>()
            }
            Value::Array(a) => a.iter().map(count_cache_control).sum(),
            _ => 0,
        }
    }

    #[test]
    fn assembled_request_caps_cache_breakpoints_at_four() {
        use flux_provider::SystemSegment;
        // Mirror the subscription-claude planner layout (transport prefix + planner-A + phase +
        // base-B, all cache:true) and then add a FIFTH cache:true segment — the case that today tips
        // the request past Anthropic's 4-breakpoint ceiling into a 400.
        let big = |name: &str| SystemSegment {
            text: format!("{name} {}", "x".repeat(CACHE_MIN_CHARS)),
            cache: true,
        };
        let mut req = Request::new("claude-opus-4-8", "hi");
        req.system_segments = vec![
            big("transport-prefix"),
            big("planner-A-catalog-and-grammar-the-large-stable-prefix"),
            big("phase-contract"),
            big("base-B-identity"),
            big("future-extra-cache-segment"),
            SystemSegment {
                text: "per-turn session symbols".into(),
                cache: false,
            },
        ];
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        assert!(
            count_cache_control(&body) <= MAX_CACHE_BREAKPOINTS,
            "Anthropic allows at most {MAX_CACHE_BREAKPOINTS} cache_control breakpoints; got {}: {body}",
            count_cache_control(&body)
        );
        // The cache hit isn't regressed: the large stable prefix still carries a breakpoint.
        let sys = body["system"].as_array().unwrap();
        let stable_prefix = sys
            .iter()
            .find(|b| {
                b["text"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("planner-A-catalog"))
            })
            .expect("the stable prefix segment is present");
        assert!(
            stable_prefix.get("cache_control").is_some(),
            "the largest stable-prefix segment must keep its breakpoint: {body}"
        );
    }

    /// C-134's named failing-first test: the conversation tail must carry a breakpoint, so the
    /// cached prefix no longer stops where the system prompt ends. Before this, every
    /// `cache_control` in a request lived in the `system` array and the whole growing transcript was
    /// re-priced at full input rate on every round.
    #[test]
    fn conversation_tail_carries_a_cache_breakpoint() {
        let mut req = Request::new("claude-opus-4-8", "hi");
        req.cache_tail = true;
        req.messages = vec![
            Message::user_text("read the file"),
            Message::assistant(vec![ContentBlock::text("on it")]),
            Message::user(vec![
                ContentBlock::tool_result_text("t1", "line one", false),
                ContentBlock::tool_result_text("t2", "line two", false),
            ]),
        ];
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        let messages = body["messages"].as_array().unwrap();

        // Exactly one message-side breakpoint, on the LAST block of the LAST message.
        assert_eq!(count_cache_control(&body["messages"]), 1, "{body}");
        let last = messages.last().unwrap()["content"].as_array().unwrap();
        assert!(
            last.last().unwrap().get("cache_control").is_some(),
            "the final content block must carry it: {body}"
        );
        assert!(
            last[0].get("cache_control").is_none(),
            "no other block may: {body}"
        );
        // C-135: the rolling tail stays on the 5-minute default — it is rewritten every round, so a
        // 1h write premium would buy retention the entry never lives to use.
        assert_eq!(last.last().unwrap()["cache_control"]["type"], "ephemeral");
        assert!(
            last.last().unwrap()["cache_control"].get("ttl").is_none(),
            "the tail must NOT take the 1h TTL: {body}"
        );
    }

    /// Opting out (or a provider without caching) leaves the messages untouched.
    #[test]
    fn conversation_tail_is_opt_in() {
        let mut req = Request::new("claude-opus-4-8", "hi");
        req.messages = vec![Message::user_text("hello")];
        let off = build_messages_body(&req, &anthropic_quirks()).unwrap();
        assert_eq!(count_cache_control(&off["messages"]), 0, "{off}");

        // Even asked for, a profile with caching disabled must not stamp one.
        req.cache_tail = true;
        let no_caching = build_messages_body(&req, &MessagesQuirks::default()).unwrap();
        assert_eq!(
            count_cache_control(&no_caching["messages"]),
            0,
            "{no_caching}"
        );
    }

    /// C-134: the ≤4 ceiling is a UNION budget. Subscription-claude's intent layout already stamps
    /// exactly four cache:true segments, so the tail's slot must cost a system segment its
    /// breakpoint rather than pushing the request to five and 400ing every planner call.
    #[test]
    fn tail_breakpoint_shares_the_four_slot_budget_with_the_system_segments() {
        use flux_provider::SystemSegment;
        let seg = |name: &str, chars: usize| SystemSegment {
            text: format!("{name} {}", "x".repeat(chars)),
            cache: true,
        };
        let mut req = Request::new("claude-opus-4-8", "hi");
        req.cache_tail = true;
        // The subscription-claude intent layout: identity prefix (tiny) + INTENT_SYSTEM + index + base.
        req.system_segments = vec![
            seg("claude-code-identity-prefix", 8),
            seg("intent-system", 2_000),
            seg("family-index", 3_000),
            seg("base-system", 9_000),
        ];
        req.messages = vec![Message::user_text("hello")];
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();

        assert_eq!(
            count_cache_control(&body),
            MAX_CACHE_BREAKPOINTS,
            "the union must be exactly the ceiling, never over: {body}"
        );
        assert_eq!(
            count_cache_control(&body["messages"]),
            1,
            "tail kept: {body}"
        );

        // Which system breakpoint is dropped is pinned, not incidental: the SMALLEST cache:true
        // segment loses it, so the large stable prefix keeps its resume point. The dropped
        // segment's bytes still ride inside the next cached segment's prefix.
        let sys = body["system"].as_array().unwrap();
        let stamped = |prefix: &str| {
            sys.iter()
                .find(|b| b["text"].as_str().is_some_and(|t| t.starts_with(prefix)))
                .unwrap_or_else(|| panic!("segment {prefix} present"))
                .get("cache_control")
                .is_some()
        };
        assert!(
            !stamped("claude-code-identity-prefix"),
            "smallest drops: {body}"
        );
        assert!(stamped("intent-system"), "{body}");
        assert!(stamped("family-index"), "{body}");
        assert!(stamped("base-system"), "{body}");
    }

    /// C-135: the stable tools+system prefix takes the 1-hour TTL so an interactive pause between
    /// turns — routinely longer than five minutes — no longer cold-starts it.
    #[test]
    fn stable_system_prefix_takes_the_one_hour_ttl() {
        use flux_provider::SystemSegment;
        let mut req = Request::new("claude-opus-4-8", "hi");
        req.system_segments = vec![SystemSegment {
            text: "x".repeat(CACHE_MIN_CHARS),
            cache: true,
        }];
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h", "{body}");

        // The unsegmented path (a plain long `system`) is the same prefix and takes the same TTL.
        let mut plain = Request::new("claude-opus-4-8", "hi");
        plain.system = Some("y".repeat(CACHE_MIN_CHARS));
        let body = build_messages_body(&plain, &anthropic_quirks()).unwrap();
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h", "{body}");
    }

    /// C-135: a profile without prompt caching (ollama-anthropic) must not leak a TTL — or any
    /// `cache_control` — onto the wire.
    #[test]
    fn no_ttl_leaks_into_a_non_caching_profile() {
        use flux_provider::SystemSegment;
        let mut req = Request::new("some-local-model", "hi");
        req.cache_tail = true;
        req.system_segments = vec![SystemSegment {
            text: "x".repeat(CACHE_MIN_CHARS),
            cache: true,
        }];
        req.messages = vec![Message::user_text("hello")];
        let body = build_messages_body(&req, &MessagesQuirks::default()).unwrap();
        assert_eq!(count_cache_control(&body), 0, "{body}");
        assert!(!body.to_string().contains("1h"), "{body}");
    }

    /// C-138: the cache-layout **contract**, pinned end to end for both Anthropic-family transports.
    ///
    /// This is the guard that stops a future segment or tool-set change halving the cache in
    /// silence. It asserts the realized LAYOUT, not just the count:
    ///   * the union of breakpoints never exceeds Anthropic's hard maximum of four;
    ///   * the stable tools+system prefix carries the 1h TTL;
    ///   * the rolling conversation tail carries the 5m default;
    ///   * the per-turn (cache:false) segment sits AFTER the last system breakpoint.
    #[test]
    fn cache_layout_contract() {
        use flux_provider::SystemSegment;
        let seg = |name: &str, chars: usize, cache: bool| SystemSegment {
            text: format!("{name} {}", "x".repeat(chars)),
            cache,
        };

        // `claude` (subscription OAuth) inserts its identity line as cached segment 0; `anthropic`
        // does not. Both layouts must satisfy the contract.
        for (transport, segments) in [
            (
                "claude",
                vec![
                    seg("identity-prefix", 8, true),
                    seg("explore-system", 1_500, true),
                    seg("base-system", 9_000, true),
                    seg("per-turn-intent", 200, false),
                ],
            ),
            (
                "anthropic",
                vec![
                    seg("explore-system", 1_500, true),
                    seg("base-system", 9_000, true),
                    seg("per-turn-intent", 200, false),
                ],
            ),
        ] {
            let mut req = Request::new("claude-sonnet-5", "hi");
            req.cache_tail = true;
            req.system_segments = segments;
            req.messages = vec![
                Message::user_text("go"),
                Message::assistant(vec![ContentBlock::text("working")]),
                Message::user(vec![ContentBlock::tool_result_text("t1", "result", false)]),
            ];
            req.tools = vec![ToolDef {
                name: "read".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
            }];
            let body = build_messages_body(&req, &anthropic_quirks()).unwrap();

            let total = count_cache_control(&body);
            assert!(
                total <= MAX_CACHE_BREAKPOINTS,
                "{transport}: the union of breakpoints must stay within Anthropic's ceiling of \
                 {MAX_CACHE_BREAKPOINTS}; got {total}. See docs/designs/llm-cache-review.md — the \
                 tail breakpoint shares this budget with the system segments: {body}"
            );

            // The rolling tail: present, on the 5-minute default.
            assert_eq!(
                count_cache_control(&body["messages"]),
                1,
                "{transport}: exactly one tail breakpoint: {body}"
            );
            let tail = body["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_array()
                .unwrap()
                .last()
                .unwrap();
            assert_eq!(tail["cache_control"]["type"], "ephemeral", "{transport}");
            assert!(
                tail["cache_control"].get("ttl").is_none(),
                "{transport}: the rolling tail must NOT take the 1h TTL: {body}"
            );

            // The stable prefix: on the 1-hour TTL, every one of them.
            let sys = body["system"].as_array().unwrap();
            let stamped: Vec<&Value> = sys
                .iter()
                .filter(|b| b.get("cache_control").is_some())
                .collect();
            assert!(!stamped.is_empty(), "{transport}: {body}");
            for block in &stamped {
                assert_eq!(
                    block["cache_control"]["ttl"], "1h",
                    "{transport}: the stable prefix takes the 1h TTL: {body}"
                );
            }

            // The per-turn segment rides AFTER the last breakpoint, so changing it cannot
            // invalidate the cached prefix.
            let last_stamped = sys
                .iter()
                .rposition(|b| b.get("cache_control").is_some())
                .expect("a stamped segment exists");
            let per_turn = sys
                .iter()
                .position(|b| {
                    b["text"]
                        .as_str()
                        .is_some_and(|t| t.starts_with("per-turn-intent"))
                })
                .expect("the per-turn segment is present");
            assert!(
                per_turn > last_stamped,
                "{transport}: per-turn material must follow the last breakpoint \
                 (at {last_stamped}, found at {per_turn}): {body}"
            );
        }
    }

    /// C-134: a round that appends more than [`CACHE_LOOKBACK_BLOCKS`] content blocks leaves the
    /// previous round's tail out of Anthropic's backward search window, so that round's tail is
    /// re-written rather than read. With the four-slot budget already full on subscription-claude
    /// there is no room for intermediate breakpoints, so the behaviour is accepted — and pinned here
    /// so it stays a known, observable property rather than a silent regression.
    #[test]
    fn tail_breakpoint_out_of_lookback() {
        let wide: Vec<ContentBlock> = (0..CACHE_LOOKBACK_BLOCKS + 5)
            .map(|i| ContentBlock::tool_result_text(format!("t{i}"), "out", false))
            .collect();
        let appended = wide.len();
        let mut req = Request::new("claude-opus-4-8", "hi");
        req.cache_tail = true;
        req.messages = vec![Message::user_text("go"), Message::user(wide)];
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();

        // Still exactly one tail breakpoint — we do not spend extra slots trying to bridge the gap.
        assert_eq!(count_cache_control(&body["messages"]), 1, "{body}");
        assert!(
            appended > CACHE_LOOKBACK_BLOCKS,
            "this round appends {appended} blocks, beyond the {CACHE_LOOKBACK_BLOCKS}-block window: \
             the previous round's tail entry is out of range and this tail is written, not read"
        );
    }

    #[test]
    fn four_cache_segments_are_all_kept() {
        use flux_provider::SystemSegment;
        // Exactly the current subscription layout (4 cache:true): none dropped, no regression.
        let seg = |name: &str| SystemSegment {
            text: name.to_string(),
            cache: true,
        };
        let mut req = Request::new("claude-opus-4-8", "hi");
        req.system_segments = vec![seg("a"), seg("b"), seg("c"), seg("d")];
        let body = build_messages_body(&req, &anthropic_quirks()).unwrap();
        assert_eq!(count_cache_control(&body), 4, "{body}");
    }

    #[test]
    fn segmented_system_joins_plain_when_caching_is_off() {
        use flux_provider::SystemSegment;
        let mut req = Request::new("z-ai/glm-4.6", "hi");
        req.system_segments = vec![
            SystemSegment {
                text: "a".into(),
                cache: true,
            },
            SystemSegment {
                text: "b".into(),
                cache: false,
            },
        ];
        let q = MessagesQuirks {
            prompt_caching: false,
            ..anthropic_quirks()
        };
        let body = build_messages_body(&req, &q).unwrap();
        assert_eq!(body["system"], "a\n\nb");
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
        // L-23: the codec now ALSO surfaces each `input_json_delta` fragment as a `ToolInputDelta`
        // (in addition to the accumulation it always did internally) so a caller can render a
        // plan skeleton progressively — collected here to prove it rides alongside, unaffected.
        let mut tool_input_deltas = Vec::new();

        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                Chunk::TextDelta(t) => text.push_str(&t),
                Chunk::Block(b) => blocks.push(b),
                Chunk::Usage(u) => last_usage = Some(u),
                Chunk::Done { stop_reason } => stop = stop_reason,
                Chunk::ToolInputDelta { name, partial_json } => {
                    tool_input_deltas.push((name, partial_json))
                }
                Chunk::MessageStart { .. } => {}
                Chunk::ThinkingDelta(_) => {}
                Chunk::StreamDiagnostic { .. } => {}
            }
        }

        assert_eq!(text, "Hello world");
        assert_eq!(stop, Some(StopReason::ToolUse));
        assert_eq!(
            tool_input_deltas,
            vec![
                ("read".to_string(), "{\"path\":".to_string()),
                ("read".to_string(), "\"a.txt\"}".to_string()),
            ]
        );
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

    /// C-34: the Messages wire (glm live probe, 2026-07-04) puts OpenRouter's `cost` on the FINAL
    /// `message_delta` usage frame — `message_start` usage never carries it. The final `Usage`
    /// chunk must carry that cost AND the carried-forward input/cache counts together.
    #[tokio::test]
    async fn messages_stream_carries_reported_cost_through_final_delta() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"z-ai/glm-4.6\",\"usage\":{\"input_tokens\":20,\"output_tokens\":1}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":10}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42,\"cost\":0.0005052,\"is_byok\":false}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut last_usage = None;
        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Usage(u) = chunk.unwrap() {
                last_usage = Some(u);
            }
        }
        let usage = last_usage.expect("a final usage chunk");
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(
            usage.input_tokens, 20,
            "input tokens from message_start must still carry to the final delta"
        );
        assert!(
            (usage
                .reported_cost_usd
                .expect("reported cost on the final delta")
                - 0.0005052)
                .abs()
                < 1e-12,
            "got {:?}",
            usage.reported_cost_usd
        );

        // Variant: the cost arrives on an EARLIER usage frame, not the final one — it must still
        // survive (sticky) to the last `Usage` chunk despite that frame not repeating it.
        let sse_early_cost = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"z-ai/glm-4.6\",\"usage\":{\"input_tokens\":20,\"output_tokens\":1}}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":10,\"cost\":0.0001,\"is_byok\":false}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":42}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream: ByteStream = Box::pin(futures::stream::once(async move {
            Ok(bytes::Bytes::from(sse_early_cost))
        }));
        let mut last_usage = None;
        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::Usage(u) = chunk.unwrap() {
                last_usage = Some(u);
            }
        }
        let usage = last_usage.expect("a final usage chunk");
        assert_eq!(usage.output_tokens, 42);
        assert!(
            (usage
                .reported_cost_usd
                .expect("cost from an earlier frame must stick")
                - 0.0001)
                .abs()
                < 1e-12,
            "got {:?}",
            usage.reported_cost_usd
        );
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

    /// A-35: a `data:` frame whose bytes don't even parse as JSON is skipped and counted, never
    /// fatal — the good frames around it still process normally.
    #[tokio::test]
    async fn messages_bad_event_json_is_skipped_and_the_stream_survives() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            // Syntactically broken — not valid JSON at all.
            "data: {this is not json at all}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut text = String::new();
        let mut stop = None;
        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            match chunk.expect("a bad-JSON frame must not fail the stream") {
                Chunk::TextDelta(t) => text.push_str(&t),
                Chunk::Done { stop_reason } => stop = stop_reason,
                _ => {}
            }
        }
        assert_eq!(text, "Hello");
        assert_eq!(stop, Some(StopReason::EndTurn));
    }

    /// A-35: a *well-formed* JSON frame whose `type` the enum doesn't know (a new vendor
    /// extension, a keep-alive-ish event) must be tolerated the same way — `StreamEvent` has no
    /// catch-all arm, so this is a normal serde "unknown variant" error, not a syntax error.
    #[tokio::test]
    async fn messages_unknown_event_type_is_tolerated() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "data: {\"type\":\"some_future_vendor_event\",\"foo\":\"bar\"}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut text = String::new();
        let mut stop = None;
        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            match chunk.expect("an unknown event type must not fail the stream") {
                Chunk::TextDelta(t) => text.push_str(&t),
                Chunk::Done { stop_reason } => stop = stop_reason,
                _ => {}
            }
        }
        assert_eq!(text, "Hi");
        assert_eq!(stop, Some(StopReason::EndTurn));
    }

    /// A-35: both drop shapes (bad JSON + unknown type) land in the same counter and surface as
    /// exactly ONE end-of-stream `StreamDiagnostic`.
    #[tokio::test]
    async fn messages_dropped_frames_surface_a_stream_diagnostic() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            // Shape 1: syntactically broken JSON.
            "data: {this is not json at all}\n\n",
            // Shape 2: well-formed JSON, unrecognized type.
            "data: {\"type\":\"some_future_vendor_event\",\"foo\":\"bar\"}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut diagnostics = Vec::new();
        let mut stream = map_messages_stream(byte_stream);
        while let Some(chunk) = stream.next().await {
            if let Chunk::StreamDiagnostic {
                dropped_frames,
                detail,
            } = chunk.expect("dropped frames must not fail the stream")
            {
                diagnostics.push((dropped_frames, detail));
            }
        }
        assert_eq!(
            diagnostics.len(),
            1,
            "exactly one end-of-stream diagnostic, got {diagnostics:?}"
        );
        assert_eq!(diagnostics[0].0, 2, "both drop shapes counted together");
        assert!(!diagnostics[0].1.is_empty());
    }

    /// A-35 guardrail pin: a *declared* provider error event (`type: "error"`) must stay fatal —
    /// tolerance is for unparseable/unrecognized bytes only, never for a real outage the provider
    /// told us about. This must pass both before and after the tolerance change.
    #[tokio::test]
    async fn messages_declared_error_event_stays_fatal() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        );
        let byte_stream: ByteStream =
            Box::pin(futures::stream::once(
                async move { Ok(bytes::Bytes::from(sse)) },
            ));

        let mut stream = map_messages_stream(byte_stream);
        let mut saw_err = false;
        while let Some(chunk) = stream.next().await {
            if let Err(e) = chunk {
                saw_err = true;
                let msg = e.to_string();
                assert!(
                    msg.contains("overloaded_error") && msg.contains("Overloaded"),
                    "got: {msg}"
                );
            }
        }
        assert!(saw_err, "a declared error event must still fail the stream");
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
        assert_eq!(
            parse_tool_input(r#"{"path":"a.txt"}"#).unwrap()["path"],
            "a.txt"
        );
        // Trailing junk (deepseek-v4-flash): the extra brace is ignored.
        assert_eq!(
            parse_tool_input(r#"{"path":"a.txt"}}"#).unwrap()["path"],
            "a.txt"
        );
        // Truncated object (glm-5.2): the missing final brace is repaired.
        assert_eq!(
            parse_tool_input(r#"{"ast":{"body":[]}"#).unwrap()["ast"]["body"],
            json!([])
        );
        // Truncated mid-string: closed best-effort (keeps the partial value).
        assert_eq!(
            parse_tool_input(r#"{"path":"a.tx"#).unwrap()["path"],
            "a.tx"
        );
        // Genuinely broken (a missing value, not just unbalanced) still errors.
        assert!(parse_tool_input(r#"{"path": }"#).is_err());
    }

    /// A-32: when even repair fails, the input becomes a parse-error sentinel — the stream (and
    /// with it the turn) survives, and the consumer rejects the call with repairable feedback.
    #[test]
    fn unrepairable_tool_input_becomes_a_sentinel() {
        // Empty stays the empty-object convention.
        assert_eq!(tool_input_or_sentinel("  "), json!({}));
        // Repairable shapes pass through the repair, no sentinel.
        assert_eq!(tool_input_or_sentinel(r#"{"path":"a.tx"#)["path"], "a.tx");
        // Unrepairable → sentinel with the serde message and a raw prefix.
        let s = tool_input_or_sentinel(r#"{"path": }"#);
        assert!(s[flux_core::ARGS_PARSE_ERROR_KEY].is_string(), "{s}");
        assert!(s[flux_core::ARGS_RAW_PREFIX_KEY]
            .as_str()
            .unwrap()
            .starts_with(r#"{"path"#));
    }
}

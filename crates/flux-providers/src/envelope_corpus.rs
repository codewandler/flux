//! Malformed-envelope corpus (A-37).
//!
//! Runtime companion to `clippy.toml`'s disallowed-methods ban: the lint stops a *new* bare-parse
//! call site at merge time, but can't tell a tolerant `match` from a bare `?` inside a function
//! that already carries a legitimate `#[allow(clippy::disallowed_methods)]` (see that file's
//! header). This module proves the actual runtime invariant — **provider bytes never error a
//! chunk stream** (docs/designs/stream-resilience.md) — holds for every codec that exists today,
//! by taking one valid fixture turn per codec and systematically corrupting it three ways:
//!
//!   - **truncation** at every byte offset (the connection dies mid-stream)
//!   - **junk-frame injection** at every frame boundary (a keep-alive/garbage frame lands between
//!     two good ones)
//!   - **single-frame corruption** (one frame's bytes are mangled, the rest of the stream is fine)
//!
//! For the three SSE-based codecs (chat, responses, messages) the assertion is unconditional: no
//! `Err` chunk is ever yielded. For bedrock's binary AWS event-stream framing, corruption *can*
//! legitimately still error (a truncated/CRC-broken frame is a genuine framing-integrity failure,
//! A-36) — the assertion there is that every such `Err` classifies as `Error::StreamDecode`, never
//! `Error::Provider`, so the A-33 planner backstop retries the call instead of killing the turn.
//!
//! ADD YOUR CODEC HERE: when a fifth wire codec ships, give it a `*_FRAMES` (or equivalent) fixture
//! below and the three truncation/junk-injection/single-frame-corruption tests mirroring the
//! pattern used for chat/responses/messages (or bedrock's binary-framing variant).

use bytes::Bytes;
use futures::StreamExt;

use flux_core::Error;
use flux_provider::{ByteStream, ChunkStream};

use crate::bedrock::map_bedrock_event_stream;
use crate::messages::map_messages_stream;
use crate::openai::{map_chat_stream, map_responses_stream};

// ---------------------------------------------------------------------------
// Fixtures — one valid, representative turn per codec, split into individual frames so the
// corruption strategies below can operate at frame granularity.
// ---------------------------------------------------------------------------

/// A valid OpenAI chat-completions SSE turn (text + a finish + a usage frame + `[DONE]`).
const CHAT_FRAMES: &[&str] = &[
    "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
    "data: [DONE]\n\n",
];

/// A valid OpenAI Responses typed-SSE turn (text delta + a tool call + completion/usage).
const RESPONSES_FRAMES: &[&str] = &[
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n",
    "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":9,\"output_tokens\":4}}}\n\n",
];

/// A valid Anthropic Messages SSE turn (text block start/delta/stop + a terminal delta + stop).
const MESSAGES_FRAMES: &[&str] = &[
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-4-6\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
];

/// The same Messages turn as `MESSAGES_FRAMES`, but as raw event JSON — bedrock wraps each one in
/// its own AWS event-stream `chunk` frame (see [`chunk_frame`]) instead of SSE text.
const BEDROCK_EVENTS: &[&str] = &[
    r#"{"type":"message_start","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":1}}}"#,
    r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
    r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
    r#"{"type":"content_block_stop","index":0}"#,
    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
    r#"{"type":"message_stop"}"#,
];

/// A single junk `data:` frame — not valid JSON at all, and not the `[DONE]` sentinel — used for
/// the junk-frame-injection strategy on all three SSE-based codecs (they all tokenize `data:`
/// lines the same way; `event:` lines, where present, are irrelevant to a garbage-injection test).
const SSE_JUNK_FRAME: &str = "data: not json at all {{{\n\n";

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

fn byte_stream_of(bytes: Vec<u8>) -> ByteStream {
    Box::pin(futures::stream::once(async move {
        Ok::<_, Error>(Bytes::from(bytes))
    }))
}

/// Drain a chunk stream and assert every item is `Ok` — the unconditional invariant for the three
/// SSE-based codecs.
async fn assert_chunk_stream_never_errs(mut stream: ChunkStream, ctx: &str) {
    while let Some(item) = stream.next().await {
        assert!(
            item.is_ok(),
            "{ctx}: unexpected Err chunk: {:?}",
            item.err()
        );
    }
}

/// Corrupt a frame's JSON payload just enough to break JSON syntax, while keeping the SSE framing
/// (the `data:`/`event:` lines and the blank-line terminator) and UTF-8 validity intact.
/// Corruption deliberately stays inside the model's JSON envelope — the layer A-34/A-35 harden —
/// rather than the SSE tokenizer itself (an orthogonal, out-of-scope transport concern; see
/// docs/designs/stream-resilience.md's risk notes on why byte-level UTF-8 corruption isn't used
/// here).
fn corrupt_json_payload(frame: &str) -> String {
    match frame.find('{') {
        // An opening brace becomes a stray closing one: guaranteed-invalid JSON, same length,
        // ASCII-safe (never breaks UTF-8 or the surrounding SSE framing).
        Some(pos) => {
            let mut s = frame.to_string();
            s.replace_range(pos..pos + 1, "}");
            s
        }
        // No JSON body at all (e.g. the chat codec's `data: [DONE]` sentinel) — corrupt the
        // sentinel itself so it's neither `[DONE]` nor valid JSON.
        None => frame.replace("data: ", "data: not-json "),
    }
}

// -- bedrock frame construction ---------------------------------------------------------------
//
// Duplicated (not imported) from bedrock.rs's own private test helpers: `crc32`/`encode_frame`/
// `chunk_frame` there are module-private, and this story's file-scope is deliberately narrow
// (targeted `#[allow]`s only) rather than widening bedrock.rs's internal visibility just for this
// corpus. The algorithm is the same standard CRC-32/IEEE, pinned by the same check value
// bedrock.rs's own suite uses.

fn corpus_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn encode_string_header(name: &str, value: &str) -> Vec<u8> {
    let mut b = vec![name.len() as u8];
    b.extend_from_slice(name.as_bytes());
    b.push(7); // value type: string
    b.extend_from_slice(&(value.len() as u16).to_be_bytes());
    b.extend_from_slice(value.as_bytes());
    b
}

fn encode_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let hb: Vec<u8> = headers
        .iter()
        .flat_map(|(n, v)| encode_string_header(n, v))
        .collect();
    let total = 12 + hb.len() + payload.len() + 4;
    let mut f = Vec::with_capacity(total);
    f.extend_from_slice(&(total as u32).to_be_bytes());
    f.extend_from_slice(&(hb.len() as u32).to_be_bytes());
    let prelude_crc = corpus_crc32(&f[..8]);
    f.extend_from_slice(&prelude_crc.to_be_bytes());
    f.extend_from_slice(&hb);
    f.extend_from_slice(payload);
    let msg_crc = corpus_crc32(&f);
    f.extend_from_slice(&msg_crc.to_be_bytes());
    f
}

/// A `chunk` event frame carrying one Anthropic stream event (the live wire shape: base64 `bytes`).
fn chunk_frame(event_json: &str) -> Vec<u8> {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    let payload = serde_json::json!({ "bytes": BASE64.encode(event_json) }).to_string();
    encode_frame(
        &[
            (":message-type", "event"),
            (":event-type", "chunk"),
            (":content-type", "application/json"),
        ],
        payload.as_bytes(),
    )
}

/// A structurally-valid AWS frame (correct CRCs) whose `chunk` payload is garbage — exercises the
/// tolerated `FrameOutcome::Garbage` path (A-36), not a declared exception.
fn bedrock_garbage_chunk_frame() -> Vec<u8> {
    encode_frame(
        &[(":message-type", "event"), (":event-type", "chunk")],
        b"not json at all",
    )
}

/// Feed `wire` through the bedrock deframer and assert every `Err` item (if any) is
/// `Error::StreamDecode` — never `Error::Provider`.
async fn assert_bedrock_errs_are_classified(wire: Vec<u8>, ctx: &str) {
    let results: Vec<_> = map_bedrock_event_stream(byte_stream_of(wire))
        .collect::<Vec<_>>()
        .await;
    for r in &results {
        if let Err(e) = r {
            assert!(
                matches!(e, Error::StreamDecode(_)),
                "{ctx}: Err must classify as StreamDecode, got {e:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Chat (OpenAI chat-completions SSE)
// ---------------------------------------------------------------------------

/// Sampling strategy: truncate at every byte offset of the fixture (0..=len). The fixture is a
/// few hundred bytes, so exhaustive is cheap here — a much larger fixture would warrant a stride.
#[tokio::test]
async fn chat_stream_survives_truncation_at_every_offset() {
    let bytes = CHAT_FRAMES.concat().into_bytes();
    for cut in 0..=bytes.len() {
        let stream: ChunkStream = Box::pin(map_chat_stream(byte_stream_of(bytes[..cut].to_vec())));
        assert_chunk_stream_never_errs(
            stream,
            &format!("chat truncated at byte {cut}/{}", bytes.len()),
        )
        .await;
    }
}

#[tokio::test]
async fn chat_stream_survives_junk_frame_injection() {
    for i in 0..=CHAT_FRAMES.len() {
        let mut frames: Vec<&str> = CHAT_FRAMES.to_vec();
        frames.insert(i, SSE_JUNK_FRAME);
        let bytes = frames.concat().into_bytes();
        let stream: ChunkStream = Box::pin(map_chat_stream(byte_stream_of(bytes)));
        assert_chunk_stream_never_errs(
            stream,
            &format!("chat junk frame injected at boundary {i}"),
        )
        .await;
    }
}

#[tokio::test]
async fn chat_stream_survives_single_frame_corruption() {
    for i in 0..CHAT_FRAMES.len() {
        let mut frames: Vec<String> = CHAT_FRAMES.iter().map(|s| s.to_string()).collect();
        frames[i] = corrupt_json_payload(&frames[i]);
        let bytes = frames.concat().into_bytes();
        let stream: ChunkStream = Box::pin(map_chat_stream(byte_stream_of(bytes)));
        assert_chunk_stream_never_errs(stream, &format!("chat frame {i} corrupted")).await;
    }
}

// ---------------------------------------------------------------------------
// Responses (OpenAI Responses typed-SSE)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn responses_stream_survives_truncation_at_every_offset() {
    let bytes = RESPONSES_FRAMES.concat().into_bytes();
    for cut in 0..=bytes.len() {
        let stream: ChunkStream =
            Box::pin(map_responses_stream(byte_stream_of(bytes[..cut].to_vec())));
        assert_chunk_stream_never_errs(
            stream,
            &format!("responses truncated at byte {cut}/{}", bytes.len()),
        )
        .await;
    }
}

/// Contractual name (A-37 Acceptance): junk-frame injection at every frame boundary.
#[tokio::test]
async fn responses_stream_survives_junk_frame_injection() {
    for i in 0..=RESPONSES_FRAMES.len() {
        let mut frames: Vec<&str> = RESPONSES_FRAMES.to_vec();
        frames.insert(i, SSE_JUNK_FRAME);
        let bytes = frames.concat().into_bytes();
        let stream: ChunkStream = Box::pin(map_responses_stream(byte_stream_of(bytes)));
        assert_chunk_stream_never_errs(
            stream,
            &format!("responses junk frame injected at boundary {i}"),
        )
        .await;
    }
}

#[tokio::test]
async fn responses_stream_survives_single_frame_corruption() {
    for i in 0..RESPONSES_FRAMES.len() {
        let mut frames: Vec<String> = RESPONSES_FRAMES.iter().map(|s| s.to_string()).collect();
        frames[i] = corrupt_json_payload(&frames[i]);
        let bytes = frames.concat().into_bytes();
        let stream: ChunkStream = Box::pin(map_responses_stream(byte_stream_of(bytes)));
        assert_chunk_stream_never_errs(stream, &format!("responses frame {i} corrupted")).await;
    }
}

// ---------------------------------------------------------------------------
// Messages (Anthropic Messages SSE — shared by anthropic/openrouter/ollama)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn messages_stream_survives_truncation_at_every_offset() {
    let bytes = MESSAGES_FRAMES.concat().into_bytes();
    for cut in 0..=bytes.len() {
        let stream: ChunkStream = map_messages_stream(byte_stream_of(bytes[..cut].to_vec()));
        assert_chunk_stream_never_errs(
            stream,
            &format!("messages truncated at byte {cut}/{}", bytes.len()),
        )
        .await;
    }
}

#[tokio::test]
async fn messages_stream_survives_junk_frame_injection() {
    for i in 0..=MESSAGES_FRAMES.len() {
        let mut frames: Vec<&str> = MESSAGES_FRAMES.to_vec();
        frames.insert(i, SSE_JUNK_FRAME);
        let bytes = frames.concat().into_bytes();
        let stream: ChunkStream = map_messages_stream(byte_stream_of(bytes));
        assert_chunk_stream_never_errs(
            stream,
            &format!("messages junk frame injected at boundary {i}"),
        )
        .await;
    }
}

/// Contractual name (A-37 Acceptance): flip/corrupt one frame within an otherwise-valid stream.
#[tokio::test]
async fn messages_stream_survives_single_frame_corruption() {
    for i in 0..MESSAGES_FRAMES.len() {
        let mut frames: Vec<String> = MESSAGES_FRAMES.iter().map(|s| s.to_string()).collect();
        frames[i] = corrupt_json_payload(&frames[i]);
        let bytes = frames.concat().into_bytes();
        let stream: ChunkStream = map_messages_stream(byte_stream_of(bytes));
        assert_chunk_stream_never_errs(stream, &format!("messages frame {i} corrupted")).await;
    }
}

// ---------------------------------------------------------------------------
// Bedrock (AWS binary event-stream framing over the Messages wire)
// ---------------------------------------------------------------------------

/// Contractual name (A-37 Acceptance): unlike the SSE codecs, bedrock's binary framing legitimately
/// still errors on truncation/corruption (a broken frame IS a real framing-integrity failure,
/// A-36) — the invariant here is classification, not absence of `Err`. Runs all three corruption
/// strategies (truncation at every offset, junk-frame injection at every boundary, single-frame
/// corruption) in one test since they share the same assertion helper and fixture.
#[tokio::test]
async fn bedrock_stream_errors_are_always_classified() {
    let base_frames: Vec<Vec<u8>> = BEDROCK_EVENTS.iter().map(|e| chunk_frame(e)).collect();
    let full: Vec<u8> = base_frames.concat();

    // Truncation: every byte offset.
    for cut in 0..=full.len() {
        assert_bedrock_errs_are_classified(
            full[..cut].to_vec(),
            &format!("bedrock truncated at {cut}/{}", full.len()),
        )
        .await;
    }

    // Junk-frame injection: a structurally-valid frame with a garbage `chunk` payload (the
    // tolerated Garbage path, not a declared exception) inserted at every frame boundary.
    let junk = bedrock_garbage_chunk_frame();
    for i in 0..=base_frames.len() {
        let mut frames = base_frames.clone();
        frames.insert(i, junk.clone());
        assert_bedrock_errs_are_classified(
            frames.concat(),
            &format!("bedrock junk frame injected at boundary {i}"),
        )
        .await;
    }

    // Single-frame corruption: flip one frame's last byte (invalidates that frame's message CRC
    // deterministically) in an otherwise-valid multi-frame stream, for every frame in turn.
    for i in 0..base_frames.len() {
        let mut frames = base_frames.clone();
        let last = frames[i].len() - 1;
        frames[i][last] ^= 0xFF;
        assert_bedrock_errs_are_classified(
            frames.concat(),
            &format!("bedrock frame {i} corrupted"),
        )
        .await;
    }
}

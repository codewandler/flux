//! The `aws` (AWS Bedrock) provider.
//!
//! Bedrock serves Anthropic models behind an AWS Signature-V4 gate. The load-bearing reuse:
//! `bedrock-runtime invoke-with-response-stream` on an Anthropic model streams **native Anthropic
//! Messages events** — the exact shape flux's [`crate::messages`] codec already produces and parses
//! — so the wire codec is a thin wrapper over [`build_messages_body`] that moves
//! `anthropic_version` from a header into the body (Bedrock's one wire quirk) and a deframer
//! ([`map_bedrock_event_stream`]) that unwraps AWS's binary event-stream framing into the SSE bytes
//! the shared [`map_messages_stream`] mapper parses.
//!
//! The credential side is **SigV4 signing** (hand-rolled here, ~150 lines, pinned by a known-answer
//! test) over an injected [`BedrockCredentialsResolver`] — the seam that lets the credential source
//! be swapped without touching L1. The shipped resolvers: [`EnvStaticResolver`] (reads
//! `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`/`AWS_REGION`) and
//! [`AwsChainResolver`] (the full env → SSO → IRSA → EKS Pod Identity chain, below). Resolved
//! creds are cached and **re-resolved through the resolver when they near expiry** (C-37) — the
//! chain's STS sessions rotate under a long-running process — and [`bedrock_with_chain`] builds
//! the chain-backed provider **sync + lazily** (first request resolves). See
//! `docs/designs/subscription-providers-and-cost.md` (consolidated providers & cost design).
//!
//! Layering: L1, no flux deps above L0/L1. Crypto (`sha2`/`hmac`) is pure; the credential reads
//! env (the established L1 pattern — `anthropic_from_env` reads `ANTHROPIC_API_KEY`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use futures::StreamExt;
// hmac 0.13 (digest 0.11): `new_from_slice` moved off `Mac` onto the re-exported `KeyInit`.
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::messages::{
    anthropic_model_caps, build_messages_body, map_messages_stream, MessagesQuirks, ProviderProfile,
};
use flux_core::{Error, Result};
use flux_provider::{ByteStream, ChunkStream, Credential, NativeProvider, Request, WireCodec};

type HmacSha256 = Hmac<Sha256>;

const BEDROCK_ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const SIGV4_SERVICE: &str = "bedrock";
const SIGV4_TERMINATOR: &str = "aws4_request";
const SIGV4_ALGO: &str = "AWS4-HMAC-SHA256";

/// Serializes tests that inspect or mutate the process-global AWS environment across both the
/// Bedrock owner module and the shared spec factory tests.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Quirks profile
// ---------------------------------------------------------------------------

/// Bedrock passes the full Anthropic Messages feature set through to the same backend — the same
/// profile as Anthropic-direct, including the per-model capability gating (C-49):
/// [`anthropic_model_caps`] understands inference-profile ids
/// (`global.anthropic.claude-haiku-4-5-20251001-v1:0`), so `aws/haiku` no longer sends adaptive
/// thinking to a model that rejects it.
pub struct BedrockProfile;

impl ProviderProfile for BedrockProfile {
    fn quirks_for(&self, model: &str) -> MessagesQuirks {
        let caps = anthropic_model_caps(model);
        MessagesQuirks {
            prompt_caching: true,
            thinking_adaptive: caps.adaptive_thinking,
            effort_output_config: caps.effort,
            sampling_params: caps.sampling_params,
            extra_body: Default::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// The Bedrock Anthropic wire protocol: `POST /model/{modelId}/invoke-with-response-stream`, body
/// is the Anthropic Messages shape with `anthropic_version: "bedrock-2023-05-31"` in the body (not
/// a header), response is an AWS binary event-stream whose chunk payloads are native Messages
/// streaming events.
pub struct BedrockAnthropic;

impl WireCodec for BedrockAnthropic {
    fn build_body(&self, req: &Request) -> Result<Value> {
        let mut body = build_messages_body(req, &BedrockProfile.quirks_for(&req.model))?;
        // Bedrock's one wire quirk: the version lives in the body, not a header.
        body["anthropic_version"] = json!(BEDROCK_ANTHROPIC_VERSION);
        // Bedrock rejects `model` (it's in the URL path via the model id) and `stream` (streaming
        // is expressed by the URL, not a body flag). Both are emitted by the shared Messages body
        // builder; strip them.
        body.as_object_mut()
            .expect("messages body is an object")
            .remove("model");
        body.as_object_mut()
            .expect("messages body is an object")
            .remove("stream");
        Ok(body)
    }

    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        map_messages_stream(map_bedrock_event_stream(bytes))
    }

    fn wire_headers(&self) -> Vec<(&'static str, String)> {
        // NO `anthropic-version` header — it's in the body. Auth headers are set by the credential.
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// AWS event-stream → Anthropic SSE (C-09d)
// ---------------------------------------------------------------------------
//
// `invoke-with-response-stream` responds with `application/vnd.amazon.eventstream`: binary frames
// of `[total_len u32 | headers_len u32 | prelude_crc u32 | headers | payload | message_crc u32]`
// (big-endian, CRC-32/IEEE). Each `chunk` event's payload is `{"bytes":"<base64>"}` where the
// decoded bytes are one native Anthropic Messages streaming event (`message_start`,
// `content_block_delta`, ...) — Bedrock's stream is the Anthropic stream, wrapped in AWS framing.
// The deframer re-frames each event as SSE (`event: <type>\ndata: <json>\n\n`) so the shared
// [`map_messages_stream`] parses it unchanged.

/// The smallest possible frame: a 12-byte prelude + a 4-byte message CRC (no headers, no payload).
const MIN_FRAME_LEN: usize = 16;
/// Corrupt-length guard — no legitimate Bedrock frame (a model delta) approaches this.
const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// CRC-32/IEEE (reflected, poly `0xEDB88320`) — the checksum AWS event-stream frames carry.
/// Bitwise (no table): frames are small. Pinned by the standard check value
/// (`crc32(b"123456789") == 0xCBF43926`).
fn crc32(data: &[u8]) -> u32 {
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

fn read_u32_be(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

/// The total length of the frame at the head of `buf` once it is fully buffered (`None` = need
/// more bytes). Validates the prelude (length bounds + CRC) as soon as its 12 bytes arrive, so
/// corrupt framing fails fast instead of stalling on a bogus length.
fn buffered_frame_len(buf: &[u8]) -> Result<Option<usize>> {
    if buf.len() < 12 {
        return Ok(None);
    }
    let total = read_u32_be(buf, 0) as usize;
    if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&total) {
        // A-36: an implausible length prefix is a framing-integrity failure (the underlying
        // event-stream framing is broken), not one junk payload — classify `StreamDecode` so the
        // A-33 backstop retries the call instead of killing the turn.
        return Err(Error::StreamDecode(format!(
            "bedrock event-stream: implausible frame length {total}"
        )));
    }
    if read_u32_be(buf, 8) != crc32(&buf[..8]) {
        return Err(Error::StreamDecode(
            "bedrock event-stream: prelude CRC mismatch".to_string(),
        ));
    }
    Ok((buf.len() >= total).then_some(total))
}

/// Parse an event-stream header block into (name, value) pairs. The routing headers Bedrock sends
/// (`:message-type`, `:event-type`, `:exception-type`, `:content-type`) are all strings (type 7);
/// non-string value types are length-skipped.
fn parse_event_headers(block: &[u8]) -> Result<Vec<(String, String)>> {
    // A-36: every error in this function is a header-block *structure* failure (truncation, a
    // non-utf8 name/value, an unknown value-type tag) — genuine framing-integrity breakage, not a
    // junk payload. Classify `StreamDecode` so the A-33 backstop retries the call.
    fn take<'a>(b: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
        if b.len() < n {
            return Err(Error::StreamDecode(
                "bedrock event-stream: truncated header block".to_string(),
            ));
        }
        let (head, tail) = b.split_at(n);
        *b = tail;
        Ok(head)
    }
    let mut b = block;
    let mut out = Vec::new();
    while !b.is_empty() {
        let name_len = take(&mut b, 1)?[0] as usize;
        let name = std::str::from_utf8(take(&mut b, name_len)?)
            .map_err(|_| {
                Error::StreamDecode("bedrock event-stream: non-utf8 header name".to_string())
            })?
            .to_string();
        let value_type = take(&mut b, 1)?[0];
        match value_type {
            0 | 1 => {}                      // bool true / false — no value bytes
            2 => drop(take(&mut b, 1)?),     // byte
            3 => drop(take(&mut b, 2)?),     // short
            4 => drop(take(&mut b, 4)?),     // int
            5 | 8 => drop(take(&mut b, 8)?), // long / timestamp
            9 => drop(take(&mut b, 16)?),    // uuid
            6 | 7 => {
                // byte-buffer / string: u16 length + bytes.
                let len =
                    u16::from_be_bytes(take(&mut b, 2)?.try_into().expect("2 bytes")) as usize;
                let value = take(&mut b, len)?;
                if value_type == 7 {
                    let v = std::str::from_utf8(value).map_err(|_| {
                        Error::StreamDecode(
                            "bedrock event-stream: non-utf8 header value".to_string(),
                        )
                    })?;
                    out.push((name, v.to_string()));
                }
            }
            t => {
                return Err(Error::StreamDecode(format!(
                    "bedrock event-stream: unknown header value type {t}"
                )))
            }
        }
    }
    Ok(out)
}

/// The outcome of mapping one complete frame (A-36).
enum FrameOutcome {
    /// Emit this synthesized SSE event downstream.
    Sse(String),
    /// A non-chunk event with nothing to emit (tolerated — e.g. a future Bedrock event type).
    Skip,
    /// A `chunk` event whose *payload* failed to decode (not valid JSON / the `bytes` wrapper's
    /// base64 doesn't decode / the decoded bytes aren't UTF-8) — one AWS-level chunk was junk. The
    /// caller skips it, counts it, and warns; deframing continues with the next frame.
    Garbage(String),
}

/// Map one complete frame to a [`FrameOutcome`]. A `chunk` event's payload is
/// `{"bytes":"<base64 of one Anthropic stream event>"}` (plus a `p` padding field, ignored) —
/// payload-level decode failures become [`FrameOutcome::Garbage`] (tolerated). `exception`/`error`
/// frames are a **declared** failure reported by AWS/the model (not garbage bytes) and stay fatal
/// via `Error::Provider`, unchanged from before A-36 — do not reclassify this path. Genuine
/// framing-integrity failures (message CRC mismatch, a header block that overruns the frame, a
/// frame missing `:message-type`) surface as `Error::StreamDecode` so the A-33 planner backstop
/// retries the call instead of killing the turn.
// A-37: the three `serde_json::from_slice`/`from_str` calls in this fn are the tolerant
// chunk-payload parse (→ `FrameOutcome::Garbage`, never fatal) and the best-effort event-type/
// message-text extraction (`.ok()`, falls back on failure) — allowed at this tight scope;
// `envelope_corpus.rs` proves the tolerance holds.
#[allow(clippy::disallowed_methods)]
fn frame_to_sse(frame: &[u8]) -> Result<FrameOutcome> {
    let total = frame.len();
    if read_u32_be(frame, total - 4) != crc32(&frame[..total - 4]) {
        return Err(Error::StreamDecode(
            "bedrock event-stream: message CRC mismatch".to_string(),
        ));
    }
    let headers_len = read_u32_be(frame, 4) as usize;
    let headers_end = 12usize
        .checked_add(headers_len)
        .filter(|&e| e <= total - 4)
        .ok_or_else(|| {
            Error::StreamDecode("bedrock event-stream: header block overruns frame".to_string())
        })?;
    let headers = parse_event_headers(&frame[12..headers_end])?;
    let payload = &frame[headers_end..total - 4];
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };

    match header(":message-type") {
        Some("event") if header(":event-type") == Some("chunk") => {
            let wrapper: Value = match serde_json::from_slice(payload) {
                Ok(v) => v,
                Err(e) => {
                    return Ok(FrameOutcome::Garbage(format!(
                        "chunk payload is not JSON ({e})"
                    )))
                }
            };
            let b64 = match wrapper.get("bytes").and_then(|v| v.as_str()) {
                Some(b) => b,
                None => return Ok(FrameOutcome::Garbage("chunk without `bytes`".to_string())),
            };
            let event_bytes = match BASE64.decode(b64) {
                Ok(b) => b,
                Err(e) => {
                    return Ok(FrameOutcome::Garbage(format!(
                        "chunk bytes are not base64 ({e})"
                    )))
                }
            };
            let event_json = match String::from_utf8(event_bytes) {
                Ok(s) => s,
                Err(_) => {
                    return Ok(FrameOutcome::Garbage(
                        "chunk bytes are not utf8".to_string(),
                    ))
                }
            };
            // The decoded bytes are one Anthropic Messages stream event; its `type` becomes the
            // SSE event name (the shared mapper dispatches on the JSON `type` tag anyway).
            let event_type = serde_json::from_str::<Value>(&event_json)
                .ok()
                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
                .unwrap_or_else(|| "message".to_string());
            Ok(FrameOutcome::Sse(format!(
                "event: {event_type}\ndata: {event_json}\n\n"
            )))
        }
        Some("event") => Ok(FrameOutcome::Skip), // no other event types today; tolerate future additions
        Some(other) => {
            // `exception` / `error` frames: a DECLARED failure reported by AWS/the model — the
            // payload is `{"message": "..."}`. Stays fatal via `Error::Provider`, exactly as
            // before A-36: this is a real failure being reported by the provider, not garbage
            // bytes, and must not be reclassified (or masked as a retryable decode error).
            let kind = header(":exception-type")
                .or_else(|| header(":error-code"))
                .unwrap_or(other);
            let message = serde_json::from_slice::<Value>(payload)
                .ok()
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                .unwrap_or_else(|| String::from_utf8_lossy(payload).into_owned());
            Err(Error::Provider(format!("bedrock stream {kind}: {message}")))
        }
        None => Err(Error::StreamDecode(
            "bedrock event-stream: frame without :message-type".to_string(),
        )),
    }
}

/// Decode an AWS binary event-stream into Anthropic SSE bytes. Stateful: frames arrive split
/// across arbitrary HTTP chunk boundaries, so bytes accumulate until a complete frame is buffered.
///
/// A-36 draws two DIFFERENT lines here. A **truncated tail** (connection cut mid-frame) or any
/// other framing-**integrity** failure (CRC mismatch, a header block overrunning the buffer) means
/// the AWS event-stream framing itself is broken — silently dropping it would swallow a
/// `PayloadPart`, so it stays an error, just classified `Error::StreamDecode` so the A-33 planner
/// backstop retries the call instead of killing the turn. A single `chunk` event's **payload**
/// being garbage (not JSON / bad base64 / not UTF-8) is a different, lower-stakes failure — "one
/// AWS-level chunk was junk" — so it is skipped, counted, and warned, and deframing continues.
/// This layer emits raw bytes (not `Chunk`s), so there's no `StreamDiagnostic` to surface here;
/// the downstream Messages codec's own diagnostics (A-35) cover content-level accounting.
pub fn map_bedrock_event_stream(byte_stream: ByteStream) -> ByteStream {
    Box::pin(async_stream::try_stream! {
        let mut s = byte_stream;
        let mut buf: Vec<u8> = Vec::new();
        let mut dropped_frames: u32 = 0;
        while let Some(chunk) = s.next().await {
            buf.extend_from_slice(&chunk?);
            while let Some(len) = buffered_frame_len(&buf)? {
                let frame: Vec<u8> = buf.drain(..len).collect();
                match frame_to_sse(&frame)? {
                    FrameOutcome::Sse(sse) => yield bytes::Bytes::from(sse),
                    FrameOutcome::Skip => {}
                    FrameOutcome::Garbage(detail) => {
                        dropped_frames += 1;
                        tracing::warn!(
                            dropped_frames,
                            detail = %detail,
                            "bedrock event-stream: skipping unparseable chunk payload"
                        );
                    }
                }
            }
        }
        if !buf.is_empty() {
            Err(Error::StreamDecode(format!(
                "bedrock event-stream: {} trailing bytes (truncated frame)",
                buf.len()
            )))?;
        }
    })
}

// ---------------------------------------------------------------------------
// Credentials — injected resolver + hand-rolled SigV4
// ---------------------------------------------------------------------------

/// Resolved AWS credentials for a Bedrock request. The chain (SSO / IRSA / EKS Pod Identity / IMDS
/// / static) is the resolver's concern; this is the neutral carrier handed to [`sign_v4`].
#[derive(Clone)]
pub struct BedrockCreds {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    pub region: String,
    /// When these credentials stop working (STS session expiry). `None` for sources that don't
    /// report one (static env keys) — such creds are never considered stale (C-37).
    pub expiration: Option<DateTime<Utc>>,
}

/// How close to expiry credentials are still considered usable. Within this window `apply()`
/// re-resolves through the stored resolver instead of signing with soon-dead creds (C-37) —
/// the same 5-minute guard AWS SDKs use for their credential caches.
const BEDROCK_CREDS_EXPIRY_WINDOW_SECS: i64 = 300;

/// Are these creds absent-of-expiry-fresh? Stale = an expiration exists and lies within the
/// [`BEDROCK_CREDS_EXPIRY_WINDOW_SECS`] buffer from now.
fn creds_stale(creds: &BedrockCreds) -> bool {
    match creds.expiration {
        Some(at) => (at - Utc::now()).num_seconds() <= BEDROCK_CREDS_EXPIRY_WINDOW_SECS,
        None => false,
    }
}

/// Parse an ISO-8601/RFC-3339 expiration stamp (the STS / Pod Identity wire form, `Z` suffix and
/// fractional seconds tolerated). Unparseable input degrades to `None` — never-refresh, today's
/// behavior — rather than erroring a working credential path.
fn parse_expiration_iso(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Resolves AWS credentials + region for a Bedrock request. The seam: implemented by
/// [`EnvStaticResolver`] (the shipped stand-in) and, per the design's Option C, by an `aws-bedrock`
/// plugin resolver that embeds `aws-config`. Swappable at one trait — L1 never knows the source.
#[async_trait]
pub trait BedrockCredentialsResolver: Send + Sync {
    async fn resolve(&self) -> Result<BedrockCreds>;
    /// Force a refresh (the C-04 401 path). The default is a no-op for sources that can't.
    async fn refresh(&self) -> Result<()> {
        Ok(())
    }
}

/// The shipped stand-in resolver: reads `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` /
/// `AWS_SESSION_TOKEN` / `AWS_REGION` (or `AWS_DEFAULT_REGION`). Drives the dev account via
/// `aws configure export-credentials --profile <p> --format env` (materialized into env) until the
/// `aws-bedrock` plugin (Option C) lands; the plugin resolver replaces this at the
/// [`BedrockCredentialsResolver`] seam, not in L1.
pub struct EnvStaticResolver;

/// Read AWS static credentials + region from the environment. Shared by [`EnvStaticResolver`] (the
/// async trait path, for refresh) and [`bedrock_with_env`] (the sync construction path, so the CLI's
/// sync `build_provider` can build an `aws` provider without an async runtime). Pure env reads —
/// the established L1 pattern.
fn creds_from_env() -> Result<BedrockCreds> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID")
        .map_err(|_| Error::Auth("AWS_ACCESS_KEY_ID is not set".to_string()))?;
    if access_key.trim().is_empty() {
        return Err(Error::Auth("AWS_ACCESS_KEY_ID is empty".to_string()));
    }
    let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .map_err(|_| Error::Auth("AWS_SECRET_ACCESS_KEY is not set".to_string()))?;
    let session_token = std::env::var("AWS_SESSION_TOKEN")
        .ok()
        .filter(|s| !s.is_empty());
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string());
    Ok(BedrockCreds {
        access_key,
        secret_key,
        session_token,
        region,
        expiration: None,
    })
}

#[async_trait]
impl BedrockCredentialsResolver for EnvStaticResolver {
    async fn resolve(&self) -> Result<BedrockCreds> {
        creds_from_env()
    }
}

/// A Bedrock [`Credential`]: holds the model id + a creds cache and signs each request with SigV4.
/// The **region is pinned at construction** (eager constructors pin it from the first resolved
/// creds; the lazy [`bedrock_with_chain`] pins it from `AWS_REGION`/`AWS_DEFAULT_REGION`) so
/// `endpoint()` is answerable before any resolution, and every resolved cred is coerced to the pin
/// so the URL host and the SigV4 credential scope always agree. Creds start `None` for the lazy
/// constructor and are (re-)resolved by [`apply`](Credential::apply) whenever absent or within the
/// expiry window (C-37) — temporary STS sessions (SSO / IRSA / EKS Pod Identity) rotate under a
/// long-running process, so resolve-once would go dark at the first expiry.
pub struct BedrockCredential {
    model_id: String,
    /// The region requests are routed to and signed against — pinned for the credential's life.
    region: String,
    /// The resolved-creds cache. `None` until the first request for the lazy constructor.
    creds: Mutex<Option<BedrockCreds>>,
    /// Re-resolves the chain when the cache is empty or stale (C-37).
    resolver: Arc<dyn BedrockCredentialsResolver>,
}

impl BedrockCredential {
    fn endpoint_url(&self) -> String {
        // Streaming invoke. The model id may contain `.`/`:`/`-` — percent-encode the path
        // segment (not the slashes between segments).
        let encoded = percent_encode_segment(&self.model_id);
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke-with-response-stream",
            self.region, encoded
        )
    }

    /// The cached creds if still fresh, else a re-resolve through the stored resolver. The
    /// resolve runs **outside** the (std) creds lock — it can't be held across `.await` — so
    /// concurrent turns may race a duplicate resolve; that's benign (the chain sources are
    /// idempotent local reads / metadata GETs) and both results are valid.
    async fn fresh_creds(&self) -> Result<BedrockCreds> {
        {
            let cached = self.creds.lock().expect("bedrock creds mutex poisoned");
            if let Some(c) = cached.as_ref() {
                if !creds_stale(c) {
                    return Ok(c.clone());
                }
            }
        }
        let mut resolved = self.resolver.resolve().await?;
        // Coerce to the pinned region: `endpoint()` already routed there, and the SigV4 scope
        // must match the URL host. (STS-family creds are region-agnostic.)
        resolved.region = self.region.clone();
        *self.creds.lock().expect("bedrock creds mutex poisoned") = Some(resolved.clone());
        Ok(resolved)
    }
}

#[async_trait]
impl Credential for BedrockCredential {
    fn endpoint(&self) -> String {
        self.endpoint_url()
    }

    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let creds = self.fresh_creds().await?;
        sign_v4(rb, &creds, self.model_id.clone())
    }
}

// ---------------------------------------------------------------------------
// SigV4 — hand-rolled, pinned by a known-answer test
// ---------------------------------------------------------------------------

/// Percent-encode a single URL path segment per RFC 3986 (unreserved `A-Za-z0-9-._~` preserved),
/// encoding everything else (including `/` so a model id can't inject path segments). AWS SigV4
/// canonical-URI uses the same encoding for non-S3 services.
fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Hex-encode bytes (lowercase) — the format SigV4 uses for all hashes.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex_lower(&h.finalize())
}

/// Sign a `reqwest::RequestBuilder` with AWS Signature-V4 (service `bedrock`) and return it with the
/// `Authorization`, `x-amz-date`, `x-amz-content-sha256`, and (if present) `x-amz-security-token`
/// headers attached. The body is probed via `try_clone().build()` (the standard way to read a
/// `RequestBuilder`'s body without consuming it); all headers already on the builder (e.g.
/// `content-type`) are folded into the canonical headers so the signature matches what's sent.
fn sign_v4(
    rb: reqwest::RequestBuilder,
    creds: &BedrockCreds,
    _model_id: String,
) -> Result<reqwest::RequestBuilder> {
    // Probe the request to read method/url/headers/body without consuming the builder.
    let probe = rb
        .try_clone()
        .ok_or_else(|| Error::Auth("bedrock: request body is not cloneable for signing".into()))?;
    let req = probe
        .build()
        .map_err(|e| Error::Auth(format!("bedrock: build request: {e}")))?;
    let method = req.method().as_str();
    let url = req.url();
    let host = url
        .host_str()
        .ok_or_else(|| Error::Auth("bedrock: endpoint has no host".into()))?;
    let body_bytes: Vec<u8> = req
        .body()
        .and_then(|b| b.as_bytes())
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let payload_hash = sha256_hex(&body_bytes);

    // Timestamps.
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    // Canonical headers: collect everything already on the builder + the AWS headers we'll add.
    // (Lowercase names, trimmed values, sorted by name.)
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in req.headers().iter() {
        let n = name.as_str().to_lowercase();
        // Skip hop-by-hop / auto headers the client computes — they aren't stable across the probe
        // and the real send, and SigV4 doesn't require them.
        if matches!(
            n.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        let v = value
            .to_str()
            .map_err(|_| Error::Auth("bedrock: non-ascii header value".into()))?
            .trim()
            .to_string();
        headers.push((n, v));
    }
    headers.push(("host".to_string(), host.to_string()));
    headers.push(("x-amz-content-sha256".to_string(), payload_hash.clone()));
    headers.push(("x-amz-date".to_string(), amz_date.clone()));
    if let Some(tok) = &creds.session_token {
        headers.push(("x-amz-security-token".to_string(), tok.clone()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers: String = headers.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_headers: String = headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");

    // Canonical URI: the encoded path. Bedrock paths are `/model/{id}/invoke`; encode each segment.
    let canonical_uri = if url.path().is_empty() {
        "/".to_string()
    } else {
        url.path()
            .split('/')
            .map(|seg| {
                if seg.is_empty() {
                    String::new()
                } else {
                    percent_encode_segment(seg)
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    };

    // Canonical query string: sorted by key, encoded.
    let mut q: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (percent_encode_segment(&k), percent_encode_segment(&v)))
        .collect();
    q.sort();
    let canonical_query = q
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let canonical_hash = sha256_hex(canonical_request.as_bytes());

    let credential_scope = format!(
        "{date_stamp}/{region}/{service}/{terminator}",
        date_stamp = date_stamp,
        region = creds.region,
        service = SIGV4_SERVICE,
        terminator = SIGV4_TERMINATOR
    );
    let string_to_sign = format!(
        "{algo}\n{amz_date}\n{scope}\n{hash}",
        algo = SIGV4_ALGO,
        amz_date = amz_date,
        scope = credential_scope,
        hash = canonical_hash
    );

    // Signing key: AWS4-HMAC-SHA256 chain (date → region → service → "aws4_request").
    let k_date = hmac_key(
        format!("AWS4{}", creds.secret_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_key(&k_date, creds.region.as_bytes());
    let k_service = hmac_key(&k_region, SIGV4_SERVICE.as_bytes());
    let k_signing = hmac_key(&k_service, SIGV4_TERMINATOR.as_bytes());
    let signature = hex_lower(&hmac_bytes(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "{algo} Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        algo = SIGV4_ALGO,
        access_key = creds.access_key,
        scope = credential_scope,
        signed_headers = signed_headers,
        signature = signature
    );

    Ok(rb
        .header("x-amz-content-sha256", &payload_hash)
        .header("x-amz-date", &amz_date)
        .header("authorization", authorization)
        .header(
            "x-amz-security-token",
            creds.session_token.clone().unwrap_or_default(),
        ))
}

fn hmac_key(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut h = HmacSha256::new_from_slice(key).expect("hmac key length");
    h.update(msg);
    h.finalize().into_bytes().to_vec()
}

fn hmac_bytes(key: &[u8], msg: &[u8]) -> Vec<u8> {
    hmac_key(key, msg)
}

// ---------------------------------------------------------------------------
// Model resolution
// ---------------------------------------------------------------------------

/// Resolve a flux alias to a Bedrock model id. Cross-region inference-profile ids (`us.`/`global.`)
/// are the only form newer Claude 4/5 models accept on-demand (direct foundation-model ids are
/// rejected); legacy claude-3 ids are marked "Legacy" and not aliased to.
/// Resolve a flux model alias to a Bedrock inference-profile id. Region-aware: the cross-region
/// inference-profile prefix (`us.`/`eu.`/`global.`) must match the Bedrock region the request
/// targets, or Bedrock returns 400 "The provided model identifier is invalid" (the profile isn't
/// registered in that region's catalog). `global.` profiles (haiku) work in every region; `us.`/
/// `eu.` profiles are region-specific. The region is read from `AWS_REGION`/`AWS_DEFAULT_REGION`
/// (set by the credential chain before this runs) and defaults to `us` for unknown regions.
pub fn resolve_model(alias: &str) -> String {
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_default();
    resolve_model_for_region(alias, &region)
}

/// Resolve an alias for an explicitly pinned request region. The public model-spec factory uses
/// this path when a caller injects a credential resolver, so model selection and the credential's
/// endpoint cannot drift through ambient `AWS_REGION` state.
pub(crate) fn resolve_model_for_region(alias: &str, region: &str) -> String {
    let prefix = region_prefix(region);
    match alias {
        // The active Claude 4-6 generation. Region-prefixed (us./eu.) — both exist in their
        // respective region catalogs. `global.` is not available for sonnet-4-6 in all regions.
        "" | "sonnet" => format!("{prefix}.anthropic.claude-sonnet-4-6"),
        "opus" => format!("{prefix}.anthropic.claude-opus-4-6-v1"),
        // Haiku is a `global.` profile (works in every region) by default — don't region-prefix it
        // unless overridden (see `haiku_profile_prefix`): some IAM setups only grant
        // `bedrock:InvokeModel`/`InvokeModelWithResponseStream` on region-specific inference
        // profiles, not `global.*` ones, and would otherwise have to fork the whole alias table
        // just to keep haiku regional.
        "haiku" => format!(
            "{}.anthropic.claude-haiku-4-5-20251001-v1:0",
            haiku_profile_prefix()
        ),
        other => other.to_string(),
    }
}

/// The cross-region inference-profile prefix used for the `haiku` alias. Defaults to `global`
/// (today's behavior — a global profile, valid in every region); set `FLUX_BEDROCK_HAIKU_PROFILE`
/// to override it — e.g. to `us`/`eu` to pin haiku to the same region-specific prefix `sonnet`/
/// `opus` already use, for an IAM setup without `global.*` inference-profile access. Read the same
/// way every other Bedrock knob in this module is (a plain `std::env::var` with a default), so it
/// composes with `AWS_REGION`/`AWS_PROFILE`/… without any new configuration mechanism.
fn haiku_profile_prefix() -> String {
    match std::env::var("FLUX_BEDROCK_HAIKU_PROFILE") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => "global".to_string(),
    }
}

/// Map an AWS region to its cross-region inference-profile prefix. `eu-*` → `eu`, everything else
/// (including `us-*` and unknown) → `us`.
fn region_prefix(region: &str) -> &'static str {
    if region.starts_with("eu-") {
        "eu"
    } else {
        "us"
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// Build an `aws` Bedrock provider explicitly from static `AWS_*` environment credentials. Reads
/// env once via [`creds_from_env`]; the stored [`EnvStaticResolver`] backs the C-04 refresh path.
/// The shared model-spec factory deliberately uses [`bedrock_with_chain`] instead, so temporary
/// SSO/IRSA credentials stay lazy and refreshable rather than being snapshotted into env.
pub fn bedrock_with_env(model_id: String) -> Result<NativeProvider> {
    let creds = creds_from_env()?;
    Ok(NativeProvider::new(
        "aws",
        Arc::new(BedrockAnthropic),
        Arc::new(BedrockCredential {
            model_id,
            region: creds.region.clone(),
            creds: Mutex::new(Some(creds)),
            resolver: Arc::new(EnvStaticResolver),
        }),
    ))
}

/// Build the `aws` Bedrock provider from the injected credential resolver (async — resolves the
/// chain once). The Option C plugin resolver plugs in here.
pub async fn bedrock_with(
    model_id: String,
    resolver: Arc<dyn BedrockCredentialsResolver>,
) -> Result<NativeProvider> {
    let creds = resolver.resolve().await?;
    Ok(NativeProvider::new(
        "aws",
        Arc::new(BedrockAnthropic),
        Arc::new(BedrockCredential {
            model_id,
            region: creds.region.clone(),
            creds: Mutex::new(Some(creds)),
            resolver,
        }),
    ))
}

// ---------------------------------------------------------------------------
// AWS credential chain (C-09b) — env → SSO → IRSA → EKS Pod Identity
// ---------------------------------------------------------------------------
//
// Resolves `BedrockCreds` from the full AWS default chain WITHOUT an `aws` CLI binary — the same
// sources `aws-config` walks, hand-rolled over direct `std::fs` + `reqwest`. This follows the
// established flux-credentials precedent: the credential-bootstrap path (reading `~/.flux/...` or
// `~/.aws/...` token caches, refreshing OAuth/SSO tokens) is a separate trust boundary from the
// agent-tool IO path (which goes through `flux_system::net::guard`). flux-credentials already reads
// `~/.flux/credentials.toml` via `std::fs` and refreshes OAuth tokens via `reqwest` directly; the
// AWS chain does the same for `~/.aws/sso/cache` and STS/SSO-OIDC.
//
// Sources tried in order (first wins): (1) static env (`AWS_ACCESS_KEY_ID` + friends); (2) SSO —
// reads `~/.aws/config` for the profile's `sso_session`/`sso_account_id`/`sso_role_name`, reads the
// cached access token from `~/.aws/sso/cache/<sha1(session)>.json`, refreshs it via SSO-OIDC
// `CreateToken` if expired, then calls `sso:GetRoleCredentials`; (3) IRSA (k8s web-identity) —
// `AWS_ROLE_ARN` + `AWS_WEB_IDENTITY_TOKEN_FILE` → `sts:AssumeRoleWithWebIdentity`; (4) EKS Pod
// Identity — `AWS_CONTAINER_CREDENTIALS_FULL_URI` → HTTP GET. IMDS (EC2 instance role) is not
// implemented (flux doesn't run on bare EC2); add it if ever needed.

/// Resolve AWS credentials via the default chain (env → SSO → IRSA → EKS Pod Identity), without an
/// `aws` CLI. The async-aware path used by the CLI's `aws` provider arm; falls back to static env
/// (the sync `bedrock_with_env` path) when the chain has nothing.
pub async fn resolve_default_chain() -> Result<BedrockCreds> {
    // 1. Static env (fast path — covers prod with env-injected creds and `aws configure
    // export-credentials` materialized into env).
    if let Ok(c) = creds_from_env() {
        return Ok(c);
    }
    // 2. SSO (dev laptop — `aws sso login` once, then this reads the cached token + refreshes).
    if let Some(c) = resolve_sso().await? {
        return Ok(c);
    }
    // 3. IRSA (k8s — webhook injects AWS_ROLE_ARN + AWS_WEB_IDENTITY_TOKEN_FILE).
    if let Some(c) = resolve_irsa().await? {
        return Ok(c);
    }
    // 4. EKS Pod Identity (k8s — webhook injects AWS_CONTAINER_CREDENTIALS_FULL_URI).
    if let Some(c) = resolve_eks_pod_identity().await? {
        return Ok(c);
    }
    Err(Error::Auth(
        "no AWS credentials: set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, run `aws sso login` \
         (with AWS_PROFILE), or inject IRSA/EKS-Pod-Identity env"
            .to_string(),
    ))
}

/// Explicit compatibility helper that resolves the default chain and materializes it into `AWS_*`
/// env vars (idempotent when `AWS_ACCESS_KEY_ID` is already set). Production provider construction
/// does not call this: [`crate::spec::build`] uses [`bedrock_with_chain`], keeping temporary creds
/// lazy, cached, and expiry-refreshable. Callers that opt into materialization own its process-wide
/// mutation and snapshot semantics.
pub async fn materialize_chain_into_env() -> Result<()> {
    // Idempotent: if env is already populated (e.g. `aws configure export-credentials` ran, or
    // prod injected env creds), don't re-resolve / overwrite.
    if std::env::var("AWS_ACCESS_KEY_ID")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }
    let creds = resolve_default_chain().await?;
    std::env::set_var("AWS_ACCESS_KEY_ID", &creds.access_key);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", &creds.secret_key);
    if let Some(t) = &creds.session_token {
        std::env::set_var("AWS_SESSION_TOKEN", t);
    }
    std::env::set_var("AWS_REGION", &creds.region);
    Ok(())
}

/// A [`BedrockCredentialsResolver`] backed by the default chain ([`resolve_default_chain`]). Stored
/// in [`BedrockCredential`] for the C-04 401-refresh path (re-resolves the chain on refresh).
pub struct AwsChainResolver;

#[async_trait]
impl BedrockCredentialsResolver for AwsChainResolver {
    async fn resolve(&self) -> Result<BedrockCreds> {
        resolve_default_chain().await
    }
}

/// Build the `aws` Bedrock provider from the default credential chain (async — resolves SSO/IRSA).
/// The full-chain counterpart to [`bedrock_with_env`] (env-only, sync): tries env → SSO → IRSA →
/// EKS Pod Identity, stores the [`AwsChainResolver`] for the 401-refresh path.
pub async fn bedrock_with_default_chain(model_id: String) -> Result<NativeProvider> {
    bedrock_with(model_id, Arc::new(AwsChainResolver)).await
}

/// Build the `aws` Bedrock provider from the default chain, **sync + lazy** (C-37): construction
/// resolves nothing — the first request's `apply()` walks the chain, and every later request
/// re-resolves whenever the cached creds near expiry. This is the constructor for sync call sites
/// (sub-agent provider factories, server startup paths) that want chain-backed credentials
/// without async plumbing and without the [`materialize_chain_into_env`] snapshot (which freezes
/// temporary creds into env, where chain step 1 re-reads them forever — defeating refresh).
///
/// The request **region is pinned here** from `AWS_REGION`/`AWS_DEFAULT_REGION` (default
/// `us-east-1`) — set it explicitly; an SSO profile's `region` does not apply to this
/// constructor. Credential misconfiguration surfaces as a per-request auth error, not a
/// construction failure.
pub fn bedrock_with_chain(model_id: String) -> NativeProvider {
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string());
    bedrock_with_lazy_resolver(model_id, region, Arc::new(AwsChainResolver))
}

/// Assemble a lazy Bedrock provider from an explicit region and resolver. Kept crate-private so
/// public callers use [`crate::spec::build_with_bedrock_resolver`], preserving model-spec parsing
/// and alias resolution instead of constructing credentials directly.
pub(crate) fn bedrock_with_lazy_resolver(
    model_id: String,
    region: String,
    resolver: Arc<dyn BedrockCredentialsResolver>,
) -> NativeProvider {
    NativeProvider::new(
        "aws",
        Arc::new(BedrockAnthropic),
        Arc::new(BedrockCredential {
            model_id,
            region,
            creds: Mutex::new(None),
            resolver,
        }),
    )
}

// --- minimal `~/.aws/config` INI parser -------------------------------------

/// A parsed `~/.aws/config`: section → `key → value`. Sections are `[default]`, `[profile <name>]`,
/// `[sso-session <name>]`. Keys are lowercased; values trimmed. A tiny INI subset (no nesting,
// no escapes) — the AWS config format is flat `key = value` under bracketed sections.
fn parse_aws_config(
    text: &str,
) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    let mut out: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    let mut cur = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            cur = s.trim().to_string();
            out.entry(cur.clone()).or_default();
        } else if let Some((k, v)) = line.split_once('=') {
            out.entry(cur.clone())
                .or_default()
                .insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    out
}

/// The section name for a profile in `~/.aws/config`: `"default"` for `[default]`, `"profile <name>"`
/// for `[profile <name>]`.
fn profile_section(profile: &str) -> String {
    if profile == "default" {
        "default".to_string()
    } else {
        format!("profile {profile}")
    }
}

/// `$HOME/.aws/config` (or `AWS_CONFIG_FILE`).
fn aws_config_path() -> Result<std::path::PathBuf> {
    if let Ok(p) = std::env::var("AWS_CONFIG_FILE") {
        return Ok(std::path::PathBuf::from(p));
    }
    Ok(home_dir()?.join(".aws").join("config"))
}

/// `$HOME/.aws/sso/cache/<sha1(hex)>.json`.
fn sso_cache_path(cache_key: &str) -> Result<std::path::PathBuf> {
    let mut h = sha1::Sha1::new();
    h.update(cache_key.as_bytes());
    let digest = hex::encode(h.finalize());
    Ok(home_dir()?
        .join(".aws")
        .join("sso")
        .join("cache")
        .join(format!("{digest}.json")))
}

fn home_dir() -> Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| Error::Auth("HOME is not set (cannot locate ~/.aws)".to_string()))
}

/// `Ok(true)` if the token expired (expiresAt < now). Tolerates the `Z` suffix and fractional secs.
fn token_expired(expires_at: &str) -> bool {
    match chrono::DateTime::parse_from_rfc3339(expires_at) {
        Ok(t) => t.with_timezone(&chrono::Utc) < chrono::Utc::now(),
        Err(_) => true, // unparseable → treat as expired (refresh)
    }
}

// --- SSO -----------------------------------------------------------------------------------------

/// Resolve SSO credentials: read the profile's sso_session, read/refresh the cached access token,
/// call `sso:GetRoleCredentials`. Returns `None` (not an error) if no SSO profile is configured —
/// the chain falls through to the next source.
// A-37: the `serde_json::from_str` calls here parse the local SSO token cache and the
// `GetRoleCredentials` response — the AWS credential-resolution path, not the model-stream
// invariant this story enforces (A-36 explicitly left it untouched). Allowed at this tight scope.
#[allow(clippy::disallowed_methods)]
async fn resolve_sso() -> Result<Option<BedrockCreds>> {
    let profile = std::env::var("AWS_PROFILE")
        .or_else(|_| std::env::var("AWS_DEFAULT_PROFILE"))
        .unwrap_or_else(|_| "default".to_string());
    let cfg_text = match std::fs::read_to_string(aws_config_path()?) {
        Ok(t) => t,
        Err(_) => return Ok(None), // no ~/.aws/config → not an SSO setup
    };
    let cfg = parse_aws_config(&cfg_text);
    let prof = match cfg.get(&profile_section(&profile)) {
        Some(p) => p,
        None => return Ok(None),
    };
    // Modern sso-session format: `sso_session = <name>` (+ `sso_account_id`, `sso_role_name`).
    // Legacy: `sso_start_url` + `sso_region` directly in the profile.
    let (session_name, start_url, sso_region) = if let Some(session) = prof.get("sso_session") {
        let sec = cfg.get(&format!("sso-session {session}")).ok_or_else(|| {
            Error::Auth(format!("~/.aws/config: [sso-session {session}] missing"))
        })?;
        (
            session.clone(),
            sec.get("sso_start_url")
                .ok_or_else(|| {
                    Error::Auth(format!("sso-session {session}: sso_start_url missing"))
                })?
                .clone(),
            sec.get("sso_region")
                .or_else(|| prof.get("sso_region"))
                .cloned()
                .unwrap_or_else(|| "us-east-1".to_string()),
        )
    } else if let Some(url) = prof.get("sso_start_url") {
        // Legacy: cache key is the start_url, region from the profile's sso_region.
        (
            url.clone(),
            url.clone(),
            prof.get("sso_region")
                .cloned()
                .unwrap_or_else(|| "us-east-1".to_string()),
        )
    } else {
        return Ok(None); // not an SSO profile
    };
    let account_id = prof
        .get("sso_account_id")
        .ok_or_else(|| Error::Auth(format!("profile {profile}: sso_account_id missing")))?
        .clone();
    let role_name = prof
        .get("sso_role_name")
        .ok_or_else(|| Error::Auth(format!("profile {profile}: sso_role_name missing")))?
        .clone();
    // The profile's `region` (for the Bedrock endpoint) — falls back to the sso_region then env.
    let region = prof
        .get("region")
        .cloned()
        .or_else(|| {
            std::env::var("AWS_REGION")
                .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                .ok()
        })
        .unwrap_or_else(|| sso_region.clone());

    // Cache key: sha1(session_name) for sso-session format, sha1(start_url) for legacy.
    let cache_key = if prof.contains_key("sso_session") {
        &session_name
    } else {
        &start_url
    };
    let cache_path = sso_cache_path(cache_key)?;
    let cache_text = std::fs::read_to_string(&cache_path).map_err(|e| {
        Error::Auth(format!(
            "SSO token cache {} unreadable ({e}); run `aws sso login --profile {profile}` first",
            cache_path.display()
        ))
    })?;
    let mut cache: serde_json::Value = serde_json::from_str(&cache_text)
        .map_err(|e| Error::Auth(format!("SSO token cache corrupt ({e})")))?;

    // Refresh if the access token is expired (or missing).
    let expires_at = cache
        .get("expiresAt")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if token_expired(expires_at) {
        refresh_sso_token(&mut cache, &cache_path, &sso_region).await?;
    }
    let access_token = cache
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth("SSO cache: accessToken missing".to_string()))?
        .to_string();

    // GetRoleCredentials: GET portal.sso.<region>.amazonaws.com/federation/credentials?account_id=…
    // &role_name=… — the access token goes in the `x-amz-sso_bearer_token` HEADER (not a query
    // param), matching botocore (verified against its serialized request). A query-param access
    // token is rejected with 401 "Session token not found or invalid".
    let url = format!(
        "https://portal.sso.{sso_region}.amazonaws.com/federation/credentials?account_id={account_id}&role_name={role_name}"
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .header("x-amz-sso_bearer_token", &access_token)
        .send()
        .await
        .map_err(|e| Error::Auth(format!("SSO GetRoleCredentials: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Auth(format!(
            "SSO GetRoleCredentials → {status}: {}",
            truncate(&body, 300)
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Auth(format!("SSO GetRoleCredentials: bad JSON ({e})")))?;
    let rc = v.get("roleCredentials").ok_or_else(|| {
        Error::Auth("SSO GetRoleCredentials: roleCredentials missing".to_string())
    })?;
    Ok(Some(BedrockCreds {
        access_key: rc
            .get("accessKeyId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Auth("SSO: accessKeyId missing".to_string()))?
            .to_string(),
        secret_key: rc
            .get("secretAccessKey")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Auth("SSO: secretAccessKey missing".to_string()))?
            .to_string(),
        session_token: rc
            .get("sessionToken")
            .and_then(|v| v.as_str())
            .map(String::from),
        region,
        // `roleCredentials.expiration` is epoch **milliseconds** (the one non-ISO stamp in the
        // chain); absent/odd values degrade to `None` (never-stale).
        expiration: rc
            .get("expiration")
            .and_then(|v| v.as_i64())
            .and_then(DateTime::from_timestamp_millis),
    }))
}

/// Refresh an expired SSO access token via SSO-OIDC `CreateToken` (refresh_token grant) and persist
/// the new token + refresh token back to the cache file (so `aws sso login` need not run again
/// until the refresh token itself expires).
// A-37: same credential-path exception as `resolve_sso` — parses the `CreateToken` response, not
// model-stream bytes.
#[allow(clippy::disallowed_methods)]
async fn refresh_sso_token(
    cache: &mut serde_json::Value,
    cache_path: &std::path::Path,
    sso_region: &str,
) -> Result<()> {
    let client_id = cache
        .get("clientId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth("SSO refresh: clientId missing".to_string()))?;
    let client_secret = cache
        .get("clientSecret")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth("SSO refresh: clientSecret missing".to_string()))?;
    let refresh_token = cache
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::Auth("SSO refresh: refreshToken missing — run `aws sso login` again".to_string())
        })?;
    let url = format!("https://oidc.{sso_region}.amazonaws.com/token");
    // CreateToken is a JSON POST (application/json) with camelCase keys — matching the AWS SSO-OIDC
    // API the `aws` CLI speaks (verified against botocore's serialized request). A form-encoded
    // body with snake_case keys is rejected with 400 `invalid_request`.
    let body = serde_json::json!({
        "clientId": client_id,
        "clientSecret": client_secret,
        "grantType": "refresh_token",
        "refreshToken": refresh_token,
    });
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Auth(format!("SSO CreateToken: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Auth(format!(
            "SSO CreateToken → {status}: {}",
            truncate(&body, 300)
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Auth(format!("SSO CreateToken: bad JSON ({e})")))?;
    let new_access = v
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Auth("SSO CreateToken: accessToken missing".to_string()))?;
    let new_refresh = v.get("refreshToken").and_then(|v| v.as_str());
    let expires_in = v.get("expiresIn").and_then(|v| v.as_u64()).unwrap_or(28800);
    // Update the cache in place + persist (atomic write, 0600 — the cache holds refresh tokens).
    cache["accessToken"] = serde_json::Value::String(new_access.to_string());
    if let Some(r) = new_refresh {
        cache["refreshToken"] = serde_json::Value::String(r.to_string());
    }
    let new_expires = chrono::Utc::now() + chrono::Duration::seconds(expires_in as i64);
    cache["expiresAt"] =
        serde_json::Value::String(new_expires.format("%Y-%m-%dT%H:%M:%SZ").to_string());
    let dir = cache_path
        .parent()
        .ok_or_else(|| Error::Auth("SSO cache: no parent dir".to_string()))?;
    std::fs::create_dir_all(dir).map_err(|e| Error::Auth(format!("SSO cache mkdir: {e}")))?;
    let tmp = cache_path.with_extension("json.tmp");
    let bytes =
        serde_json::to_vec_pretty(cache).map_err(|e| Error::Auth(format!("SSO cache: {e}")))?;
    // Write 0600 + rename (atomic; matches aws CLI's own cache perms; 0600 is unix-only — on
    // Windows the file gets default ACLs, same as the aws CLI there).
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| Error::Auth(format!("SSO cache write: {e}")))?;
        f.write_all(&bytes)
            .map_err(|e| Error::Auth(format!("SSO cache write: {e}")))?;
    }
    std::fs::rename(&tmp, cache_path).map_err(|e| Error::Auth(format!("SSO cache rename: {e}")))?;
    Ok(())
}

// --- IRSA (k8s web-identity) --------------------------------------------------------------------

/// Resolve IRSA credentials: `AWS_ROLE_ARN` + `AWS_WEB_IDENTITY_TOKEN_FILE` →
/// `sts:AssumeRoleWithWebIdentity`. Returns `None` if the IRSA env vars aren't set.
async fn resolve_irsa() -> Result<Option<BedrockCreds>> {
    let (role_arn, token_file) = match (
        std::env::var("AWS_ROLE_ARN").ok(),
        std::env::var("AWS_WEB_IDENTITY_TOKEN_FILE").ok(),
    ) {
        (Some(r), Some(t)) => (r, t),
        _ => return Ok(None),
    };
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string());
    let token = std::fs::read_to_string(&token_file)
        .map_err(|e| Error::Auth(format!("IRSA token file {token_file}: {e}")))?;
    let url = format!(
        "https://sts.{region}.amazonaws.com/?Action=AssumeRoleWithWebIdentity&Version=2011-06-15&RoleArn={role_arn}&RoleSessionName=flux-bedrock&WebIdentityToken={token}"
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| Error::Auth(format!("STS AssumeRoleWithWebIdentity: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Auth(format!(
            "STS AssumeRoleWithWebIdentity → {status}: {}",
            truncate(&body, 300)
        )));
    }
    // The response is XML; extract the credential fields with a tiny tag scan (avoid an XML dep).
    let access_key = extract_xml_text(&body, "AccessKeyId")
        .ok_or_else(|| Error::Auth("STS: AccessKeyId missing".to_string()))?;
    let secret_key = extract_xml_text(&body, "SecretAccessKey")
        .ok_or_else(|| Error::Auth("STS: SecretAccessKey missing".to_string()))?;
    let session_token = extract_xml_text(&body, "SessionToken");
    let expiration = extract_xml_text(&body, "Expiration")
        .as_deref()
        .and_then(parse_expiration_iso);
    Ok(Some(BedrockCreds {
        access_key,
        secret_key,
        session_token,
        region,
        expiration,
    }))
}

// --- EKS Pod Identity ---------------------------------------------------------------------------

/// Resolve EKS Pod Identity credentials: `AWS_CONTAINER_CREDENTIALS_FULL_URI` (+ optional auth
/// token) → HTTP GET. Returns `None` if the env var isn't set.
// A-37: same credential-path exception as `resolve_sso` — parses the Pod Identity HTTP response,
// not model-stream bytes.
#[allow(clippy::disallowed_methods)]
async fn resolve_eks_pod_identity() -> Result<Option<BedrockCreds>> {
    let uri = match std::env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI").ok() {
        Some(u) => u,
        None => return Ok(None),
    };
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_string());
    let mut req = reqwest::Client::new().get(&uri);
    // The auth token: AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE (file) or AWS_TOKEN_AUTHORIZATION (literal).
    if let Ok(tok_file) = std::env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE") {
        if let Ok(tok) = std::fs::read_to_string(&tok_file) {
            req = req.header("Authorization", tok.trim());
        }
    } else if let Ok(tok) = std::env::var("AWS_TOKEN_AUTHORIZATION") {
        req = req.header("Authorization", tok);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| Error::Auth(format!("EKS Pod Identity: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(Error::Auth(format!(
            "EKS Pod Identity → {status}: {}",
            truncate(&body, 300)
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Auth(format!("EKS Pod Identity: bad JSON ({e})")))?;
    Ok(Some(BedrockCreds {
        access_key: v
            .get("AccessKeyId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Auth("EKS Pod Identity: AccessKeyId missing".to_string()))?
            .to_string(),
        secret_key: v
            .get("SecretAccessKey")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Auth("EKS Pod Identity: SecretAccessKey missing".to_string()))?
            .to_string(),
        session_token: v.get("Token").and_then(|v| v.as_str()).map(String::from),
        region,
        expiration: v
            .get("Expiration")
            .and_then(|v| v.as_str())
            .and_then(parse_expiration_iso),
    }))
}

/// Extract the text of the first `<tag>...</tag>` from an XML blob (no XML dep — STS responses are
/// simple enough for a tag scan).
fn extract_xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        s[..max].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::{Chunk, ContentBlock, StopReason};

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- SigV4 known-answer test ---------------------------------------------------------------
    //
    // sign_v4 is pinned against an **independent** HMAC-SHA256 implementation (Python's `hmac` /
    // `hashlib`) for a fixed input vector: GET / to iam.amazonaws.com on 20150830T123600Z, region
    // us-east-1, service iam, secret `wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY`, key `AKIDEXAMPLE`.
    // The expected values are what Python computes for these exact inputs (cross-verified at
    // authoring time); a regression in the signing-key derivation, canonical-request hash, or final
    // signature diverges from the pinned value and points at exactly which step broke.

    fn example_creds() -> BedrockCreds {
        BedrockCreds {
            access_key: "AKIDEXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            expiration: None,
        }
    }

    #[test]
    fn profile_gates_thinking_off_for_haiku_inference_profiles() {
        // C-49: `aws/haiku` resolves to a haiku inference-profile id; adaptive thinking and
        // `output_config.effort` are hard 400s on that generation, so the profile must gate both
        // off — through Bedrock's regional/global prefixes and `-v1:0` suffix.
        for id in [
            "global.anthropic.claude-haiku-4-5-20251001-v1:0",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        ] {
            let q = BedrockProfile.quirks_for(id);
            assert!(!q.thinking_adaptive, "{id}");
            assert!(!q.effort_output_config, "{id}");
            assert!(q.sampling_params, "{id}");
        }
        // The 4.6-family profiles keep the full feature set.
        let q = BedrockProfile.quirks_for("us.anthropic.claude-sonnet-4-6");
        assert!(q.thinking_adaptive);
        assert!(q.effort_output_config);
        assert!(q.sampling_params);
    }

    #[test]
    fn signing_key_matches_reference_implementation() {
        // kSigning for 20150830/us-east-1/iam — cross-verified against Python `hmac`.
        let k_date = hmac_key(b"AWS4wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", b"20150830");
        let k_region = hmac_key(&k_date, b"us-east-1");
        let k_service = hmac_key(&k_region, b"iam");
        let k_signing = hmac_key(&k_service, b"aws4_request");
        let expected = "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9";
        assert_eq!(hex_lower(&k_signing), expected);
    }

    #[test]
    fn canonical_request_and_signature_match_reference_implementation() {
        // Build the canonical request for the GET / example by hand (matching sign_v4's construction),
        // then assert the hashes/signature an independent Python `hmac`/`hashlib` computes.
        let canonical_request = "GET\n/\n\nhost:iam.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let canonical_hash = sha256_hex(canonical_request.as_bytes());
        // Cross-verified against Python `hashlib`:
        assert_eq!(
            canonical_hash,
            "8d3d1f45b67fa6f54eb9def444311319974e40bcff9a53fcc9f2d60cf8d61580"
        );

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/iam/aws4_request\n{}",
            canonical_hash
        );
        let k_date = hmac_key(b"AWS4wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", b"20150830");
        let k_region = hmac_key(&k_date, b"us-east-1");
        let k_service = hmac_key(&k_region, b"iam");
        let k_signing = hmac_key(&k_service, b"aws4_request");
        let signature = hex_lower(&hmac_bytes(&k_signing, string_to_sign.as_bytes()));
        // Cross-verified against Python `hmac` for these exact inputs:
        assert_eq!(
            signature,
            "91fb24346d00546d6da247c85eb79148080a6e3ae1ac9aa8eae9ccdabfd70b33"
        );
    }

    #[test]
    fn percent_encode_segment_preserves_unreserved_and_encodes_others() {
        assert_eq!(
            percent_encode_segment("us.anthropic.claude-sonnet-4-6"),
            "us.anthropic.claude-sonnet-4-6"
        );
        // `:` and `/` are NOT unreserved → encoded.
        assert_eq!(percent_encode_segment("a:b/c"), "a%3Ab%2Fc");
        assert_eq!(
            percent_encode_segment("global.anthropic.claude-haiku-4-5-20251001-v1:0"),
            "global.anthropic.claude-haiku-4-5-20251001-v1%3A0"
        );
    }

    // -- Codec ----------------------------------------------------------------------------------

    #[test]
    fn build_body_strips_model_and_stream_and_injects_version() {
        let req = Request {
            model: "us.anthropic.claude-sonnet-4-6".to_string(),
            system: None,
            system_segments: Vec::new(),
            cache_tail: false,
            messages: vec![flux_core::Message::user(vec![
                flux_core::ContentBlock::Text {
                    text: "say ok".to_string(),
                },
            ])],
            tools: vec![],
            max_tokens: 32,
            temperature: None,
            top_p: None,
            stop_sequences: vec![],
            thinking: false,
            effort: None,
            trace: None,
            metadata: serde_json::Map::new(),
        };
        let body = BedrockAnthropic.build_body(&req).unwrap();
        // Bedrock invoke-model rejects `model` (it's in the URL) and `stream` (streaming is a
        // different URL) — both must be stripped. Fails first if the strip is dropped.
        assert!(body.get("model").is_none(), "body must not carry `model`");
        assert!(body.get("stream").is_none(), "body must not carry `stream`");
        assert_eq!(body["anthropic_version"], "bedrock-2023-05-31");
        assert_eq!(body["max_tokens"], 32);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn wire_headers_is_empty_no_anthropic_version_header() {
        assert!(BedrockAnthropic.wire_headers().is_empty());
    }

    // -- C-09d: AWS event-stream deframer --------------------------------------------------------

    fn encode_string_header(name: &str, value: &str) -> Vec<u8> {
        let mut b = vec![name.len() as u8];
        b.extend_from_slice(name.as_bytes());
        b.push(7); // value type: string
        b.extend_from_slice(&(value.len() as u16).to_be_bytes());
        b.extend_from_slice(value.as_bytes());
        b
    }

    /// Encode one AWS event-stream frame — the test-side inverse of the deframer. (Not circular:
    /// `crc32` itself is pinned by `crc32_matches_check_value`.)
    fn encode_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let hb: Vec<u8> = headers
            .iter()
            .flat_map(|(n, v)| encode_string_header(n, v))
            .collect();
        let total = 12 + hb.len() + payload.len() + 4;
        let mut f = Vec::with_capacity(total);
        f.extend_from_slice(&(total as u32).to_be_bytes());
        f.extend_from_slice(&(hb.len() as u32).to_be_bytes());
        let prelude_crc = crc32(&f[..8]);
        f.extend_from_slice(&prelude_crc.to_be_bytes());
        f.extend_from_slice(&hb);
        f.extend_from_slice(payload);
        let msg_crc = crc32(&f);
        f.extend_from_slice(&msg_crc.to_be_bytes());
        f
    }

    /// A `chunk` event frame carrying one Anthropic stream event, matching the recorded live wire:
    /// the payload wraps base64 `bytes` plus the `p` padding field Bedrock adds.
    fn chunk_frame(event_json: &str) -> Vec<u8> {
        let payload =
            json!({ "bytes": BASE64.encode(event_json), "p": "abcdefghijklmnop" }).to_string();
        encode_frame(
            &[
                (":message-type", "event"),
                (":event-type", "chunk"),
                (":content-type", "application/json"),
            ],
            payload.as_bytes(),
        )
    }

    fn byte_stream_from(wire: Vec<u8>, piece_len: usize) -> ByteStream {
        let pieces: Vec<_> = wire
            .chunks(piece_len)
            .map(|c| Ok::<_, Error>(bytes::Bytes::copy_from_slice(c)))
            .collect();
        Box::pin(futures::stream::iter(pieces))
    }

    #[test]
    fn crc32_matches_check_value() {
        // The standard CRC-32/IEEE check value (see the CRC catalog): crc32("123456789").
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[tokio::test]
    async fn event_stream_emits_anthropic_sse() {
        let wire = chunk_frame(r#"{"type":"message_stop"}"#);
        let out: Vec<bytes::Bytes> = map_bedrock_event_stream(byte_stream_from(wire, 4096))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        let sse = String::from_utf8(out.concat()).unwrap();
        assert_eq!(
            sse,
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
    }

    #[tokio::test]
    async fn event_stream_deframes_a_full_turn_split_across_boundaries() {
        // A full invoke-with-response-stream turn: each frame's `bytes` is one native Anthropic
        // Messages stream event.
        let events = [
            r#"{"type":"message_start","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let wire: Vec<u8> = events.iter().flat_map(|e| chunk_frame(e)).collect();
        // Feed the bytes in 7-byte pieces: every frame arrives split across chunk boundaries, so a
        // deframer that assumes frame-aligned reads drops PayloadParts (the failing-first mode).
        let chunks: Vec<Chunk> = BedrockAnthropic
            .map_stream(byte_stream_from(wire, 7))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|c| c.unwrap())
            .collect();
        assert!(
            matches!(chunks[0], Chunk::MessageStart { ref model } if model == "claude-sonnet-4-6")
        );
        let text: String = chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello", "a dropped PayloadPart loses a delta");
        assert!(chunks
            .iter()
            .any(|c| matches!(c, Chunk::Block(ContentBlock::Text { text }) if text == "Hello")));
        assert!(matches!(
            chunks.last().unwrap(),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn)
            }
        ));
    }

    #[tokio::test]
    async fn event_stream_surfaces_exception_frames() {
        let wire = encode_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "throttlingException"),
            ],
            br#"{"message":"Too many requests"}"#,
        );
        let results: Vec<_> = map_bedrock_event_stream(byte_stream_from(wire, 4096))
            .collect::<Vec<_>>()
            .await;
        let err = results
            .into_iter()
            .find_map(|r| r.err())
            .expect("an exception frame must surface as an error");
        let msg = err.to_string();
        assert!(msg.contains("throttlingException"), "kind in: {msg}");
        assert!(msg.contains("Too many requests"), "message in: {msg}");
    }

    #[tokio::test]
    async fn bedrock_truncated_tail_is_a_classified_stream_decode_error() {
        // A-36: a stream that ends mid-frame is a genuine framing-integrity failure — the
        // underlying AWS event-stream framing itself is broken, not one bad payload. It must stay
        // fatal, but classified `StreamDecode` so the A-33 backstop retries the call.
        let mut wire = chunk_frame(r#"{"type":"message_stop"}"#);
        let next = chunk_frame(r#"{"type":"ping"}"#);
        wire.extend_from_slice(&next[..10]); // connection cut mid-frame
        let results: Vec<_> = map_bedrock_event_stream(byte_stream_from(wire, 4096))
            .collect::<Vec<_>>()
            .await;
        assert!(results[0].is_ok(), "the complete frame still decodes");
        let err = results
            .last()
            .unwrap()
            .as_ref()
            .expect_err("trailing bytes must error, not be dropped");
        assert!(err.to_string().contains("truncated"), "{err}");
        assert!(
            matches!(err, Error::StreamDecode(_)),
            "a truncated tail must classify as StreamDecode, not Provider: {err:?}"
        );
    }

    #[tokio::test]
    async fn bedrock_non_json_chunk_payload_is_skipped() {
        // A-36: a `chunk` event whose payload bytes aren't valid JSON is "one AWS-level chunk was
        // junk" — skip it (count it, warn) and keep deframing subsequent frames; it must NOT kill
        // the stream.
        let garbage = encode_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            b"not json at all",
        );
        let good = chunk_frame(r#"{"type":"message_stop"}"#);
        let mut wire = garbage;
        wire.extend_from_slice(&good);
        let out: Vec<bytes::Bytes> = map_bedrock_event_stream(byte_stream_from(wire, 4096))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.expect("a garbage chunk payload must be skipped, not fatal"))
            .collect();
        let sse = String::from_utf8(out.concat()).unwrap();
        assert_eq!(
            sse, "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            "deframing continues past the garbage frame to the good one"
        );
    }

    #[tokio::test]
    async fn bedrock_bad_base64_chunk_is_skipped() {
        // A-36: the outer `{"bytes": "<base64>"}` wrapper's base64 doesn't decode — same "one
        // AWS-level chunk was junk" tolerance as non-JSON payloads.
        let payload = json!({ "bytes": "not-valid-base64!!!" }).to_string();
        let garbage = encode_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            payload.as_bytes(),
        );
        let good = chunk_frame(r#"{"type":"message_stop"}"#);
        let mut wire = garbage;
        wire.extend_from_slice(&good);
        let out: Vec<bytes::Bytes> = map_bedrock_event_stream(byte_stream_from(wire, 4096))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|r| r.expect("bad base64 must be skipped, not fatal"))
            .collect();
        let sse = String::from_utf8(out.concat()).unwrap();
        assert_eq!(
            sse, "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            "deframing continues past the garbage frame to the good one"
        );
    }

    #[tokio::test]
    async fn bedrock_crc_mismatch_is_a_classified_stream_decode_error() {
        // A-36: a CRC mismatch is a genuine framing-integrity failure (the underlying AWS
        // event-stream framing is broken), not "one chunk's payload was junk" — it must still be
        // fatal, but classified `StreamDecode` (not `Provider`) so the A-33 planner backstop
        // retries the call as one step instead of killing the whole turn.
        let mut wire = chunk_frame(r#"{"type":"message_stop"}"#);
        wire[8] ^= 0xFF; // flip a prelude-CRC byte
        let results: Vec<_> = map_bedrock_event_stream(byte_stream_from(wire, 4096))
            .collect::<Vec<_>>()
            .await;
        let err = results[0].as_ref().expect_err("corrupt prelude must error");
        assert!(err.to_string().contains("prelude CRC"), "{err}");
        assert!(
            matches!(err, Error::StreamDecode(_)),
            "CRC mismatch must classify as StreamDecode, not Provider: {err:?}"
        );
    }

    #[tokio::test]
    async fn bedrock_message_crc_mismatch_is_also_classified_stream_decode_error() {
        // The message-level CRC (frame_to_sse), a distinct integrity check from the prelude CRC
        // above, must reclassify the same way.
        let mut wire = chunk_frame(r#"{"type":"message_stop"}"#);
        let last = wire.len() - 1;
        wire[last] ^= 0xFF; // flip a message-CRC byte (the trailing 4 bytes of the frame)
        let results: Vec<_> = map_bedrock_event_stream(byte_stream_from(wire, 4096))
            .collect::<Vec<_>>()
            .await;
        let err = results[0]
            .as_ref()
            .expect_err("corrupt message CRC must error");
        assert!(err.to_string().contains("message CRC"), "{err}");
        assert!(
            matches!(err, Error::StreamDecode(_)),
            "message CRC mismatch must classify as StreamDecode, not Provider: {err:?}"
        );
    }

    #[test]
    fn endpoint_is_streaming_invoke() {
        let cred = BedrockCredential {
            model_id: "us.anthropic.claude-sonnet-4-6".to_string(),
            region: "us-east-1".to_string(),
            creds: Mutex::new(Some(example_creds())),
            resolver: Arc::new(EnvStaticResolver),
        };
        assert_eq!(
            cred.endpoint(),
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/us.anthropic.claude-sonnet-4-6/invoke-with-response-stream"
        );
    }

    // -- resolve_model --------------------------------------------------------------------------

    #[test]
    fn resolve_model_maps_aliases_to_cross_region_profiles() {
        let _env = env_guard();
        // Default (no AWS_REGION set, no haiku override) → us-prefix, haiku stays global.
        unsafe {
            std::env::remove_var("AWS_REGION");
            std::env::remove_var("AWS_DEFAULT_REGION");
            std::env::remove_var("FLUX_BEDROCK_HAIKU_PROFILE");
        }
        assert_eq!(resolve_model("sonnet"), "us.anthropic.claude-sonnet-4-6");
        assert_eq!(resolve_model("opus"), "us.anthropic.claude-opus-4-6-v1");
        assert_eq!(
            resolve_model("haiku"),
            "global.anthropic.claude-haiku-4-5-20251001-v1:0"
        );
        // Empty (bare `aws`) → sonnet default.
        assert_eq!(resolve_model(""), "us.anthropic.claude-sonnet-4-6");
        // Pass-through for explicit ids.
        assert_eq!(
            resolve_model("us.anthropic.claude-opus-4-8"),
            "us.anthropic.claude-opus-4-8"
        );
    }

    /// D-60: `FLUX_BEDROCK_HAIKU_PROFILE` overrides haiku's cross-region inference-profile prefix
    /// — for IAM setups that don't grant `bedrock:InvokeModel*` on `global.*` profiles, only
    /// region-specific ones. Unset (the default) keeps today's behavior (`global.`); set, it
    /// replaces the prefix outright — sonnet/opus are untouched either way.
    #[test]
    fn resolve_model_haiku_profile_is_overridable_and_defaults_to_global() {
        let _env = env_guard();
        unsafe {
            std::env::remove_var("AWS_REGION");
            std::env::remove_var("AWS_DEFAULT_REGION");
            std::env::remove_var("FLUX_BEDROCK_HAIKU_PROFILE");
        }
        // Default: unchanged `global.` behavior.
        assert_eq!(
            resolve_model("haiku"),
            "global.anthropic.claude-haiku-4-5-20251001-v1:0"
        );

        // Override: pin haiku to the region-specific prefix instead.
        unsafe {
            std::env::set_var("FLUX_BEDROCK_HAIKU_PROFILE", "us");
        }
        assert_eq!(
            resolve_model("haiku"),
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        );
        // sonnet/opus are unaffected by the haiku-only override.
        assert_eq!(resolve_model("sonnet"), "us.anthropic.claude-sonnet-4-6");

        unsafe {
            std::env::remove_var("FLUX_BEDROCK_HAIKU_PROFILE");
        }
    }

    #[test]
    fn resolve_model_is_region_aware() {
        let _env = env_guard();
        // Failing-first for the region-prefix bug: the cross-region inference-profile id must
        // match the Bedrock region — `us.anthropic.*` is invalid in eu-central-1 (Bedrock 400
        // "The provided model identifier is invalid"), and vice versa. The credential chain sets
        // AWS_REGION before resolve_model runs; the prefix follows it.
        unsafe {
            std::env::set_var("AWS_REGION", "eu-central-1");
        }
        assert_eq!(resolve_model("sonnet"), "eu.anthropic.claude-sonnet-4-6");
        assert_eq!(resolve_model("opus"), "eu.anthropic.claude-opus-4-6-v1");
        // Haiku stays `global.` regardless of region (it's a global profile).
        assert_eq!(
            resolve_model("haiku"),
            "global.anthropic.claude-haiku-4-5-20251001-v1:0"
        );
        unsafe {
            std::env::set_var("AWS_REGION", "us-east-1");
        }
        assert_eq!(resolve_model("sonnet"), "us.anthropic.claude-sonnet-4-6");
        unsafe {
            std::env::remove_var("AWS_REGION");
        }
    }

    // -- EnvStaticResolver ----------------------------------------------------------------------

    // Holding the env lock across the resolver's await is the point: it blocks the other
    // env-mutating test threads (each #[tokio::test] runs its own runtime, so no deadlock).
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn env_static_resolver_reads_aws_env() {
        let _env = env_guard();
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", "AKIATEST");
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "secret");
            std::env::set_var("AWS_SESSION_TOKEN", "tok");
            std::env::set_var("AWS_REGION", "eu-west-1");
        }
        let creds = EnvStaticResolver.resolve().await.unwrap();
        assert_eq!(creds.access_key, "AKIATEST");
        assert_eq!(creds.secret_key, "secret");
        assert_eq!(creds.session_token.as_deref(), Some("tok"));
        assert_eq!(creds.region, "eu-west-1");
        let chain_creds = AwsChainResolver.resolve().await.unwrap();
        assert_eq!(chain_creds.access_key, "AKIATEST");
        assert_eq!(chain_creds.secret_key, "secret");
        assert_eq!(chain_creds.session_token.as_deref(), Some("tok"));
        assert_eq!(chain_creds.region, "eu-west-1");
        unsafe {
            std::env::remove_var("AWS_ACCESS_KEY_ID");
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
            std::env::remove_var("AWS_SESSION_TOKEN");
            std::env::remove_var("AWS_REGION");
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn env_static_resolver_errors_without_access_key() {
        let _env = env_guard();
        unsafe {
            std::env::remove_var("AWS_ACCESS_KEY_ID");
        }
        assert!(EnvStaticResolver.resolve().await.is_err());
    }

    #[allow(dead_code)]
    fn _example_creds_for_doctest() -> BedrockCreds {
        example_creds()
    }

    // --- C-09b: AWS credential chain pure helpers ----------------------------------------------

    #[test]
    fn parse_aws_config_handles_sections_and_profiles() {
        let text = r#"
[default]
region = us-east-1

[profile babelforce-dev]
sso_session = babelforce
sso_account_id = 123456789012
sso_role_name = DeveloperAccess

[sso-session babelforce]
sso_start_url = https://d-xxxxxxxxxx.awsapps.com/start
sso_region = eu-central-1
"#;
        let cfg = parse_aws_config(text);
        assert_eq!(cfg["default"]["region"], "us-east-1");
        assert_eq!(cfg["profile babelforce-dev"]["sso_session"], "babelforce");
        assert_eq!(
            cfg["profile babelforce-dev"]["sso_account_id"],
            "123456789012"
        );
        assert_eq!(
            cfg["sso-session babelforce"]["sso_start_url"],
            "https://d-xxxxxxxxxx.awsapps.com/start"
        );
        assert_eq!(cfg["sso-session babelforce"]["sso_region"], "eu-central-1");
    }

    #[test]
    fn profile_section_is_default_or_profile_prefix() {
        assert_eq!(profile_section("default"), "default");
        assert_eq!(profile_section("babelforce-dev"), "profile babelforce-dev");
    }

    #[test]
    fn token_expired_detects_past_and_future() {
        let past = "2020-01-01T00:00:00Z";
        let future = "2099-01-01T00:00:00Z";
        assert!(token_expired(past), "past expiry → expired");
        assert!(!token_expired(future), "future expiry → valid");
        // Unparseable → treat as expired (forces a refresh rather than using a bad token).
        assert!(token_expired("not-a-date"));
    }

    #[test]
    fn sso_cache_path_is_sha1_of_session_under_home() {
        // The cache filename for an sso-session profile is sha1(session_name).hex — matching the
        // `aws` CLI's own cache layout (verified live against the dev account).
        std::env::set_var("HOME", "/tmp/flux-sso-test-home");
        let p = sso_cache_path("babelforce").unwrap();
        let mut h = sha1::Sha1::new();
        h.update(b"babelforce");
        let digest = hex::encode(h.finalize());
        assert_eq!(
            p,
            std::path::PathBuf::from(format!(
                "/tmp/flux-sso-test-home/.aws/sso/cache/{digest}.json"
            ))
        );
        std::env::remove_var("HOME");
    }

    #[test]
    fn extract_xml_text_pulls_tag_content() {
        let xml = r#"<AssumeRoleWithWebIdentityResponse><Credentials><AccessKeyId>AKIAFOO</AccessKeyId><SecretAccessKey>SECRET</SecretAccessKey><SessionToken>TOK</SessionToken></Credentials></AssumeRoleWithWebIdentityResponse>"#;
        assert_eq!(
            extract_xml_text(xml, "AccessKeyId"),
            Some("AKIAFOO".to_string())
        );
        assert_eq!(
            extract_xml_text(xml, "SecretAccessKey"),
            Some("SECRET".to_string())
        );
        assert_eq!(
            extract_xml_text(xml, "SessionToken"),
            Some("TOK".to_string())
        );
        assert_eq!(extract_xml_text(xml, "Missing"), None);
    }

    // -- C-37: credential lifecycle — lazy resolve + expiry re-resolution ------------------------

    /// Counts its resolves and hands out creds with a configurable expiration (and a region that
    /// deliberately differs from the credential's pin, to prove the coercion).
    struct CountingResolver {
        calls: std::sync::atomic::AtomicUsize,
        expiration: Option<DateTime<Utc>>,
    }

    impl CountingResolver {
        fn new(expiration: Option<DateTime<Utc>>) -> Arc<Self> {
            Arc::new(Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
                expiration,
            })
        }
        fn count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl BedrockCredentialsResolver for CountingResolver {
        async fn resolve(&self) -> Result<BedrockCreds> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(BedrockCreds {
                access_key: "AKIDEXAMPLE".to_string(),
                secret_key: "SECRET".to_string(),
                session_token: None,
                region: "us-west-2".to_string(), // ≠ the pin — coercion must overwrite it
                expiration: self.expiration,
            })
        }
    }

    fn lazy_credential(resolver: Arc<CountingResolver>) -> BedrockCredential {
        BedrockCredential {
            model_id: "eu.anthropic.claude-sonnet-4-6".to_string(),
            region: "eu-central-1".to_string(),
            creds: Mutex::new(None),
            resolver,
        }
    }

    #[tokio::test]
    async fn lazy_credential_resolves_on_first_apply_not_construction() {
        let resolver = CountingResolver::new(None);
        let cred = lazy_credential(resolver.clone());
        assert_eq!(resolver.count(), 0, "construction must not resolve");
        let rb = reqwest::Client::new()
            .post(cred.endpoint())
            .json(&json!({}));
        let _signed = cred.apply(rb).await.expect("apply signs after resolving");
        assert_eq!(resolver.count(), 1, "first apply walks the chain");
    }

    #[tokio::test]
    async fn fresh_creds_resolve_once_across_applies() {
        // Far-future expiry (and the None case via the lazy test above): the cache holds.
        let resolver = CountingResolver::new(Some(Utc::now() + chrono::Duration::hours(2)));
        let cred = lazy_credential(resolver.clone());
        cred.fresh_creds().await.unwrap();
        cred.fresh_creds().await.unwrap();
        assert_eq!(resolver.count(), 1, "fresh creds are not re-resolved");
    }

    #[tokio::test]
    async fn near_expiry_creds_re_resolve_on_apply() {
        // Inside the 5-minute window: every use re-resolves (the resolver keeps returning
        // near-expiry creds, so each call is stale again — in production the chain returns a
        // fresh session).
        let resolver = CountingResolver::new(Some(Utc::now() + chrono::Duration::seconds(60)));
        let cred = lazy_credential(resolver.clone());
        cred.fresh_creds().await.unwrap();
        cred.fresh_creds().await.unwrap();
        assert_eq!(resolver.count(), 2, "near-expiry creds must re-resolve");
    }

    #[tokio::test]
    async fn resolved_creds_are_coerced_to_the_pinned_region() {
        let resolver = CountingResolver::new(None);
        let cred = lazy_credential(resolver);
        let creds = cred.fresh_creds().await.unwrap();
        // The resolver said us-west-2; the pin wins so the SigV4 scope matches the URL host.
        assert_eq!(creds.region, "eu-central-1");
    }

    #[test]
    fn lazy_endpoint_uses_pinned_region_before_resolve() {
        let cred = lazy_credential(CountingResolver::new(None));
        assert_eq!(
            cred.endpoint(),
            "https://bedrock-runtime.eu-central-1.amazonaws.com/model/eu.anthropic.claude-sonnet-4-6/invoke-with-response-stream"
        );
    }

    #[test]
    fn creds_stale_only_within_window() {
        let with = |expiration| BedrockCreds {
            expiration,
            ..example_creds()
        };
        assert!(!creds_stale(&with(None)), "no expiry → never stale");
        assert!(!creds_stale(&with(Some(
            Utc::now() + chrono::Duration::hours(1)
        ))));
        assert!(creds_stale(&with(Some(
            Utc::now() + chrono::Duration::seconds(60)
        ))));
        assert!(creds_stale(&with(Some(
            Utc::now() - chrono::Duration::seconds(10)
        ))));
    }

    #[test]
    fn parse_expiration_tolerates_wire_formats() {
        assert!(parse_expiration_iso("2030-01-01T00:00:00Z").is_some());
        assert!(parse_expiration_iso("2030-01-01T00:00:00.123456Z").is_some());
        assert!(parse_expiration_iso("2030-01-01T02:00:00+02:00").is_some());
        assert!(parse_expiration_iso(" 2030-01-01T00:00:00Z ").is_some());
        assert!(parse_expiration_iso("not-a-date").is_none());
        assert!(parse_expiration_iso("").is_none());
    }
}

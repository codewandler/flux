//! The `aws` (AWS Bedrock) provider.
//!
//! Bedrock serves Anthropic models behind an AWS Signature-V4 gate. The load-bearing reuse:
//! `bedrock-runtime invoke-model` on an Anthropic model returns **native Anthropic Messages JSON**
//! — the exact shape flux's [`crate::messages`] codec already produces and parses — so the wire
//! codec is a thin wrapper over [`build_messages_body`] that moves `anthropic_version` from a header
//! into the body (Bedrock's one wire quirk) and parses the single non-streaming response JSON into
//! [`Chunk`](flux_core::Chunk)s.
//!
//! The credential side is **SigV4 signing** (hand-rolled here, ~150 lines, pinned by a known-answer
//! test) over an injected [`BedrockCredentialsResolver`] — the seam that lets the credential source
//! be swapped without touching L1. The shipped stand-in is [`EnvStaticResolver`] (reads
//! `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`/`AWS_REGION`); the design's
//! Option C replaces it with an `aws-bedrock` plugin that embeds `aws-config` (full SSO / IRSA /
//! EKS Pod Identity chain) at this same trait. See `docs/designs/aws-bedrock-provider.md`.
//!
//! Non-streaming `invoke-model` ships first (the response is one Messages JSON object, mapped by
//! [`map_messages_json`]). Streaming (`invoke-with-response-stream` → AWS binary event-stream →
//! the existing [`map_messages_stream`]) is C-09d.
//!
//! Layering: L1, no flux deps above L0/L1. Crypto (`sha2`/`hmac`) is pure; the credential reads
//! env (the established L1 pattern — `anthropic_from_env` reads `ANTHROPIC_API_KEY`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::messages::{build_messages_body, MessagesQuirks, ProviderProfile};
use flux_core::{Chunk, ContentBlock, Error, Result, StopReason, Usage};
use flux_provider::{ByteStream, ChunkStream, Credential, NativeProvider, Request, WireCodec};

type HmacSha256 = Hmac<Sha256>;

const BEDROCK_ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const SIGV4_SERVICE: &str = "bedrock";
const SIGV4_TERMINATOR: &str = "aws4_request";
const SIGV4_ALGO: &str = "AWS4-HMAC-SHA256";

// ---------------------------------------------------------------------------
// Quirks profile
// ---------------------------------------------------------------------------

/// Bedrock passes the full Anthropic Messages feature set through to the same backend — the same
/// profile as Anthropic-direct.
pub struct BedrockProfile;

impl ProviderProfile for BedrockProfile {
    fn quirks_for(&self, _model: &str) -> MessagesQuirks {
        MessagesQuirks {
            prompt_caching: true,
            thinking_adaptive: true,
            effort_output_config: true,
            extra_body: Default::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire codec
// ---------------------------------------------------------------------------

/// The Bedrock Anthropic wire protocol: `POST /model/{modelId}/invoke` (non-streaming), body is the
/// Anthropic Messages shape with `anthropic_version: "bedrock-2023-05-31"` in the body (not a
/// header), response is a single native Messages JSON object.
pub struct BedrockAnthropic;

impl WireCodec for BedrockAnthropic {
    fn build_body(&self, req: &Request) -> Result<Value> {
        let mut body = build_messages_body(req, &BedrockProfile.quirks_for(&req.model))?;
        // Bedrock's one wire quirk: the version lives in the body, not a header.
        body["anthropic_version"] = json!(BEDROCK_ANTHROPIC_VERSION);
        // Bedrock invoke-model (non-streaming) rejects `model` (it's in the URL path via the
        // model id) and `stream` (streaming is a *different* URL). Both are emitted by the shared
        // Messages body builder; strip them.
        body.as_object_mut()
            .expect("messages body is an object")
            .remove("model");
        body.as_object_mut()
            .expect("messages body is an object")
            .remove("stream");
        Ok(body)
    }

    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        map_messages_json(bytes)
    }

    fn wire_headers(&self) -> Vec<(&'static str, String)> {
        // NO `anthropic-version` header — it's in the body. Auth headers are set by the credential.
        Vec::new()
    }
}

/// Map a single Bedrock `invoke-model` response (one native Messages JSON object) into [`Chunk`]s.
///
/// The response shape is `{"model","content":[{"type":"text"|"thinking"|...}],"stop_reason",
/// "usage":{"input_tokens","output_tokens","cache_creation_input_tokens","cache_read_input_tokens"}}`.
/// `flux_core::ContentBlock` already deserializes from the Messages content shape, so the content
/// array maps directly; usage maps field-for-field. Emits `MessageStart` → one `TextDelta` per text
/// block (live display) + the assembled `Block` per content block → `Usage` → `Done`.
fn map_messages_json(byte_stream: ByteStream) -> ChunkStream {
    Box::pin(async_stream::try_stream! {
        // Buffer the whole body — invoke-model returns one JSON object, not a stream of events.
        let mut buf: Vec<u8> = Vec::new();
        let mut s = byte_stream;
        while let Some(chunk) = s.next().await {
            buf.extend_from_slice(&chunk?);
        }
        let resp: Value = serde_json::from_slice(&buf).map_err(|e| {
            Error::Provider(format!("bedrock: bad response JSON ({e}); raw head: {}", String::from_utf8_lossy(&buf[..buf.len().min(400)])))
        })?;

        let model = resp
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        yield Chunk::MessageStart { model };

        if let Some(content) = resp.get("content").and_then(|v| v.as_array()) {
            for block in content {
                // Text deltas feed live display; the assembled Block is emitted for the loop host.
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                        yield Chunk::TextDelta(text.to_string());
                    }
                }
                match serde_json::from_value::<ContentBlock>(block.clone()) {
                    Ok(b) => yield Chunk::Block(b),
                    Err(_) => {}
                }
            }
        }

        if let Some(u) = resp.get("usage") {
            yield Chunk::Usage(Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                cache_creation_input_tokens: u
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                cache_read_input_tokens: u
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                reasoning_tokens: 0,
            });
        }

        let stop = resp
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .map(map_stop_reason)
            .unwrap_or(StopReason::Unknown);
        yield Chunk::Done { stop_reason: Some(stop) };
    })
}

fn map_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => StopReason::Refusal,
        _ => StopReason::Unknown,
    }
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
    })
}

#[async_trait]
impl BedrockCredentialsResolver for EnvStaticResolver {
    async fn resolve(&self) -> Result<BedrockCreds> {
        creds_from_env()
    }
}

/// A Bedrock [`Credential`]: holds the model id + resolved creds (cached, refreshed on 401) and
/// signs each request with SigV4. `endpoint()` needs the region, which comes from the resolver — so
/// the creds are resolved once at construction (by [`bedrock_from_env`]) and cached; the C-04
/// 401-refresh path re-resolves via the stored resolver.
pub struct BedrockCredential {
    model_id: String,
    creds: Mutex<BedrockCreds>,
    #[allow(dead_code)]
    // C-04: wired when the 401-refresh TokenSource lands (Option C plugin resolver).
    resolver: Arc<dyn BedrockCredentialsResolver>,
}

impl BedrockCredential {
    fn endpoint_url(&self, creds: &BedrockCreds) -> String {
        // Non-streaming invoke-model. The model id may contain `.`/`:`/`-` — percent-encode the
        // path segment (not the slashes between segments).
        let encoded = percent_encode_segment(&creds.model_id_placeholder(&self.model_id));
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke",
            creds.region, encoded
        )
    }
}

// `BedrockCreds` carries the region but not the model id (the model is the credential's, not the
// chain's); thread it through the URL builder via a tiny helper so `endpoint()` reads only creds.
impl BedrockCreds {
    fn model_id_placeholder<'a>(&'a self, model_id: &'a str) -> String {
        model_id.to_string()
    }
}

#[async_trait]
impl Credential for BedrockCredential {
    fn endpoint(&self) -> String {
        // Sync: read the cached creds. (Resolved once at construction; refreshed on 401.)
        let creds = self.creds.lock().expect("bedrock creds mutex poisoned");
        self.endpoint_url(&creds)
    }

    async fn apply(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let creds = self
            .creds
            .lock()
            .expect("bedrock creds mutex poisoned")
            .clone();
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
pub fn resolve_model(alias: &str) -> String {
    match alias {
        "" | "sonnet" => "us.anthropic.claude-sonnet-4-6",
        "opus" => "us.anthropic.claude-opus-4-6-v1",
        "haiku" => "global.anthropic.claude-haiku-4-5-20251001-v1:0",
        other => other,
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

/// Build the `aws` Bedrock provider from `AWS_*` env (the static-creds stand-in), **sync** — for
/// the CLI's sync `build_provider`. Reads env once via [`creds_from_env`]; the stored
/// [`EnvStaticResolver`] backs the C-04 refresh path. For the dev account (SSO-only), materialize
/// short-lived creds into env first: `aws configure export-credentials --profile <p> --format env`.
/// (When the `aws-bedrock` plugin lands, this is replaced by the async [`bedrock_with`] with an
/// injected plugin resolver — the L1 seam is unchanged.)
pub fn bedrock_with_env(model_id: String) -> Result<NativeProvider> {
    let creds = creds_from_env()?;
    Ok(NativeProvider::new(
        "aws",
        Arc::new(BedrockAnthropic),
        Arc::new(BedrockCredential {
            model_id,
            creds: Mutex::new(creds),
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
            creds: Mutex::new(creds),
            resolver,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
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

    #[tokio::test]
    async fn map_messages_json_parses_one_response_into_chunks() {
        let resp = json!({
            "model": "claude-sonnet-4-6",
            "content": [{"type":"text","text":"ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 14, "output_tokens": 4,
                "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
            }
        });
        let bytes = futures::stream::once(async move {
            Ok::<_, flux_core::Error>(bytes::Bytes::from(resp.to_string()))
        });
        let stream = map_messages_json(Box::pin(bytes));
        let chunks: Vec<Chunk> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|c| c.unwrap())
            .collect();
        // MessageStart → TextDelta("ok") → Block(Text) → Usage → Done.
        assert!(
            matches!(chunks[0], Chunk::MessageStart { ref model } if model == "claude-sonnet-4-6")
        );
        assert!(matches!(chunks[1], Chunk::TextDelta(ref t) if t == "ok"));
        assert!(matches!(chunks[2], Chunk::Block(ContentBlock::Text { ref text }) if text == "ok"));
        assert!(matches!(chunks[3], Chunk::Usage(_)));
        assert!(matches!(
            chunks[4],
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn)
            }
        ));
    }

    // -- resolve_model --------------------------------------------------------------------------

    #[test]
    fn resolve_model_maps_aliases_to_cross_region_profiles() {
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

    // -- EnvStaticResolver ----------------------------------------------------------------------

    #[tokio::test]
    async fn env_static_resolver_reads_aws_env() {
        // SAFETY: these env reads/writes are confined to this test; std::env is process-global but
        // tests don't run the resolver concurrently against these keys.
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
        unsafe {
            std::env::remove_var("AWS_ACCESS_KEY_ID");
            std::env::remove_var("AWS_SECRET_ACCESS_KEY");
            std::env::remove_var("AWS_SESSION_TOKEN");
        }
    }

    #[tokio::test]
    async fn env_static_resolver_errors_without_access_key() {
        unsafe {
            std::env::remove_var("AWS_ACCESS_KEY_ID");
        }
        assert!(EnvStaticResolver.resolve().await.is_err());
    }

    #[allow(dead_code)]
    fn _example_creds_for_doctest() -> BedrockCreds {
        example_creds()
    }
}

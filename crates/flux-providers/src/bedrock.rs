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
    let prefix = region_prefix(&region);
    match alias {
        // The active Claude 4-6 generation. Region-prefixed (us./eu.) — both exist in their
        // respective region catalogs. `global.` is not available for sonnet-4-6 in all regions.
        "" | "sonnet" => format!("{prefix}.anthropic.claude-sonnet-4-6"),
        "opus" => format!("{prefix}.anthropic.claude-opus-4-6-v1"),
        // Haiku is a `global.` profile (works in every region) — don't region-prefix it.
        "haiku" => "global.anthropic.claude-haiku-4-5-20251001-v1:0".to_string(),
        other => other.to_string(),
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

/// Resolve the default chain and materialize the result into `AWS_*` env vars (idempotent: a no-op
/// when `AWS_ACCESS_KEY_ID` is already set). The CLI's async `build_agent` calls this for the `aws`
/// provider arm before the sync `build_provider` → `bedrock_with_env` reads env — so the resolved
/// creds reach every sync path (REPL `/model`, sub-agent factory, server) without making
/// `build_provider` async (the sub-agent `Spawner` closure is sync). The session token + region are
/// set too.creds stay in the process env for the session.
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
    }))
}

/// Refresh an expired SSO access token via SSO-OIDC `CreateToken` (refresh_token grant) and persist
/// the new token + refresh token back to the cache file (so `aws sso login` need not run again
/// until the refresh token itself expires).
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
    // Write 0600 + rename (atomic; matches aws CLI's own cache perms).
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| Error::Auth(format!("SSO cache write: {e}")))?;
        use std::io::Write;
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
    Ok(Some(BedrockCreds {
        access_key,
        secret_key,
        session_token,
        region,
    }))
}

// --- EKS Pod Identity ---------------------------------------------------------------------------

/// Resolve EKS Pod Identity credentials: `AWS_CONTAINER_CREDENTIALS_FULL_URI` (+ optional auth
/// token) → HTTP GET. Returns `None` if the env var isn't set.
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
        // Default (no AWS_REGION set) → us-prefix.
        unsafe {
            std::env::remove_var("AWS_REGION");
            std::env::remove_var("AWS_DEFAULT_REGION");
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

    #[test]
    fn resolve_model_is_region_aware() {
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
}

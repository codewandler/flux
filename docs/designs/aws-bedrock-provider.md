# Design: AWS Bedrock LLM provider

**Status:** planned (scoping) · **Pillar:** Core · **Layer:** L1 (`flux-providers` + `flux-credentials`)
+ L0 (`flux-core`) + L6 (`flux-cli`) · **Owner:** Timo

This design documents **what it takes** to add an `aws` (AWS Bedrock) provider to flux, grounded in
live exploration against the dev account (IAM Identity Center / SSO). It does not commit to an
implementation; it scopes the work, names the two design forks, and lists the smallest-first cut.

## TL;DR

Bedrock is the **lowest-cost new provider flux could add**, because the wire is already implemented:

> `bedrock invoke-model` on an Anthropic model returns **native Anthropic Messages JSON**
> (`{"type":"message","content":[{"type":"text",...}],"usage":{...}}`) — byte-for-byte the shape
> flux's existing `AnthropicMessages` codec (`crates/flux-providers/src/messages`) already
> produces and parses. Verified live: `invoke-model` with body
> `{"anthropic_version":"bedrock-2023-05-31","max_tokens":32,"messages":[{"role":"user",...}]}`
> returns the standard Messages response.

So the wire codec is **~90% reuse**. The real work is exactly two things, and they live on the
`Credential` axis, not the `WireCodec` axis:

1. **SigV4 request signing** — every Bedrock request is AWS-Signature-V4-signed (region `bedrock`,
   service `bedrock`). flux's provider abstraction hands the `Credential::apply` a
   `reqwest::RequestBuilder`; a `BedrockCredential` signs the final request.
2. **The AWS credential chain** — the dev account is **SSO-only** (IAM Identity Center), so a
   static-key-only credential reader is *not* enough to "just work" here. This is the design fork
   that decides how heavy the dependency footprint gets.

The streaming framing adds one new parser (AWS binary event-stream) as a thin adapter in front of
the existing Anthropic SSE mapper.

## Why a Bedrock provider at all

- **Enterprise reach.** Bedrock is the compliance-friendly path to Claude (and Llama/Mistral) for
  orgs that cannot send data to `api.anthropic.com` directly. A flux `aws` provider lets the same
  agent harness target a Bedrock-provisioned Claude with no workflow change.
- **Same models, different billing.** Bedrock Anthropic rates match the direct Anthropic per-1M-token
  rates; flux's C-05 cost model already prices Claude — only a pricing-table prefix entry is needed.
- **Reuse.** Because the response is the Anthropic Messages shape, the entire `messages` module
  (body builder, SSE mapper, quirks profiles, thinking/cache/tool support) carries over unchanged.

## What already works — verified live (dev account, SSO)

```
$ aws bedrock-runtime invoke-model \
    --model-id 'us.anthropic.claude-sonnet-4-6' --region us-east-1 \
    --body $(base64 -w0 body.json) /tmp/out.json
# /tmp/out.json:
{
  "model": "claude-sonnet-4-6",
  "id": "msg_bdrk_01URQpgxPDXyS9UTDUAdmkiy",
  "type": "message", "role": "assistant",
  "content": [{"type":"text","text":"ok"}],
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 14, "output_tokens": 4,
    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
    "cache_creation": {"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":0}
  }
}
```

Observations that scope the work:

- **Response = native Messages JSON.** `content[]`, `stop_reason`, `usage.input_tokens` /
  `output_tokens` / `cache_*` all line up with `flux_core::Usage` and the Messages codec.
- **Body carries `anthropic_version: "bedrock-2023-05-31"`** *instead of* the `anthropic-version`
  header. flux's codec currently emits the version via `wire_headers()`; the Bedrock codec must
  instead inject it into the body and emit **no** `anthropic-version` header. (One quirks flag.)
- **Model ids are namespaced** (`us.anthropic.claude-sonnet-4-6`, `anthropic.claude-haiku-4-5-...`).
  Cross-region inference profiles carry a region-prefix (`us.` / `eu.` / `global.`) and must be
  invoked by that id against one Bedrock runtime endpoint — they are not resolvable to a bare
  foundation-model id. `resolve_model` owns the alias map (`sonnet`→the active cross-region sonnet,
  etc.).
- **Newer models require an inference profile** — direct foundation-model ids like
  `anthropic.claude-haiku-4-5-20251001-v1:0` reject on-demand with
  *"Invocation of model ID … with on-demand throughput isn't supported. Retry with the ID or ARN of
  an inference profile."* The resolver must prefer the `us.*` / `global.*` inference-profile ids.
  Legacy models (claude-3-sonnet/haiku) are marked *"Legacy"* and **not usable** on this account —
  the resolver must not alias to them.
- **Streaming** uses `POST /model/{modelId}/invoke-with-response-stream`, returning
  `application/vnd.amazon.eventstream` — AWS **binary** event-stream framing — whose `PayloadPart`
  blobs are the raw Anthropic SSE bytes (`event: content_block_delta\ndata: {...}\n\n`).
  Concatenating the PayloadPart bytes yields a complete Anthropic SSE stream → flux's existing
  `map_messages_stream` parses it unchanged. One new decoder sits in front: AWS event-stream →
  concatenated bytes → existing mapper.

## The two design forks

### Fork 1 — SigV4 signing: hand-roll vs. SDK

| | Hand-rolled SigV4 | `aws-sigv4` crate |
|---|---|---|
| **New deps** | none (`sha2`+`hmac`+`base64` already in the tree via `flux-secret`/rustls) | `aws-sigv4` + `aws-credential-types` + transitive |
| **Lines** | ~150 (canonical-request + string-to-sign + signing-key + HMAC chain) | ~10 (`sign(...)` call) |
| **Correctness risk** | real (subtle: host canonicalization, query sort, `x-amz-content-sha256`, session-token header) | low (maintained by AWS) |
| **fit with flux** | matches "minimal deps / the LLM is not the runtime" | adds a curated-but-heavy AWS dep slice |

**Recommendation:** hand-roll SigV4. It is a closed, stable, 15-year-old algorithm; the crypto is
just HMAC-SHA256 (already a dep); and flux's provider abstraction gives us the request bytes to sign
at a clean seam (`Credential::apply`). SigV4 is *not* something that drifts, so the maintenance cost
of hand-rolling is ~zero, and it keeps the dep surface flat (flux today pulls zero AWS SDK crates).
Cover it with a **known-answer test** (an AWS-documented canonical example) so the implementation is
pinned, not vibes.

### Fork 2 — credential chain: static-only vs. full SSO

This is the fork that matters most, because **the dev account is SSO-only**:

```
[profile babelforce-dev]
sso_session = babelforce
sso_account_id = <redacted>
sso_role_name = DeveloperAccess
[sso-session babelforce]
sso_start_url = <redacted IdP start url>
sso_region = eu-central-1
```

There are **no static keys** in `~/.aws/credentials` (the file has no profiles). A static-key-only
reader (`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN` + `~/.aws/credentials`)
**cannot drive the dev account** without a manual bootstrap. The SSO path is: `aws sso login`
(browser OIDC → cached token in `~/.aws/sso/cache/*.json`) → `sso:CreateToken` refresh →
`sso:GetRoleCredentials` (returns the role's `accessKeyId`/`secretAccessKey`/`sessionToken`, cached
for the role-session duration, typically 8h). Hand-rolling that is ~300 error-prone lines (OIDC
refresh token handling, token expiry, role-credential caching/refresh).

| | Static-only + manual SSO bootstrap | `aws-config` chain (full SSO/IMDS) |
|---|---|---|
| **New deps** | none | `aws-config` + `aws-credential-types` + `aws-sdk-sso` + transitive (the heaviest slice) |
| **Drives dev account directly?** | **No** — user runs `aws configure export-credentials --profile babelforce-dev` (or `aws sso login` + `aws sts get-session-token`) to materialize short-lived keys into env, then flux reads them | **Yes** — reads `~/.aws/config` SSO, refreshes, fetches role creds |
| **Lines** | ~40 (env + `~/.aws/credentials` profile read) | ~5 (`SdkConfig::builder().load_from_env().await`) |
| **fit with flux** | matches minimal-deps stance; SSO is a **documented bootstrap step** | pulls a large AWS dep tree into L1 |

**Recommendation (smallest-first):** **static-only credential reader**, with the SSO bootstrap
documented as a one-time `aws configure export-credentials` step (it writes short-lived keys to env
or a `--format env` dump the user sources). This keeps L1 dep-free of the AWS SDK, lands the
provider for the common case (long-lived IAM keys, env-injected SSO-derived keys, EC2 IMDS-via-env
for completeness later), and defers the in-process SSO flow to a follow-up story if the manual
bootstrap proves too frictional. The fork is **reversible** — the `BedrockCredential` trait impl is
the only seam, so swapping the static reader for `aws-config` later changes one file.

The thing to *not* do is half-implement SSO: a buggy hand-rolled OIDC refresh is a worse outcome
than an honest static reader with a documented bootstrap.

## Architecture (how it slots in)

Following the C-03 precedent (each provider owns its own module + `resolve_model`; the CLI owns only
the bare-alias shorthand policy):

```
flux-providers (L1)
└── src/bedrock.rs            ← NEW module: SigV4 credential, BedrockAnthropic codec,
                                 resolve_model, aws-event-stream adapter, *_from_env()
    reuses crate::messages    ← body builder + SSE mapper (unchanged)
    reuses flux-secret crypto ← sha2/hmac/base64 (already deps)
└── src/lib.rs                ← pub mod bedrock;

flux-credentials (L1)
└── src/lib.rs                ← bedrock_credential_from_env() — env + ~/.aws/credentials
                                 profile read (static keys only, v1)

flux-cli (L6)
└── src/main.rs               ← "aws" in KNOWN_PROVIDERS;
                                 bedrock_from_env() in build_provider;
                                 bare "aws"/"bedrock" shorthand → provider default model

flux-core (L0)
└── src/pricing.rs            ← bedrock/anthropic.* rate entries (match direct Anthropic rates)
```

### The codec — `BedrockAnthropic` (new, ~80 lines)

```rust
pub struct BedrockAnthropic;

impl WireCodec for BedrockAnthropic {
    fn build_body(&self, req: &Request) -> Result<Value> {
        // Reuse the shared body builder, then move anthropic_version from header → body.
        let mut body = build_messages_body(req, &BedrockProfile.quirks_for(&req.model))?;
        body["anthropic_version"] = json!("bedrock-2023-05-31");
        Ok(body)
    }
    fn map_stream(&self, bytes: ByteStream) -> ChunkStream {
        // AWS event-stream (binary) → concatenate PayloadPart bytes → existing Messages SSE mapper.
        map_bedrock_event_stream(bytes, map_messages_stream)
    }
    fn wire_headers(&self) -> Vec<(&'static str, String)> {
        Vec::new()  // NO anthropic-version header (it's in the body)
    }
}
```

`BedrockProfile` is a `ProviderProfile` with the full Anthropic feature set (prompt caching,
adaptive thinking, effort) — Bedrock passes these through to the same Anthropic backend. The only
quirks flag worth adding: `anthropic_version_in_body: bool` (or just override in the codec, as above)
so the shared `build_messages_body` doesn't need a Bedrock special-case.

### The credential — `BedrockSigV4` (new, ~150 + 80 lines)

```rust
pub struct BedrockSigV4 {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,  // present for SSO-derived + assumed-role creds
    pub region: String,                  // e.g. "us-east-1"
    pub model_id: String,                // e.g. "us.anthropic.claude-sonnet-4-6"
}

#[async_trait]
impl Credential for BedrockSigV4 {
    fn endpoint(&self) -> String {
        format!("https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke-with-response-stream",
                self.region, url::encode(&self.model_id))
    }
    async fn apply(&self, rb: RequestBuilder) -> Result<RequestBuilder> {
        // sign_v4(req, &self.access_key, &self.secret_key, self.session_token.as_deref(),
        //         region=&self.region, service="bedrock", body_sha256)
        Ok(sign_v4(rb, self).await?)
    }
}
```

- `sign_v4` is a free function in `bedrock.rs` (~150 lines): canonical request → string-to-sign →
  AWS4-HMAC-SHA256 signing key → signature → `Authorization` header. Also sets `x-amz-date`,
  `x-amz-content-sha256`, and `x-amz-security-token` when a session token is present. Pinned by a
  **known-answer test** using an AWS-documented example (service `bedrock`, region `us-east-1`).
- `resolve_model` lives here (per the C-03 "provider owns its resolution" rule): maps `sonnet` →
  `us.anthropic.claude-sonnet-4-6`, `opus` → `us.anthropic.claude-opus-4-6-v1`, `haiku` →
  `global.anthropic.claude-haiku-4-5-20251001-v1:0`, pass-through otherwise. Never aliases to the
  legacy claude-3 ids (rejected by the account).

### The streaming adapter — `map_bedrock_event_stream` (new, ~120 lines)

AWS event-stream is a binary framed format: each message has headers (incl. `:message-type`,
`:event-type`) + a payload. The `:event-type == "chunk"` / `PayloadPart` payloads for Anthropic
models are the raw Anthropic SSE text. The adapter:

1. Decodes AWS event-stream frames from the byte stream (a small ~100-line decoder; no dep — the
   format is a documented length-prefixed header map + payload + CRC, with `aws-smithy-eventstream`
   available as an opt-but-skippable dep).
2. Concatenates the `PayloadPart` payloads.
3. Feeds the concatenated bytes to the existing `map_messages_stream` (which expects an Anthropic
   SSE byte stream).

So the existing SSE mapper is reused unchanged — the adapter is purely a deframer.

### `map_messages_stream` parity

The existing mapper already produces `Chunk::ThinkingDelta` (Bedrock streams thinking the same way
as direct Anthropic, including the `signature` continuity blob), `Chunk::Usage` (the `usage` fields
line up), and tool-use blocks. The C-05 cost model prices the resolved `aws/...` spec from the
pricing table. **No changes to `flux-core`, `flux-lang`, or the agent loop** — Bedrock is just
another `Provider`.

## Acceptance (for the implementation story)

- [ ] `aws/<model-id>` (and bare `aws`) resolves via `flux_providers::bedrock::resolve_model` and
  completes a turn against the live dev account (failing-first: a mock-provider test asserts the
  body carries `anthropic_version: "bedrock-2023-05-31"` and emits **no** `anthropic-version` header;
  a live smoke confirms a real `us.anthropic.claude-sonnet-4-6` turn).
- [ ] `sign_v4` is pinned by a known-answer test against an AWS-documented SigV4 example (fails
  before the signing key derivation is correct).
- [ ] `map_bedrock_event_stream` decodes a recorded AWS event-stream fixture into Anthropic SSE
  bytes and the existing `map_messages_stream` parses it to `Chunk`s (failing-first: a fixture
  test that breaks if the deframer drops a PayloadPart).
- [ ] `cargo test -p flux-codegate` stays green — Bedrock lives in L1; no new cross-layer edge.
- [ ] Pricing: the `aws/anthropic.*` rate entries resolve in `flux_core::pricing` (Bedrock Anthropic
  rates match direct Anthropic); a live codex-style smoke shows the cost suffix on a Bedrock turn.
- [ ] SSO bootstrap is documented (one line in the provider story + README pointer): run
  `aws configure export-credentials --profile <p> --format env` (or set
  `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`/`AWS_REGION`) before `flux run -m aws`.

## Risks / open questions

- **AWS event-stream decoder dep.** `aws-smithy-eventstream` is the canonical decoder (~adds a
  dep). A hand-rolled decoder (~100 lines, the format is simple and stable) avoids it. Decide per
  the minimal-deps preference; either is acceptable — the seam is the `map_bedrock_event_stream`
  function signature.
- **Region / inference-profile resolution.** Cross-region profiles (`us.` / `global.`) are invoked
  against a single regional Bedrock runtime endpoint but route internally. The region the user
  passes (`AWS_REGION` / `--region`) must be one that serves the profile. Edge: a `global.` profile
  invoked against `us-east-1` works; an `eu.` profile against `us-east-1` may not. Document, don't
  silently rewrite.
- **Inference-profile vs foundation-model id ambiguity.** `resolve_model` must know which models
  need a profile (newer Claude 4/5) vs which accept a bare foundation-model id. The safest default
  is to always alias to the cross-region inference-profile id (the live test showed foundation-model
  ids for Claude 4/5 are rejected on-demand). The list of "active" profiles is queryable via
  `bedrock list-inference-profiles` but hardcoding the current set is acceptable for v1 (they change
  slowly; a stale alias fails loudly with a clear AWS error).
- **Layering.** `flux-credentials` resolving a Bedrock credential is fine (L1). SigV4 signing is
  pure crypto (no IO) — it could even live in `flux-secret` (L0) if we want it testable without L1,
  but keeping it in `flux-providers::bedrock` avoids an L0→nothing edge and matches where it's used.
- **Token refresh / 401.** Static keys don't expire (long-lived IAM) or expire as a unit (SSO
  role creds, ~8h). On a 401 with no `token_source`, there's nothing to refresh — surface the AWS
  error. The C-04 force-refresh-on-401 path is a no-op for Bedrock v1 (no `TokenSource`).

## Smallest-first cut (recommended story breakdown)

1. **C-09a — Bedrock SigV4 + static-key credential (L1).** `bedrock.rs`: `sign_v4` + known-answer
   test, `BedrockSigV4` credential, `BedrockAnthropic` codec (reuses `messages`), `resolve_model`.
   Drives the dev account via the SSO bootstrap (export-credentials → env). Live smoke green.
2. **C-09b — AWS event-stream deframer + streaming (L1).** `map_bedrock_event_stream`, fixture
   test, wire `invoke-with-response-stream`. Streaming turn green.
3. **C-09c — Pricing + CLI routing (L0+L6).** `aws` in `KNOWN_PROVIDERS`, `bedrock_from_env()`,
   bare `aws` shorthand, `aws/anthropic.*` pricing entries, README/SSO-bootstrap docs.

(C-09a alone is enough for a working `flux run -m aws` with non-streaming fallback; b makes it
stream; c makes it first-class. Splitting keeps each shippable and each gate-green.)

## What this is *not*

- Not an in-process SSO/OIDC flow (deferred — manual bootstrap for v1).
- Not the Converse API (the normalized AWS schema). InvokeModel + native Anthropic Messages reuses
  the codec; Converse would be a separate, larger codec for non-Anthropic Bedrock models (Meta
  Llama, Mistral) and is out of scope until flux needs a non-Anthropic Bedrock model.
- Not IMDS / EC2 instance-role credentials (deferred — same chain-implementation question as SSO).

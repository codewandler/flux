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
| **New deps** | none (`sha2`+`hmac`+`base64` already in the tree via `flux-secret`/rustls) | `aws-sigv4` (focused signer, lighter than the full chain) |
| **Lines** | ~150 (canonical-request + string-to-sign + signing-key + HMAC chain) | ~10, but needs `http::Request` round-trip from `reqwest` |
| **Correctness risk** | real but bounded (subtle: host canonicalization, query sort, `x-amz-content-sha256`, session-token header) | low (maintained by AWS) |
| **fit with flux** | clean seam with `reqwest::RequestBuilder` (method/url/headers/body in hand) | AWS signers target `http::Request`, not `reqwest`, so the path is `reqwest`→`http`→sign→`http`→`reqwest` |

**Recommendation:** hand-roll SigV4 **even though AWS crates are now pulled for the chain** (Fork 2).
SigV4 is a closed, stable, 15-year-old algorithm (canonical-request + two HMAC layers); the crypto is
just HMAC-SHA256 (already a dep); and flux's `Credential::apply` hands us a `reqwest::RequestBuilder`
with method/url/headers/body all in hand — a cleaner seam than converting to `http::Request` and back
for `aws-sigv4`. The signing-key derivation is pinned by a **known-answer test** (an AWS-documented
canonical example, service `bedrock`/region `us-east-1`) so the implementation is correct by
definition, not vibes. The risky part of AWS auth is the *credential chain* (Fork 2) — own that via
the SDK; the *signing* is the stable, cheap-to-own part.

### Fork 2 — credential chain: embedded SDK vs. plugin

This is the fork that matters most, and the deployment model **decides it**. flux must support two
modes, both of which a static-key reader **cannot** cover:

- **Dev — `aws sso login` (IAM Identity Center).** The dev account is SSO-only (`sso_session`/
  `sso_account_id`/`sso_role_name` in `~/.aws/config`; **no static keys** in `~/.aws/credentials`).
  The desired workflow is `aws sso login` once → flux reads the cached SSO token, refreshes it, and
  fetches role credentials — *no* per-session `aws configure export-credentials` step. That SSO path
  is: read `~/.aws/config` sso block → refresh the cached access token (`~/.aws/sso/cache/*.json`)
  via `sso-oidc:CreateToken` → `sso:GetRoleCredentials` → `{accessKeyId, secretAccessKey,
  sessionToken}` with ~8h expiry, refreshing on expiry. Hand-rolling that correctly (refresh-token
  handling, the sso cache format, expiry races) is ~250 error-prone lines.
- **Prod — k8s auto-injected credentials.** Two mechanisms are in widespread use (confirm which your
  infra uses, but the chain handles *both* so flux need not know):
  - **IRSA** (IAM Roles for Service Accounts): a webhook injects `AWS_ROLE_ARN` +
    `AWS_WEB_IDENTITY_TOKEN_FILE` (a projected SA-token JWT); the app calls STS
    `AssumeRoleWithWebIdentity` to exchange the JWT for role creds. Most common on EKS today.
  - **EKS Pod Identity** (newer, replacing IRSA): the webhook injects
    `AWS_CONTAINER_CREDENTIALS_FULL_URI` + `AWS_TOKEN_AUTHORIZATION`; the app GETs that URI → JSON
    creds.
  A static-key reader sees **none** of these env vars — IRSA/EKS Pod Identity use the web-identity
  / HTTP-endpoint sources, not `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`.

There are now **three** honest places to put the credential chain, and a shared seam (below) keeps
the choice swappable. **SigV4 signing and the Messages codec stay hand-rolled in L1 in all three** —
only the *credential source* moves.

#### Option A — embedded `aws-config` in `flux-providers`, behind a `bedrock` feature

- ~5 lines to load the default chain. AWS SDK deps compile into `flux-providers` when the feature
  is on, and into `flux-cli` (which enables it). Same gate `realtime` already uses in this crate.
- Drives dev SSO ✓ and prod k8s IRSA/EKS Pod Identity ✓ (the `aws-config` chain).
- **Cost:** AWS SDK deps enter the flux build graph (behind a feature); and `aws-config` makes its
  STS/SSO HTTP calls with **its own reqwest**, bypassing `flux_system::net::guard`. (Routing those
  through the guarded net means plugging a custom AWS HTTP client into `aws-config` — at which point
  you've done the hard part of Option C in the wrong crate.)

#### Option B — plugin shells to the user's `aws` CLI (reuse the existing `aws` plugin pattern)

- **Zero AWS SDK deps anywhere in flux.** The existing `plugins/aws` plugin already declares
  `process: vec!["aws"]` and shells to the user's installed `aws` binary for its EC2/datasource ops.
  Bedrock auth reuses that: the `aws` CLI **owns the entire credential chain** (SSO refresh,
  `AssumeRoleWithWebIdentity`, EKS Pod Identity, IMDS) — flux re-implements none of it.
- The plugin gains an `auth` op: `host.run("aws", ["configure","export-credentials","--format","env","--profile",profile])`
  → parse `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`/`AWS_REGION` → return to
  the host. The host registers the keys with the `Redactor` and hands them to `flux-providers::bedrock`,
  which hand-rolls SigV4 and makes the Bedrock HTTP call itself (via reqwest, like every provider).
- Creds are cached in the long-lived plugin process; only re-shells on expiry (~8h) or a 401. Steady
  state is one IPC round-trip returning cached creds, not an `aws` spawn per turn.
- **Drives dev SSO ✓** (the `aws` CLI is present locally). **Prod k8s: ✓ *iff* the flux image bundles
  the `aws` CLI**; ✗ if the container has no `aws` binary (then Option C is needed). This is the one
  deployment constraint that decides B vs C.
- **Two small, precedent-backed protocol extensions are required** (this is the honest cost, not
  zero-protocol):
  1. **A host-only/internal op channel.** `OperationSpec` has no `internal`/`host_only` flag today —
     every op "becomes a tool projected to the agent". An `auth` op returning raw AWS keys must **not**
     be LLM-callable (the model would call it and the keys would appear in the tool result → leak).
     The D-27 `credential` capability + Redactor-registration pattern is the precedent (it already
     keeps a materialized secret out of model-visible output); B needs its *plugin→host* counterpart
     — an `internal: true` flag on `OperationSpec` (op not advertised to the LLM, host-dispatchable)
     with its result fed to the `Redactor`. Small, and `contribute` is already a host-directed
     (non-LLM-tool) plugin output, so the shape isn't foreign.
  2. **`process.run` accepting an optional `env`.** Today only `process_spawn` takes env overrides;
     one-shot `process.run` passes the cleared+allow-listed env only, so `AWS_PROFILE`/`AWS_REGION`/
     `AWS_ROLE_ARN` can't reach the `aws` child. Either add an optional `env` field to `process.run`
     (parity with `process_spawn`) or resolve via `process_spawn`+`process_read`.

#### Option C — plugin with embedded `aws-config`, resolving the chain over host callbacks

- The `aws-config` deps live in **the plugin binary** (`plugins/aws-bedrock/Cargo.toml`, the nested
  workspace excluded from the root gate — AUTHORING.md: "Heavy vendor deps live here, never in the
  root flux gate"). The plugin is installed where flux runs, so **prod k8s needs no `aws` CLI in the
  image** — the plugin is the resolver.
- Drives dev SSO ✓ and prod k8s IRSA/EKS Pod Identity ✓ — the full `aws-config` chain, in the plugin.
- The plugin must service `aws-config`'s IO **through host callbacks** (the plugin rule: no `reqwest`,
  no `std::fs`): a custom AWS `HttpClient` impl over `host.http.do` (so STS/SSO calls traverse
  `flux_system::net::guard`, not a bypass reqwest), and a **new scoped `fs.read` capability** for
  `~/.aws/config` + `~/.aws/sso/cache` (none exists today — `blob.*` is a scratch store, not a
  general path read). Plus the same host-only-op channel as B for returning the creds.
- **Cost:** the most new protocol surface (custom AWS HTTP client + `fs.read` capability +
  host-only ops), but the most architecturally pure — zero AWS deps in core, every byte of AWS IO
  through the guarded envelope, no dependency on an external `aws` CLI binary.

| | A — embedded (`bedrock` feature) | B — plugin → `aws` CLI | C — plugin embeds `aws-config` |
|---|---|---|---|
| **AWS SDK deps in flux core** | yes (feature-gated) | **none** | **none** |
| **Dev SSO** | ✓ (aws-config) | ✓ (aws CLI) | ✓ (aws-config in plugin) |
| **Prod k8s (IRSA / EKS Pod Identity)** | ✓ (aws-config) | ✓ **iff `aws` CLI in image** | ✓ (no `aws` CLI needed) |
| **AWS IO through `flux_system` guard** | ✗ (aws-config's own reqwest) | ✓ (SigV4 HTTP in flux-providers, via guarded net) | ✓ (custom HTTP client over host `http.do`) |
| **New protocol surface** | none | 2 small (host-only op, `process.run` env) | 3 (the two + `fs.read` + AWS HTTP client) |
| **`flux plugin aws-bedrock auth` UX** | ✗ | ✓ | ✓ |
| **Reusable cred provider for non-LLM AWS** | ✗ | ✓ (one aws plugin) | ✓ |
| **Lines / lift** | smallest | small + 2 protocol knobs | largest |

#### Recommendation: Option B smallest-first, Option C as the documented escalation

The user's framing — *"aws plugin — needs to be installed, holds all the credentials, and also
provides credentials providers … `flux plugin aws-bedrock auth`"* — maps directly to the plugin
options and is the most flux-aligned: it isolates AWS deps from the root gate (AUTHORING.md policy),
keeps the plugin process env-cleared (it never reads host secrets from `std::env`; the `aws` CLI it
shells to is itself env-cleared by the guarded spawn path and resolves the chain from `~/.aws`), and
reuses the existing `plugins/aws` plugin's `process: ["aws"]` capability plus the D-27
`credential`-capability/Redactor precedent.

- **B first** because it is the smallest lift that stays in the plugin model and gives the `auth`
  UX, reusing the `aws` CLI as the universal chain resolver (SSO + IRSA + EKS Pod Identity + IMDS).
  It covers both stated modes **provided the flux image bundles the `aws` CLI** — which is the one
  thing to confirm against your prod k8s setup.
- **C is the escalation** if a prod container can't carry the `aws` CLI: then the plugin embeds
  `aws-config` and becomes the resolver itself. The two share the exact same L1 seam
  (`BedrockCredentialsResolver`), so moving B→C changes the plugin, not `flux-providers::bedrock`.
- **A is rejected** as the primary path: it puts AWS deps in the flux build graph and lets
  `aws-config` bypass `net::guard`. (A remains a valid *fallback* if the plugin protocol work proves
  out of scope for a given release — the seam makes the swap one file.)

The thing to *not* do is half-implement any of them: a static-key reader (the earlier v1 idea)
  covers neither mode; a plugin `auth` op without the host-only channel leaks keys to the model; an
  embedded `aws-config` without a guarded HTTP client bypasses the net guard. Pick B, land the two
  small protocol knobs, and escalate to C only if the prod image can't carry `aws`.

#### The unifying seam — `BedrockCredentialsResolver` (L1, all options share it)

Regardless of A/B/C, `flux-providers::bedrock` (L1) owns the **wire codec + hand-rolled SigV4 + the
`BedrockCredentialsResolver` trait**, and the *credential source* is injected at the CLI seam —
exactly like `TokenSource` is injected for OAuth providers. This makes A/B/C swappable at one trait:

```rust
/// Resolves AWS credentials + region for a Bedrock request. Implemented by:
///   - Option A: an `aws-config`-backed resolver (L6, behind the `bedrock` feature)
///   - Option B: a resolver that calls the `aws` plugin's `auth` op (L6 → plugin host)
///   - Option C: an `aws-config`-in-plugin resolver proxied through the plugin host (L6 → plugin)
#[async_trait]
pub trait BedrockCredentialsResolver: Send + Sync {
    async fn resolve(&self) -> Result<BedrockCreds>;
    /// Force a refresh (the C-04 401 path). No-op for sources that can't.
    async fn refresh(&self) -> Result<()> { Ok(()) }
}
pub struct BedrockCreds {
    pub access_key: String, pub secret_key: String,
    pub session_token: Option<String>, pub region: String, pub expiry: Option<Instant>,
}
```

## Architecture (how it slots in)

Following the C-03 precedent (each provider owns its own module + `resolve_model`; the CLI owns only
the bare-alias shorthand policy):

```
flux-providers (L1)  — no AWS SDK deps; the credential source is injected.
└── src/bedrock.rs            ← NEW module: BedrockCredential (takes an injected
                                 BedrockCredentialsResolver), hand-rolled sign_v4, BedrockAnthropic
                                 codec, resolve_model, aws-event-stream adapter, *_with(resolver)
    reuses crate::messages    ← body builder + SSE mapper (unchanged)
    reuses flux-secret crypto ← sha2/hmac/base64 (already deps, for sign_v4)
└── src/lib.rs                ← pub mod bedrock;  (no feature gate — no AWS deps live here)

plugins/aws (L4)             ← gains an `auth` op (Option B): host.run("aws",
                                 ["configure","export-credentials","--format","env","--profile",p])
                                 → creds, returned on the host-only/credential channel (Redactor-registered).
                                 Already declares process: ["aws"]; EC2/datasource ops unchanged.
   (Option C escalation: a plugins/aws-bedrock member embeds aws-config + a host-callback HTTP client.)

flux-credentials (L1)         ← no AWS reader; the chain lives in the plugin. (Keeps the OAuth store.)

flux-cli (L6)
└── src/main.rs               ← "aws" in KNOWN_PROVIDERS; bedrock_from_env() builds a resolver that
                                 calls the aws plugin's `auth` op via the plugin host, injects it into
                                 BedrockCredential; bare "aws" shorthand → provider default model

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

### The credential — `BedrockCredential` (new; takes an injected resolver, ~60 lines)

```rust
pub struct BedrockCredential {
    model_id: String,
    resolver: Arc<dyn BedrockCredentialsResolver>,   // injected at the CLI seam (Option A/B/C)
}

#[async_trait]
impl Credential for BedrockCredential {
    fn endpoint(&self) -> String {
        // region comes from the resolver per-request (it lives with the creds).
        unimplemented!("see apply: endpoint needs the resolved region")
    }
    async fn apply(&self, rb: RequestBuilder) -> Result<RequestBuilder> {
        let creds = self.resolver.resolve().await?;          // aws CLI plugin (B) / aws-config (A/C)
        sign_v4(rb, &creds, service="bedrock").await        // hand-rolled SigV4 (region from creds)
    }
    // C-04 force-refresh-on-401: surface the resolver as a TokenSource whose refresh() calls
    // resolver.refresh() (for B: re-shells `aws configure export-credentials`; for A/C: the chain).
    fn token_source(&self) -> Option<Arc<dyn TokenSource>> { Some(self.as_token_source()) }
}
```

- `BedrockCredential` is **credential-source-agnostic** — it holds only `model_id` + the injected
  `BedrockCredentialsResolver`. The CLI picks the resolver: Option B wires a resolver that calls the
  `aws` plugin's `auth` op (creds returned on the host-only channel, Redactor-registered so the keys
  never appear in model-visible output); Option A/C wire an `aws-config`-backed resolver. In dev,
  `AWS_PROFILE=<p>` after `aws sso login` is enough (B shells `aws configure export-credentials
  --profile <p>`); in prod k8s the injected IRSA/EKS-Pod-Identity vars are enough (the `aws` CLI or
  the in-plugin chain resolves them). **No `flux-credentials` AWS reader and no manual
  `export-credentials` step from the user.**
- `sign_v4` is a free function in `bedrock.rs` (~150 lines): canonical request → string-to-sign →
  AWS4-HMAC-SHA256 signing key → signature → `Authorization` header. Sets `x-amz-date`,
  `x-amz-content-sha256`, and `x-amz-security-token` when a session token is present. Pinned by a
  **known-answer test** (AWS-documented example, service `bedrock`/region `us-east-1`).
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
- [ ] SSO (dev) and k8s-injected (prod) auth both work with **no manual `export-credentials`
      step**: dev uses `AWS_PROFILE=<p>` after `aws sso login` (Option B: the aws plugin shells
      `aws configure export-credentials --profile <p>`); prod uses the injected IRSA
      (`AWS_ROLE_ARN`+`AWS_WEB_IDENTITY_TOKEN_FILE`) or EKS Pod Identity
      (`AWS_CONTAINER_CREDENTIALS_FULL_URI`) vars, resolved by the `aws` CLI / in-plugin chain.
      Failing-first: a mock `BedrockCredentialsResolver` returns canned creds; a live smoke
      confirms a real `aws sso login`'d turn against the dev account.
- [ ] The `aws` plugin's `auth` op is **host-only/internal** (not advertised to the LLM as a tool)
      and its returned keys are registered with the `Redactor` — the model cannot call `auth` and
      the keys never appear in model-visible output. Failing-first: a test asserts the op is absent
      from the projected tool catalog; a redactor test asserts the keys are scrubbed.
- [ ] **Zero AWS SDK crates in the root flux gate** — `cargo build --workspace` (no features) pulls
      none; the chain lives in the `aws` plugin (Option B shells the user's `aws` CLI; Option C would
      add `aws-config` to the plugin's nested workspace, not the root).

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
- **Token refresh / 401.** The injected `BedrockCredentialsResolver.refresh()` is the C-04 path:
  Option B re-shells `aws configure export-credentials`; A/C re-resolve the chain. Static (long-lived
  IAM) keys don't expire; SSO role creds expire as a unit (~8h). On a 401 with a refreshable
  resolver, force one refresh + retry; otherwise surface the AWS error.

## Smallest-first cut (recommended story breakdown)

1. **C-09a — Plugin protocol knobs + `aws` plugin `auth` op (L4).** Add an `internal`/`host_only`
   flag to `OperationSpec` (op not advertised to the LLM; result fed to the `Redactor`) and an
   optional `env` to `process.run` (parity with `process_spawn`). Add the `auth` op to the existing
   `plugins/aws` plugin: `host.run("aws", ["configure","export-credentials","--format","env","--profile",p])`
   → parse creds → return on the host-only channel. Failing-first: a test asserts `auth` is absent
   from the projected tool catalog and its keys are redacted; a test asserts `process.run` forwards
   `env`.
2. **C-09b — Bedrock SigV4 + codec + injected resolver (L1).** `flux-providers::bedrock.rs`:
   `BedrockCredentialsResolver` trait, `BedrockCredential` (holds `model_id` + resolver), hand-rolled
   `sign_v4` + known-answer test, `BedrockAnthropic` codec (reuses `messages`), `resolve_model`.
   Drives the dev account via the aws plugin (`AWS_PROFILE=<p>` after `aws sso login`); a prod k8s
   pod via the injected IRSA / EKS Pod Identity vars (resolved by the `aws` CLI the plugin shells).
   CLI wires the plugin-backed resolver at the seam. Live smoke green against dev. (Non-streaming
   fallback first; streaming in C-09c.)
3. **C-09c — AWS event-stream deframer + streaming (L1).** `map_bedrock_event_stream`, fixture test,
   wire `invoke-with-response-stream`. Streaming turn green.
4. **C-09d — Pricing + CLI routing + docs (L0+L6).** `aws` in `KNOWN_PROVIDERS`, `bedrock_from_env()`,
   bare `aws` shorthand, `aws/anthropic.*` pricing entries, README docs (dev `AWS_PROFILE`+`aws sso
   login`; prod k8s injected vars; **Option C escalation** if the prod image can't carry the `aws` CLI).

(C-09a+b is enough for a working `flux run -m aws` across dev-SSO and prod-k8s; c makes it stream;
d makes it first-class. Option C — a `plugins/aws-bedrock` member embedding `aws-config` over host
callbacks — is the documented escalation, swapping only the resolver impl at the C-09b seam.)

## What this is *not*

- Not a hand-rolled SSO/web-identity credential chain (the `aws` CLI / `aws-config` owns that —
  hand-rolling four refresh sources is the bug class the SDK exists to absorb). Hand-rolled is
  only the SigV4 *signing* (stable crypto, known-answer pinned), not the chain.
- Not the Converse API (the normalized AWS schema). InvokeModel + native Anthropic Messages reuses
  the codec; Converse would be a separate, larger codec for non-Anthropic Bedrock models (Meta
  Llama, Mistral) and is out of scope until flux needs a non-Anthropic Bedrock model.
- Not IMDS / EC2 instance-role credentials as a *bespoke* path (the `aws` CLI / `aws-config`
  default chain already covers IMDS; Option B/C get it for free).

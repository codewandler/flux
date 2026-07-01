---
id: C-09
title: AWS Bedrock LLM provider
pillar: Core
status: in-progress
epic: aws-bedrock-provider
design: docs/designs/aws-bedrock-provider.md
note: DECISION = Option C (aws-bedrock plugin embeds aws-config, no aws CLI in prod). IMPLEMENTING in two non-colliding waves: (1) L1 core now — flux-providers::bedrock (SigV4 + Messages codec + BedrockCredentialsResolver trait + resolve_model) + L0 pricing + L6 routing, with an env-static resolver stand-in so `flux run -m aws` works against dev (aws configure export-credentials); (2) C-09a/b plugin + protocol knobs deferred — another session has the plugins/ workspace open (Cargo.lock collision). The L1 seam means the plugin resolver swaps in at one trait.
---

# AWS Bedrock LLM provider

## Goal
Add an `aws` (AWS Bedrock) provider so flux can drive Bedrock-provisioned Claude (and later Llama/
Mistral) through the same agent harness, authenticated via the AWS credential chain. The wire is
the Anthropic Messages shape flux already speaks, so this is a `Credential` + thin-codec story, not
a new-protocol story.

## Acceptance
- [ ] `aws/<model-id>` (and bare `aws`) resolves via `flux_providers::bedrock::resolve_model` and
      completes a turn against the live dev account. Failing-first: a mock-provider test asserts the
      request body carries `anthropic_version: "bedrock-2023-05-31"` and emits **no**
      `anthropic-version` header (breaks if the codec forgets the body move).
- [ ] `sign_v4` is pinned by a known-answer test against an AWS-documented SigV4 example
      (service `bedrock`, region `us-east-1`); fails before the signing-key derivation is correct.
- [ ] `map_bedrock_event_stream` decodes a recorded AWS event-stream fixture into Anthropic SSE
      bytes and the existing `map_messages_stream` parses it to `Chunk`s; fails if the deframer
      drops a `PayloadPart`.
- [ ] A live smoke: `flux run -m aws/us.anthropic.claude-sonnet-4-6` (dev: `AWS_PROFILE=<p>` after
      `aws sso login`) returns a turn with the cost suffix.
- [ ] `cargo test -p flux-codegate` green — Bedrock lives in L1; no new cross-layer edge.
- [ ] Pricing: `aws/anthropic.*` rate entries resolve in `flux_core::pricing` (match direct
      Anthropic rates); a Bedrock turn shows the `$`/`~$X (sub)` suffix.
- [ ] SSO (dev) and k8s-injected (prod) auth both work with **no manual `export-credentials` step**:
      dev uses `AWS_PROFILE=<p>` after `aws sso login`; prod uses the injected IRSA
      (`AWS_ROLE_ARN`+`AWS_WEB_IDENTITY_TOKEN_FILE`) or EKS Pod Identity
      (`AWS_CONTAINER_CREDENTIALS_FULL_URI`) vars. Failing-first: a mock test asserts
      `BedrockCredential` builds from a minimal `~/.aws/config` SSO block fixture; a live smoke
      confirms a real `aws sso login`'d turn against the dev account.
- [ ] The AWS SDK deps are behind an off-by-default `bedrock` feature in `flux-providers`
      (mirroring the `realtime` precedent); the default `cargo build` pulls no AWS SDK crates. The
      shipped `flux-cli` enables it. Dev/prod auth + the feature flag are documented in README + CLI help.

## Progress
- **C-09c (L1 core) + C-09e (pricing + CLI routing) LANDED** — the non-colliding wave. `flux run -m aws`
  works end-to-end against the dev account (live-verified: `say ok`→"ok"; opus alias; a real read-file
  tool-use turn). SigV4 + codec + resolver + resolve_model + pricing + CLI routing all in.
  - `crates/flux-providers/src/bedrock.rs` (new): `BedrockAnthropic` codec (reuses `messages`, injects
    `anthropic_version` in the body, **strips `model`+`stream`** — Bedrock invoke-model rejects both,
    caught by the live smoke), non-streaming `map_messages_json` (invoke-model returns one Messages
    JSON object → `Chunk`s), hand-rolled `sign_v4` (pinned by two cross-verified known-answer tests
    against an independent Python `hmac` impl), `BedrockCredentialsResolver` trait + `BedrockCreds`,
    `BedrockCredential` (Credential impl; model id baked into the URL), `EnvStaticResolver` (the
    stand-in), `bedrock_with_env` (sync constructor for the sync CLI), `resolve_model`.
  - `crates/flux-core/src/pricing.rs`: `aws/anthropic.*` rate entries (match direct Anthropic) + `aws`
    added to the L0 `known_provider` mirror (without it `split_provider` left the whole `aws/<id>`
    spec unsplit → `rates_for` missed). Failing-first test asserts `aws/us.anthropic.*` prices as
    metered, not subscription.
  - `crates/flux-cli/src/main.rs`: `aws` in `KNOWN_PROVIDERS`, bare `aws` shorthand, `build_provider`
    arm (resolves the model **before** constructing the credential — Bedrock bakes the id into the
    URL; the resolution match runs after the native-construction match, caught by the live smoke).
  - **Live bugs the smoke caught** (unit tests couldn't): (1) Bedrock rejects `model`+`stream` in the
    body; (2) the model id was empty in the URL because resolution ran after construction; (3) the
    L0 `known_provider` mirror didn't include `aws` so cost returned `None`. All fixed with
    failing-first tests.
  - Gate green: build/test --workspace, clippy -D warnings, fmt, flux-codegate.
- **C-09a/b (plugin + protocol knobs) DEFERRED** — another session has the `plugins/` workspace open
  (Cargo.lock collision: D-36 schemars migration across alertmanager/grafana/loki/prometheus). The L1
  seam (`BedrockCredentialsResolver`) means the `aws-bedrock` plugin resolver swaps in at one trait;
  `EnvStaticResolver` is the documented stand-in (dev: `aws configure export-credentials --profile <p>`).
- **C-09d (event-stream streaming) DEFERRED** — non-streaming `invoke-model` ships first (one Messages
  JSON object → chunks); the deframer (`invoke-with-response-stream` → AWS binary event-stream →
  existing `map_messages_stream`) is the follow-up.

## Notes
- Design + full scoping, the two forks, and the smallest-first breakdown (C-09a/b/c) live in
  [docs/designs/aws-bedrock-provider.md](../designs/aws-bedrock-provider.md).
- Wire reuse: `crates/flux-providers/src/messages` (body builder + SSE mapper) is unchanged; the
  Bedrock codec injects `anthropic_version` into the body and emits no version header.
- The real work splits cleanly across layers: **SigV4 + the Messages codec + a
  `BedrockCredentialsResolver` trait live in L1 `flux-providers::bedrock`** (hand-rolled, no AWS dep;
  signing pinned by a known-answer test), and the **credential chain lives in a new `aws-bedrock`
  plugin** (Option C: embeds `aws-config`, resolves SSO/IRSA/EKS-Pod-Identity/IMDS over host
  callbacks — no `aws` CLI needed in the image, which is the confirmed prod constraint). Zero AWS
  SDK deps in the flux core; all AWS IO (STS/SSO HTTP, `~/.aws` reads) through `flux_system`'s
  guarded envelope. The plugin's `auth` op is **host-only/internal** (not an LLM tool) and its keys
  are Redactor-registered. Three protocol pieces are required: (i) an `internal` flag on
  `OperationSpec`; (ii) a new path-scoped, deny-by-default `fs.read` capability for `~/.aws/config`
  + `~/.aws/credentials` + `~/.aws/sso/cache` (results Redactor-registered — the sso cache holds
  refresh tokens); (iii) an `aws-types::HttpClient` impl over `host.http.do` so STS/SSO traverse
  `net::guard`. **Option A** (embed `aws-config` in `flux-providers` behind a `bedrock` feature) is
  the fallback only if the plugin work is deferred — it pays the `net::guard` bypass cost.
- Precedent for module ownership: C-03 (each provider owns `resolve_model`; CLI keeps only bare
  shorthand policy). Bedrock gets its own `flux_providers::bedrock` module like `codex`/`anthropic`.
- Pricing: Bedrock Anthropic rates match direct Anthropic — add `aws/anthropic.*` entries to the
  `flux_core::pricing` builtin table (the C-05 cost model already prices Claude; this is a prefix).
- Out of scope: the Converse API (normalized AWS schema, for non-Anthropic Bedrock models), and a
  hand-rolled SSO/web-identity chain (the `aws-config` SDK owns that — IMDS included).

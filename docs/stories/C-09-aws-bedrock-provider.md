---
id: C-09
title: AWS Bedrock LLM provider
pillar: Core
status: backlog
epic: aws-bedrock-provider
design: docs/designs/aws-bedrock-provider.md
note: invoke-model returns native Anthropic Messages JSON — reuse the messages codec; SigV4 is hand-rolled in L1, the credential chain lives in a new aws-bedrock plugin embedding aws-config over host callbacks (no aws CLI in prod; all AWS IO through net::guard; host-only auth op so keys don't leak)
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
- (scoping complete — design doc filed; implementation not started)

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

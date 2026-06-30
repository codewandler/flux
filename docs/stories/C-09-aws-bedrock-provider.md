---
id: C-09
title: AWS Bedrock LLM provider
pillar: Core
status: backlog
epic: aws-bedrock-provider
design: docs/designs/aws-bedrock-provider.md
note: invoke-model returns native Anthropic Messages JSON — reuse the messages codec; the work is SigV4 signing + AWS credential chain (dev account is SSO-only)
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
- [ ] A live smoke: `flux run -m aws/us.anthropic.claude-sonnet-4-6` (via the documented SSO
      bootstrap) returns a turn with the cost suffix.
- [ ] `cargo test -p flux-codegate` green — Bedrock lives in L1; no new cross-layer edge.
- [ ] Pricing: `aws/anthropic.*` rate entries resolve in `flux_core::pricing` (match direct
      Anthropic rates); a Bedrock turn shows the `$`/`~$X (sub)` suffix.
- [ ] SSO bootstrap documented (provider story note + README pointer): run
      `aws configure export-credentials --profile <p> --format env` (or set
      `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`/`AWS_REGION`) before `flux run`.

## Progress
- (scoping complete — design doc filed; implementation not started)

## Notes
- Design + full scoping, the two forks, and the smallest-first breakdown (C-09a/b/c) live in
  [docs/designs/aws-bedrock-provider.md](../designs/aws-bedrock-provider.md).
- Wire reuse: `crates/flux-providers/src/messages` (body builder + SSE mapper) is unchanged; the
  Bedrock codec injects `anthropic_version` into the body and emits no version header.
- The real work is on the `Credential` axis: `sign_v4` (hand-rolled, ~150 lines, pinned by a
  known-answer test — `sha2`/`hmac`/`base64` already deps) + a static-key credential reader
  (`AWS_*` env + `~/.aws/credentials`). The dev account is **SSO-only**, so v1 requires the
  `aws configure export-credentials` bootstrap; in-process SSO/OIDC is deferred.
- Precedent for module ownership: C-03 (each provider owns `resolve_model`; CLI keeps only bare
  shorthand policy). Bedrock gets its own `flux_providers::bedrock` module like `codex`/`anthropic`.
- Pricing: Bedrock Anthropic rates match direct Anthropic — add `aws/anthropic.*` entries to the
  `flux_core::pricing` builtin table (the C-05 cost model already prices Claude; this is a prefix).
- Out of scope: the Converse API (normalized AWS schema, for non-Anthropic Bedrock models),
  in-process SSO/OIDC refresh, IMDS/EC2 instance roles.

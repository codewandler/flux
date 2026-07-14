---
id: C-63
title: Preserve lazy Bedrock credential refresh in shared construction
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: the shared model-spec factory snapshots temporary AWS credentials and can panic in current-thread Tokio
---

# Preserve lazy Bedrock credential refresh in shared construction

## Goal

Make every synchronous Bedrock provider factory use the existing lazy, expiry-aware credential chain
without blocking a Tokio runtime or mutating process-global AWS credentials.

## Acceptance

- [x] `flux_providers::spec::build("aws/...")` constructs through `bedrock_with_chain` (or an
      equivalent lazy resolver) and performs no credential resolution at construction.
- [x] Failing-first tests construct the public spec factory inside both current-thread and
      multi-thread Tokio runtimes without panic or `block_in_place`.
- [x] Failing-first tests prove factory construction does not set/change `AWS_ACCESS_KEY_ID`,
      `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, or region environment variables.
- [x] An expiring-resolver test through the public factory proves fresh credentials are cached and
      near-expiry credentials are re-resolved, preserving C-37's lifecycle guarantee.
- [x] Region selection remains deterministic before first resolution and agrees with SigV4 scope;
      static environment credentials still work.
- [x] The materialize-into-env compatibility path is deleted or isolated from production provider
      construction, and CLI/server/sub-agent model switching all use the shared fixed factory.

## Progress

- 2026-07-14 — The public model-spec factory now preserves Bedrock's lazy resolver and deterministic
  region selection without environment mutation. Public-factory runtime, environment, and two-call
  refresh/cache lifecycle tests cover the repaired construction path.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Regression follow-up to [C-37](C-37-bedrock-credential-lifecycle.md), introduced when D-152
  centralized model-spec construction.

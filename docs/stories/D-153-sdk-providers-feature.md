---
id: D-153
title: SDK `providers` feature — one-stop provider construction
pillar: Agent
status: backlog
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 3 — feature-gated batteries; default build stays provider-agnostic; ⚠ publish-order flip"
---

# SDK `providers` feature — one-stop provider construction

## Goal
`cargo add codewandler-flux-sdk --features providers` is a one-stop shop:
`flux_sdk::providers::{*, from_spec}` re-exports flux-providers and the D-152 spec resolver. The
default build keeps zero provider deps.

## Acceptance
- [ ] Failing-first: with the feature, `providers::from_spec("ollama/qwen3")` returns a working
      provider; without it, the module does not exist.
- [ ] `cargo tree -p codewandler-flux-sdk` (no features) shows no flux-providers — assert in a
      test or CI check so the lean default is enforced, not aspirational.
- [ ] `scripts/publish-crates-io.sh`: `providers` moved BEFORE `sdk` (optional deps must be
      published first; topo order flips with this dep) + PUBLISHING.md order updated.

## Progress
- (pending)

## Notes
- Blocked by D-152. The realtime voice provider stays behind flux-providers' own `realtime`
  feature — do not flatten features across the boundary.

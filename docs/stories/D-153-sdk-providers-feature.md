---
id: D-153
title: SDK `providers` feature — one-stop provider construction
pillar: Agent
status: done
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
- [x] Failing-first: with the feature, `providers::from_spec("ollama/qwen3")` returns a working
      provider; without it, the module does not exist.
- [x] `cargo tree -p codewandler-flux-sdk` (no features) shows no flux-providers — assert in a
      test or CI check so the lean default is enforced, not aspirational.
- [x] `scripts/publish-crates-io.sh`: `providers` moved BEFORE `sdk` (optional deps must be
      published first; topo order flips with this dep) + PUBLISHING.md order updated.

## Progress
- **Done (unreleased).** New opt-in `providers` cargo feature (`providers = ["dep:flux-providers"]`,
  `default = []`). `flux_sdk::providers` (`crates/flux-sdk/src/lib.rs`) re-exports the concrete
  backends + the D-152 `spec` resolver and adds `from_spec(spec) -> Result<(Box<dyn Provider>, String)>`
  (boxed provider + resolved model), ready for `Client::builder().model(model).build(provider, root)`.
  Realtime voice stays behind flux-providers' own `realtime` feature (not flattened, per Notes).
- Lean default **enforced, not aspirational**: `default_build_pulls_no_optional_provider_batteries`
  parses the manifest (`include_str!`) and asserts `default = []` + both batteries `optional`; also
  verified `cargo tree -p codewandler-flux-sdk -e no-dev` = 0 flux-providers (1 with `--features providers`).
- Publish-order flip: `scripts/publish-crates-io.sh` + `PUBLISHING.md` now list
  `codewandler-flux-providers` BEFORE `codewandler-flux-sdk` (crates.io requires the optional dep
  published first).
- Tests: `providers_from_spec_builds_a_credential_free_provider` (`ollama/qwen3` → working provider,
  model "qwen3") + the lean-default manifest check. Gate green (workspace 2164; SDK all-features 45
  lib; clippy incl. all-features / fmt / codegate). CHANGELOG + WHATS-NEW + website mirror updated.
  **Not committed/released.**

## Notes
- Blocked by D-152. The realtime voice provider stays behind flux-providers' own `realtime`
  feature — do not flatten features across the boundary.

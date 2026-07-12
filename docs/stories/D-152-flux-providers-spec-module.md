---
id: D-152
title: flux_providers::spec — move model-spec → provider resolution out of the CLI
pillar: Agent
status: done
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 3 — de-duplicates the only copy of build_provider; CLI delegates byte-identical"
---

# flux_providers::spec — model-spec → provider resolution as a library

## Goal
`parse_model_spec` + `build_provider` move from `crates/flux-cli/src/main.rs` (~1000–1115) into a
new `flux_providers::spec` module so every embedder gets `spec::build("claude/sonnet")` —
including the `claude`/`codex` subscription token-source wiring — instead of re-implementing the
CLI's mapping.

## Acceptance
- [x] Failing-first (library): `spec::build("claude/sonnet")` resolves the subscription token
      source; unknown provider error lists the known providers; bare aliases resolve per
      provider `resolve_model` maps.
- [x] CLI delegates; behavior byte-identical — snapshot the CLI's provider-error strings before
      and after; `cargo test -p flux-cli` green.
- [x] Layering: flux-providers (L1) → flux-credentials (L1) dep is codegate-legal (verify the
      gate).

## Progress
- **Done (unreleased).** New `flux_providers::spec` module (`crates/flux-providers/src/spec.rs`):
  `parse_model_spec`, `build` (former `build_provider`), `provider_prefix` (former
  `spec_provider_prefix`), `KNOWN_PROVIDERS`, and a private `ensure_aws_chain` (wraps the already
  in-crate `bedrock::materialize_chain_into_env`). AWS handling moved behind the existing bedrock
  module — flux-providers grew no new AWS config surface (per Notes).
- Errors: `parse_model_spec` uses format-string `flux_core::Error::Other` (verbatim strings); the
  `.context("… provider")` build cases became `Error::Other(format!("… provider: {e}"))`, byte-
  identical to anyhow's `{:#}` chain the CLI's top-level printer uses.
- CLI: deleted the five moved items; `build_provider` is a thin delegate to `flux_providers::spec::build`;
  `auth_row_for_spec` calls `flux_providers::spec::provider_prefix`; provider-fn imports removed. The
  CLI `parse_model_spec_covers_…` test repointed at the library fn (proves the CLI's view surfaces the
  exact strings); the `build_provider("aws/sonnet")` factory test still exercises the delegation.
- Layering: `flux-providers` (L1) → `flux-credentials` (L1) is same-layer, **no cycle**
  (`flux-credentials` deps only `flux-provider`); codegate `workspace_respects_layering` green.
  crates.io publish order already lists `codewandler-flux-credentials` (34) before
  `codewandler-flux-providers` (48) — no script change.
- Tests: 3 library tests in `spec.rs` (aliases/defaults/empty-model rejection, unknown-provider +
  bare-word errors, `provider_prefix`). Gate green (workspace 2163 / clippy / fmt / codegate).
  **Not committed/released.**

## Notes
- AWS/bedrock env-chain handling stays CLI-side or moves behind the existing bedrock module —
  decide at implementation; do not force flux-providers to grow an AWS config surface.

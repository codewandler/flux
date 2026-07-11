---
id: D-152
title: flux_providers::spec — move model-spec → provider resolution out of the CLI
pillar: Agent
status: backlog
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
- [ ] Failing-first (library): `spec::build("claude/sonnet")` resolves the subscription token
      source; unknown provider error lists the known providers; bare aliases resolve per
      provider `resolve_model` maps.
- [ ] CLI delegates; behavior byte-identical — snapshot the CLI's provider-error strings before
      and after; `cargo test -p flux-cli` green.
- [ ] Layering: flux-providers (L1) → flux-credentials (L1) dep is codegate-legal (verify the
      gate).

## Progress
- (pending)

## Notes
- AWS/bedrock env-chain handling stays CLI-side or moves behind the existing bedrock module —
  decide at implementation; do not force flux-providers to grow an AWS config surface.

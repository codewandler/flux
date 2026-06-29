---
id: C-02
title: Integration-stack hardening — embeddings backend, plugin install/call + CI, live smoke
pillar: Core
status: in-progress
theme: downstream-managed-agents
design: docs/designs/integration-plugins.md
---

# Integration-stack hardening

## Goal
Harden the shipped Slack-channel assistant integration stack (D-07/D-08/D-09/D-10) on three fronts: **wire the deferred
embeddings/semantic retrieval** behind the D-07 `Embedder` seam; make the in-repo plugin pack **reachable
and CI-tested** (`flux plugin call`/`install`, plus a `plugins/`-workspace CI step); and add a **live,
env-gated smoke** that proves a plugin works against a real vendor API. Additive and feature-gated — the
default build, keyword retrieval path, and main gate stay unchanged.

## Why
The stack is gate-green but unproven against real services and missing usability: retrieval is keyword-only
(the embeddings seam is unwired), the 8 plugins can only be invoked through an agent/app run and never build
in CI, and nothing exercises a plugin against a live API. These are the gaps between "implemented" and
"production-solid".

## Acceptance
- [ ] **`flux plugin call <name> <op> [json]`** invokes one declared op of an installed plugin directly
      (spawns the binary via `PluginHost`, drives it through `DatasourceHostCaps`), printing the result —
      the missing direct-invocation path (debugging + the smoke). Hermetic test against the `echo`/`caps`
      fixtures.
- [ ] **`flux plugin install [dir]`** registers every `flux-plugin-*` binary in `dir` (default
      `plugins/target/release`) as a descriptor. Hermetic test (temp dir → descriptors written).
- [ ] **plugins CI**: `.github/workflows/ci.yml` builds/tests/clippy/fmt the `plugins/` workspace
      (`working-directory: ./plugins`).
- [ ] **Embeddings (feature-gated `embeddings`):** an `OpenAiEmbedder` (`/v1/embeddings`, reqwest +
      `guard_url`, env key) + a `SemanticIndex` decorator over any `DatasourceBackend` doing hybrid
      keyword∪cosine rerank. Default build (feature off) and the keyword path are unchanged. Failing-first
      test: a stub `Embedder` makes `search` rerank by cosine/blend; no-embedder path identical to keyword.
- [ ] **Live smoke** `scripts/smoke-plugins.sh` (skip-not-fail, env-gated): `flux plugin call` against a
      real API when a key is present (e.g. `TAVILY_API_KEY`→websearch, `GITLAB_PERSONAL_TOKEN`→gitlab) + an
      embeddings round-trip with `FLUX_EMBEDDINGS_API_KEY`. Documented in the roadmap's standing gate.
- [ ] Full gate green each phase (`cargo build/test --workspace`, clippy `-D warnings`, fmt, flux-codegate)
      + `cargo build -p flux-capabilities --features embeddings`; plugins workspace green.

## Progress
- In progress. Plan: `~/.claude/plans/steady-sniffing-storm.md`. Phase order: plugin call/install + CI →
  embeddings (feature-gated) → live smoke.

## Notes
- Reuse: `flux_system::net::guard_url` + reqwest (`browser.rs`); `flux-plugin`
  `discover`/`add_descriptor`/`PluginHost`/`load_plugin_tools`; the L5 `DatasourceHostCaps` bridge;
  `crates/flux-plugin/tests/host.rs` spawn pattern; the `scripts/smoke-live.sh` harness.
- Vectors are in-memory (rebuilt on ingest) in v1 — durable embedding storage is a follow-up. Builds on
  [[Slack-channel assistant-integration-stack]] (D-07/D-08/D-09/D-10).

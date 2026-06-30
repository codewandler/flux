---
id: C-02
title: Integration-stack hardening — embeddings backend, plugin install/call + CI, live smoke
pillar: Core
status: done
theme: downstream-managed-services
design: docs/designs/integration-plugins.md
note: "`flux plugin call`/`install` + a `plugins/` CI job (`a8092dc`); feature-gated embeddings/semantic backend — `OpenAiEmbedder` + a `SemanticIndex` hybrid-rerank decorator, default build unchanged (`f912c24`); a live env-gated `scripts/smoke-plugins.sh` (`5fda8be`)"
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
- [x] **`flux plugin call <name> <op> [json]`** invokes one declared op directly (spawns the binary via
      `PluginHost`, drives it through `DatasourceHostCaps`), printing the result. Commit `a8092dc`; smoked
      `flux plugin call echo upper` → `{"text":"HELLO PLUGIN CALL"}`.
- [x] **`flux plugin install [dir]`** registers every `flux-plugin-*` binary in `dir` (default
      `plugins/target/release`). Hermetic unit test for the scan (`plugin_binaries_in`). Commit `a8092dc`.
- [x] **plugins CI**: a new `plugins` job in `.github/workflows/ci.yml` builds/tests/clippy/fmt the
      nested `plugins/` workspace. Commit `a8092dc`.
- [x] **Embeddings (feature-gated `embeddings`):** `OpenAiEmbedder` (`/v1/embeddings` via runtime-free
      `ureq` + `guard_url`, env config) + a `SemanticIndex` decorator over any `DatasourceBackend` doing
      hybrid keyword∪cosine rerank. Default build (feature off) + keyword path unchanged; hermetic
      stub-embedder rerank test. Commit `f912c24`.
- [x] **Live smoke** `scripts/smoke-plugins.sh` (skip-not-fail, env-gated) via `flux plugin call`;
      embeddings validated by the feature build. Documented in the roadmap's standing gate. Commit `5fda8be`.
- [x] Full gate green (`cargo build/test --workspace`, clippy `-D warnings`, fmt, flux-codegate) +
      `cargo build -p flux-capabilities --features embeddings`; plugins workspace green.

## Progress
- **Done** (commits `a8092dc` → `f912c24` → `5fda8be`). Plugins are directly invocable (`flux plugin
  call`) + one-shot installable + CI-tested; the embeddings/semantic backend is wired behind a feature gate
  (default build unchanged); a live env-gated smoke covers the pack. Plan:
  `~/.claude/plans/steady-sniffing-storm.md`.

## Notes
- Reuse: `flux_system::net::guard_url` + reqwest (`browser.rs`); `flux-plugin`
  `discover`/`add_descriptor`/`PluginHost`/`load_plugin_tools`; the L5 `DatasourceHostCaps` bridge;
  `crates/flux-plugin/tests/host.rs` spawn pattern; the `scripts/smoke-live.sh` harness.
- Vectors are in-memory (rebuilt on ingest) in v1 — durable embedding storage is a follow-up. Builds on
  [[slack-assistant-integration-stack]] (D-07/D-08/D-09/D-10).

---
id: C-88
title: Decompose god-functions and de-duplicate helpers trapped in binary crates
pillar: Core
status: done
priority: 16
design: docs/designs/harness-hardening.md
note: "Quality (Low) — build_agent_with (417 LOC) + exec_body (~1520 LOC); humanizers/dispatch/temp-dir duplicated; stringify error idiom"
---

# Decompose god-functions and de-duplicate helpers trapped in binary crates

## Goal
Reduce the two dominant maintenance risks the review flagged and the recurring duplication behind them.
`build_agent_with` (417 lines, the assembly seam every agent subcommand depends on) and `exec_body`
(~1520-line async match, ~35 node arms with copy-pasted child-body/return-propagation logic) can only be
exercised end-to-end. Separately, several pure helpers live in *binary* crates so surfaces re-implement
and drift them — the TUI's `fmt_count` even reintroduces the exact boundary bug the CLI's `fmt_tokens`
test guards against.

## Acceptance
- [x] `build_agent_with` decomposed into named steps (`resolve_cli_provider`→`ResolvedProvider`,
      `register_tool_packs`, `resolve_permissions`→`ResolvedPermissions`, `assemble_engine`→`EngineParts`)
      returning small structs.
- [ ] `exec_body` split into per-arm handlers + a shared `run_child_body` helper. **DEFERRED** — lives in
      `flux-lang/src/runtime.rs`, owned by the concurrent LANG agent; out of scope for this pass.
- [x] Token/duration humanizers hoisted to L0 (`flux-core::humanize`) and shared by CLI + TUI (fixing the
      TUI boundary bug); provider mock/lazy/eager dispatch (`is_mock_spec`), the doc-walk loop
      (`walk_docs`), and `flux-eval` temp-dir creation (`util::unique_temp_dir`) each extracted to one
      implementation.
- [ ] The `.map_err(|e| anyhow!("{e}"))` idiom (~30 sites) replaced with `?`/`.context()`. **DEFERRED** —
      broad cross-crate sweep, decoupled from the god-function/dedup work; left for a follow-up.

## Progress
- 2026-07-15 — Finished the `build_agent_with` decomposition: the earlier pass added `EngineParts`
  (struct) + `assemble_engine` (fn) but never wired them in, leaving 2 dead-code warnings that reddened
  the workspace `-D warnings` gate. Wired `assemble_engine` into `build_agent_with` (replacing the
  inlined engine-assembly tail), so the crate is clippy-clean.
- De-duplicated the provider mock-spec check (`resolve_cli_provider` + `provider_for`) into one
  `is_mock_spec` predicate. The doc-walk loop was already unified into `walk_docs` (used by both
  `build_doc_index` and the `markdown` datasource arm) in the earlier pass.
- De-duplicated `flux-eval` temp-dir creation into `util::unique_temp_dir` (one atomic-counter impl);
  rewired `runner.rs` (dropped its local copy + `COUNTER`), `ops.rs`, `aggregate.rs`, and the `git.rs`
  test (dropped its private `N` counter). Added a unit test locking in per-call uniqueness (the bug the
  process-id-only sites carried).
- Also added `allowed_secrets: None` to the CLI's `WebOptions` construction — the concurrent WEB agent
  (C-76) added that field to `flux_web::WebOptions`; the env-var fallback (`FLUX_WEB_SECRET_ALLOW`) inside
  `flux-web` keeps C-76 functional, and no `[web] allowed_secrets` config key exists to wire.
- Verified: `cargo clippy -p flux-cli -p flux-tui -p flux-eval -p codewandler-flux-core --all-targets
  -- -D warnings` exits 0; flux-cli (161 bin tests), flux-eval, flux-tui, flux-core tests all green.
- DEFERRED (per orchestration): `exec_body` decomposition (flux-lang) and the ~30-site `.map_err`
  stringify sweep. No public-API changes in this pass.

## Notes
- `crates/flux-cli/src/execution.rs:766` (`build_agent_with`), `:790/:891` (dispatch), `:59/:114` (doc-walk);
  `crates/flux-lang/src/runtime.rs:1618` (`exec_body`); `crates/flux-tui/src/lib.rs:1447` vs
  `crates/flux-cli/src/style.rs:100` (humanizers); `crates/flux-eval/src/runner.rs:32` (temp-dir).
- Design: [harness-hardening](../designs/harness-hardening.md) (§Finding inventory, code-quality note).

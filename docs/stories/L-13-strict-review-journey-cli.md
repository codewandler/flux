---
id: L-13
title: Strict review — app journey + flux review CLI & CI surfaces (Phase 4)
pillar: Agent
status: done
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: flux-app review_code journey + optional flux review command + CI output modes
---

# Strict review — app journey + flux review CLI & CI surfaces (Phase 4)

## Goal

Make the strict-review flow a product surface: a `flux-app` `review_code` journey (owns trigger +
input mapping) that runs `strict_review`, an optional `flux review --files …` convenience command,
and CI-friendly output modes (markdown, JSON, nonzero exit on high severity). The journey owns app
routing; the flow owns execution semantics — keeping app plumbing separate from review correctness.
Serves the Agent pillar: an app-level entrypoint that wakes a governed protocol on command/event.

Full design: [docs/designs/strict-review-flows.md](../designs/strict-review-flows.md) — Phase 4 &
"Journey integration".

## Acceptance

- [x] **Failing-first test:** the journey path and the direct flow path produce the **same**
  `ReviewReport` for the same inputs — added red, then green.
- [x] A `flux-app` `review_code(input)` journey runs `strict_review(files, diff, reviewers?)`.
- [x] Optional `flux review --files …` invokes the same flow through the safety envelope.
- [x] CI output modes: markdown, JSON, and a nonzero exit when a finding meets a configurable
  severity threshold.
- [x] Write/network/report-publishing effects stay outside the strict-review core and require
  explicit approval (per the design's security considerations).
- [x] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [x] CHANGELOG entry.

## Notes
- Depends on [L-10](L-10-strict-review-example-flow.md) (flow) and
  [L-12](L-12-strict-review-typed-artifacts.md) (typed `ReviewReport`); best after
  [L-11](L-11-strict-review-scoped-capabilities.md) so the served protocol is enforced, not advisory.
- Open question to settle: is strict review a built-in sample, a project template, or a first-class
  CLI command (this story picks CLI + journey).

## Progress

**Landed.** The same-report invariant is structural, not conventional: `crates/flux-app/src/review.rs`
embeds the ONE checked-in `examples/strict_review.flux` via `include_str!`
(`STRICT_REVIEW_FLOW_SRC`) and `strict_review_op()` parses that exact text once into a `DraftAst`,
wrapping it unmodified as a `strict_review` `CompositeOpDecl`. The `review_code` journey
(`review_code_journey()`) is pure plumbing — `return strict_review(files: $files)` — with no review
logic of its own. Both the journey and `flux review` bottom out in the identical `DraftAst` executing
through the identical `Executor::dispatch` envelope.

- **Journey:** `flux_app::App` grew `with_sub_agents` (mirroring `FlowClient::with_sub_agents`):
  registers `TaskTool` and installs a `SubAgents`-built spawner on every journey run's executor, so a
  journey (or a composite op it calls) can delegate via `task`. `strict_review_program()` builds a
  `Program` with the `strict_review` op, the `review_code` journey, and an `on "review"` trigger.
  `flux app run strict-review` (a built-in program name, no file) loads it — real and runnable, zero
  duplicated `.flux` text.
- **CLI:** `flux review --files <path>… [--format md|json] [--fail-on <severity>]` (new `Commands::Review`
  in `crates/flux-cli/src/main.rs`) wires roles + `SubAgents` exactly like `build_agent`
  (`build_review_sub_agents` helper, shared with the `flux app run strict-review` branch of `run_app`
  so the two surfaces can't drift), then runs `STRICT_REVIEW_FLOW_SRC` through
  `flux_sdk::FlowClient::run_flow` (deterministic `parse → analyze → execute_with`, no model round-trip
  for the flow itself). Prints markdown (default, `render_review_markdown`) or the raw `ReviewReport`
  JSON. `--fail-on <severity>` → `should_fail(report, threshold)` is a pure, unit-tested function
  (`Info|Low|Medium|High|Critical`, `>=` comparison; an unrecognized severity string fails safe as
  `Critical` so it can never silently slip under a threshold); `run_review` calls
  `std::process::exit(1)` only at the top level.
- **Self-contained:** `load_roles` already falls back to the built-in `DEFAULT_ROLES`/checked-in
  `.flux/agents/review-*.md` pattern; a project's own role files still override. The flow text ships in
  the binary via `include_str!`, so `flux review` works in any repo.
- **Security:** unchanged from L-11/L-12 — reviewer roles keep `tools: []`; `strict_review`'s core stays
  read-only (git_status/git_diff/read_many + bounded `task` fan-out + `review.aggregate`); the CLI only
  ever prints to stdout, no write/network/publishing effect was added.
- **Tests:** `crates/flux-app/tests/strict_review_journey.rs` (the headline acceptance test — added RED
  when `flux_app::App` had no sub-agent wiring and `flux_app::review` didn't exist, GREEN after; asserts
  `App::deliver("review", …)`'s journey result equals `FlowClient::run_flow`'s direct result, byte for
  byte, against the same mock reviewer fixture); `crates/flux-app/src/review.rs`'s unit tests (the
  composite op wraps the checked-in file verbatim); 10 new `flux-cli` unit tests for `should_fail`
  (off-by-default, threshold boundary, no-findings, fail-safe-on-unrecognized-severity) and
  `render_review_markdown`; existing `crates/flux-sdk/tests/strict_review.rs` untouched and green.
- **Gate:** `cargo build --workspace`, `cargo test --workspace` (912 passed, 0 failed, run with
  `--test-threads=1` — the workspace has a pre-existing, unrelated `flux-providers` bedrock test flake
  under this sandbox's ambient `AWS_*` env vars racing across parallel test threads; confirmed
  reproducible on unmodified `main` too and unrelated to this story), `clippy --all-targets -D warnings`
  (clean), `fmt --check` (clean), `flux-codegate` (clean — `flux-app` gained `flux-orchestrate` [L3], no
  inner→outer edge; `flux-sdk` added to `flux-app`'s `[dev-dependencies]` only, which the layering
  check doesn't scan). Live-model smoke-tested `flux review` end-to-end via OpenRouter Sonnet against a
  real file in this repo (confirmed exit-code flips at `--fail-on medium`, both `--format` modes
  render correctly).
- Design doc's Phase 4 section and "Journey integration" updated to match what shipped.

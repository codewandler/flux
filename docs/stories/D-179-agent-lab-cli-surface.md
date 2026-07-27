---
id: D-179
title: Agent Lab CLI — flux record / flux test / resurrect-on-open
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 5 — CLI as the reference app on the SDK; depends on D-174/D-178"
---

# Agent Lab CLI — flux record / flux test / resurrect-on-open

## Goal
Expose the Deterministic Agent Lab through the CLI (the reference app built on the SDK): record
scenarios, run them offline as a test gate, and transparently resume interrupted sessions.

## Acceptance
- [x] `flux record <name> "<prompt>"` writes a `tests/scenarios/<name>/` fixture (a `Storage::dir`
      directory) via `Scenario::record`. `--dir` relocates the scenarios root; the name must be a
      single path segment (rejected otherwise, so a fixture can't escape the root).
- [x] `flux test [<name>]` runs `Scenario::replay` offline ($0, no key, no network — a **deny-all
      approver plus a provider that refuses to answer**, so a replay that tried to go live would
      fail loudly rather than quietly cost money); exit 1 on regression; `--json` for CI. A
      regression prints BOTH halves — the world (`faithful`) and the plan snapshot's unified diff —
      and `FLUX_GOLDEN=update` re-baselines exactly as it does under `cargo test`.
      **No vacuous pass**: `flux test` with no fixtures at all is an error, not a green gate.
- [x] Resurrect-on-open: a CLI turn on a session killed mid-turn finishes it from the crash point
      first (`resurrect_on_open`, reported on stderr with the fast-forwarded/served/live counts, and
      loud about a latched divergence); `FLUX_AUTO_RESURRECT=0` disables it. `flux sessions` FLAGS an
      interrupted session (`⚠ interrupted` + a footer) but deliberately does **not** resurrect —
      running a live tail through the approval envelope must not be a side effect of asking what
      sessions exist.
- [x] Fixtures remain openable by the existing `flux replay`/`fork`/`diff` (same `Storage::dir`
      layout) — new global `--store <DIR>` points every session tool at one, proven end-to-end by
      `record_writes_a_fixture_that_flux_test_and_the_time_machine_both_read`.
- [x] Follows CLI conventions: explicit subcommands, no implicit default-run.

## Progress
- **Done** (2026-07-28). New `crates/flux-cli/src/lab_cmd.rs` (`run_record`/`run_test`), both thin
  drivers over `flux_sdk::test::Scenario` rather than a parallel implementation — `flux-cli` now
  takes `flux-sdk` with the `test-kit` feature. New global `--store <DIR>` on `Cli`, exported
  pre-runtime as `FLUX_STORE_DIR` (same channel as the other env-exported flags) and read by a new
  `execution::flux_store_dir()` that `open_event_store`/`open_flow_store` both route through — one
  resolution point, so every existing session command inherits it for free.
- `flux-sdk` gained the non-panicking `Outcome::plan_snapshot()` (the `assert_plan_snapshot` body,
  which now delegates to it) plus `plan_source()`/`calls()`/`text()` accessors — a non-`cargo test`
  gate needs to *render* a regression, not panic on it.
- 3 end-to-end tests in `crates/flux-cli/tests/agent_lab.rs` driving the real binary against the
  offline `mock` provider under an isolated HOME + CWD: the record→test→`replay --store`→`sessions
  --store` round trip, the regression gate (corrupt the golden → exit 1 naming the mismatch →
  `FLUX_GOLDEN=update` → green again), and the no-vacuous-pass guard.
- The website CLI reference is contract-tested (`cli_reference_covers_every_public_subcommand`), and
  it caught the missing entries immediately — `website/docs/agent/cli.md` gained `flux record` /
  `flux test` rows and a `--store` section.
- Gate green in both workspaces (build/test/clippy `-D warnings`/fmt) plus `flux-codegate`.

## Notes
- Files: `crates/flux-cli/src/{args,dispatch,session}.rs`.
- Depends on D-174 (Test Kit) and D-178 (Resurrect). Website docs/ops-catalog mirrors as needed
  (website is contract-tested).

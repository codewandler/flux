---
id: D-180
title: Agent Lab — cookbook recipe and dogfood golden suite
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 6 — adoption proof; the flux coding agent gets its own golden tests"
---

# Agent Lab — cookbook recipe and dogfood golden suite

## Goal
Prove the Deterministic Agent Lab on flux itself and document it for embedders — the strongest
adoption signal is the flux coding agent shipping a committed, offline, $0 golden test suite.

## Acceptance
- [x] `crates/flux-sdk/examples/agent_lab.rs` (`required-features = ["test-kit"]`) walks all four
      steps end to end and RUNS: record → offline replay + assertions → a `what_if` substitution →
      `interrupted()`. Verified by actually running it, not by inspection.
- [x] `crates/flux-sdk/tests/scenarios/coding-agent-note/` is a committed golden of the flux coding
      agent performing a real task (writing a file into its workspace), replayed hermetically by
      `crates/flux-sdk/tests/agent_golden.rs` under a deny-all approver + never-called provider —
      faithful, plan-snapshot-identical, `assert_calls(&["append"])`, `assert_never_calls` for both
      shell ops, and `assert_cost_under(0.000_001)`.
- [x] The A/B demonstration, as two tests: an edited system prompt makes `check()` report
      `model_live > 0` and a non-clean report (a REASONING change — the request no longer matches
      the golden cassette, so it falls through live and is counted, never silently served), while a
      substituted tool output produces exactly one `DiffRow::Output` with **zero** `DiffRow::Plan`
      rows (a WORLD change — same reasoning, different answer) and stays hermetic.
- [x] `website/docs/sdk/agent-lab.md` (drafted mid-epic, truth-passed here) + a WHATS-NEW entry with
      the mirror regenerated.

## Progress
- **Done** (2026-07-28).
- **Fixture provenance, stated plainly rather than dressed up**: the golden was recorded with
  `flux record --yes -m mock coding-agent-note "write a quick note"`. The agent, adaptive loop,
  plan, op catalog, and the whole safety envelope are the real ones; only the model's answers are
  canned. That is what makes it committable AND re-recordable by anyone — a live-model recording
  would replay fine (that is the cassette's job) but nobody without a key could re-baseline it.
  Verified before committing: recorded twice from scratch, `plan.flux.snap` and `model.jsonl`
  byte-identical; zero absolute paths in `model.jsonl` or `events.db` (`strings | grep /home` → 0).
  156 KB total. A `the_committed_fixture_is_portable_and_redacted` test pins the portability and
  redaction properties rather than leaving them as an assumption.
- `.gitignore` blanket-ignores `*.db`, which would have silently swallowed the fixture — added a
  negation for `**/tests/scenarios/*/*.db` (valid because only files, never a parent directory, are
  ignored above) and verified with `git check-ignore`.
- Small SDK additions this needed: `Scenario::work_dir()` (to open what a replay just recorded) and
  D-179's `Outcome::plan_snapshot()`/`plan_source()`/`calls()`/`text()`.
- Docs truth-pass resolved all three `<!-- D-180 truth-pass: verify -->` markers against the landed
  code: `WhatIf::policy` documented as shipped (D-177) with its real semantics; the `WhatIfSpec`
  example corrected — there is no `WhatIfSpec::model(..)` constructor, it is `session.what_if()
  .model(..).spec()` (or field-wise over `Default`); and the CLI examples corrected to the real flag
  spellings (`--store` is global and precedes the positional args, `--dir` relocates the scenarios
  root, `FLUX_GOLDEN=update flux test` re-baselines, `flux sessions` flags but does not resurrect).
- Gate green in both workspaces (build/test/clippy `-D warnings`/fmt), plus `flux-codegate` and
  `codewandler-flux-sdk` on the default and `test-kit` feature configurations.

## Notes
- Depends on D-174/D-176/D-178. WHATS-NEW edits require the website mirror regen (`website_in_sync`
  UPDATE=1) or the workspace test fails.
- Keep any golden fixture free of downstream-consumer internals (repo-artifact hygiene rule).

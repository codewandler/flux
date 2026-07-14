---
id: D-180
title: Agent Lab — cookbook recipe and dogfood golden suite
pillar: Agent
status: backlog
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 6 — adoption proof; the flux coding agent gets its own golden tests"
---

# Agent Lab — cookbook recipe and dogfood golden suite

## Goal
Prove the Deterministic Agent Lab on flux itself and document it for embedders — the strongest
adoption signal is the flux coding agent shipping a committed, offline, $0 golden test suite.

## Acceptance
- [ ] A `crates/flux-sdk/src/recipes/` (or examples) entry demonstrating record → commit → `cargo
      test` replay + assertions, plus a `check()` A/B after a prompt edit.
- [ ] A committed golden fixture of the flux coding agent performing a real task; `cargo test
      --features test-kit` re-runs it hermetically (deny-all approver + never-called provider), green,
      with `assert_cost_under` ≈ $0.
- [ ] A demonstration that editing the system prompt makes `check()` surface a `DiffRow::Plan`
      (reasoning regression) while a changed tool output surfaces a `DiffRow::Output`.
- [ ] Website/SDK docs page for the Agent Lab (Test/Tune/Resurrect), kept in sync with WHATS-NEW.

## Progress
- (not started — epic deferred; docs-only for now)

## Notes
- Depends on D-174/D-176/D-178. WHATS-NEW edits require the website mirror regen (`website_in_sync`
  UPDATE=1) or the workspace test fails.
- Keep any golden fixture free of downstream-consumer internals (repo-artifact hygiene rule).

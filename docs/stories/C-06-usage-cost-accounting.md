---
id: C-06
title: Usage & cost accounting — attribution, aggregation, reporting
pillar: Core
status: backlog
priority:
design: docs/designs/subscription-providers-and-cost.md
theme: subscription-providers-cost
---

# Usage & cost accounting — attribution, aggregation, reporting

## Goal
Turn captured tokens (C-05) into the full user-facing surface: usage attributed to the model that produced
it, sub-agent spend rolled into the parent, a durable cost projection over the event log, a `flux usage`
command, a server endpoint, and cache-aware CLI/TUI/server output that shows cost.

## Acceptance
- [ ] **model attribution.** The resolved model is stamped onto the usage record so a turn that switches
      model mid-flight (`/model`) attributes tokens/cost to the right model. Failing-first test
      `usage_attributed_per_model_after_switch` over a fixture event log with a mid-turn `ModelChanged`.
- [ ] **sub-agent rollup.** A parent turn's usage includes its sub-agents' token spend. Failing-first test
      `parent_turn_includes_subagent_usage` (a spawned sub-agent's tokens appear in the parent's total).
- [ ] **cost_summary projection.** A new projection over `flux-events` totals tokens + cost per session and
      in aggregate, grouped by model/provider, cache-aware. Failing-first test
      `cost_summary_rolls_up_session` over a fixture log (multiple turns, multiple models, cache tiers).
- [ ] **`flux usage` command.** A subcommand reports per-model tokens + cost for the current/last session
      and an all-sessions total. Failing-first test `flux_usage_reports_per_model_cost`.
- [ ] **server endpoint.** `GET /sessions/:id/usage` (+ an aggregate route) returns cache tiers + cost; the
      existing `POST /sessions/:id/messages` usage block stops dropping cache fields. Test
      `usage_endpoint_returns_cache_tiers_and_cost`.
- [ ] **cache-aware surfacing.** The CLI `usage_annotation` and TUI header include cache tiers + cost (TUI
      total no longer ignores cache). Test `usage_annotation_includes_cache_and_cost`.
- [ ] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- (not started)

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
  Depends on **C-05** (cost model + normalized codecs).
- Touch points: `crates/flux-events/src/{kind,store,projection}.rs` (`TurnEnded.usage` + a per-call usage
  carrier; `cost_summary` beside `turns()`), `crates/flux-flow/src/loop_host.rs` (`usage` accumulate —
  sub-agent rollup), `crates/flux-cli/src/main.rs` (`flux usage`, `usage_annotation`, sub-agent factory),
  `crates/flux-tui/src/lib.rs` (header totals), `crates/flux-server/src/lib.rs` (usage JSON + endpoint).
- Reuse: `flux-eval`'s `load_usage` (true field-wise per-turn summation — the reference for cross-turn
  aggregation, vs the engine's replace-style `accumulate`); the `turns()` projection (already carries both
  `model` and `usage` — the join point for attribution).
- The `model`-on-`TurnStarted` / `usage`-on-`TurnEnded` split is the attribution hazard — stamp the resolved
  model onto the usage record so a mid-turn switch is unambiguous.

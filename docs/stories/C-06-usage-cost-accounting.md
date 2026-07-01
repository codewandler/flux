---
id: C-06
title: Usage & cost accounting — attribution, aggregation, reporting
pillar: Core
status: done
epic: subscription-providers-and-cost
theme: subscription-providers-cost
design: docs/designs/subscription-providers-and-cost.md
note: model attribution + sub-agent rollup + a `cost_summary` projection + `flux usage` + a server endpoint + cache-aware surfacing (needs C-05)
---

# Usage & cost accounting — attribution, aggregation, reporting

## Goal
Turn captured tokens (C-05) into the full user-facing surface: usage attributed to the model that produced
it, sub-agent spend rolled into the parent, a durable cost projection over the event log, a `flux usage`
command, a server endpoint, and cache-aware CLI/TUI/server output that shows cost.

## Acceptance
- [x] **model attribution.** The resolved model is stamped onto the usage record so a turn that switches
      model mid-flight (`/model`) attributes tokens/cost to the right model. Failing-first test
      `usage_attributed_per_model_after_switch` over a fixture event log with a mid-turn `ModelChanged`.
- [x] **sub-agent rollup.** A parent turn's usage includes its sub-agents' token spend. Failing-first test
      `parent_turn_includes_subagent_usage` (a spawned sub-agent's tokens appear in the parent's total).
- [x] **cost_summary projection.** A new projection over `flux-events` totals tokens + cost per session and
      in aggregate, grouped by model/provider, cache-aware. Failing-first test
      `cost_summary_rolls_up_session` over a fixture log (multiple turns, multiple models, cache tiers).
- [x] **`flux usage` command.** A subcommand reports per-model tokens + cost for the current/last session
      and an all-sessions total. Failing-first test `flux_usage_reports_per_model_cost`.
- [x] **server endpoint.** `GET /sessions/:id/usage` (+ an aggregate route) returns cache tiers + cost; the
      existing `POST /sessions/:id/messages` usage block stops dropping cache fields. Test
      `usage_endpoint_returns_cache_tiers_and_cost`.
- [x] **cache-aware surfacing.** The CLI `usage_annotation` and TUI header include cache tiers + cost (TUI
      total no longer ignores cache). Test `usage_annotation_includes_cache_and_cost`.
- [x] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- **Done.** All six acceptance items landed; full gate green (925 tests, `--test-threads=1`; clippy
  `-D warnings` clean; `fmt --check` clean; `flux-codegate` layering lint green).
- **Attribution core** — new `EventKind::CallUsage { model, usage }` (`flux-events/src/kind.rs`) + store
  helper `record_call_usage` (turn-scoped, no-op on `turn_id < 0`, mirrors `record_plan_attempt`).
  `EngineLoopHost` (`flux-flow/src/loop_host.rs`) now records every planner call individually
  (`calls: Mutex<Vec<(String, Usage)>>`, reset per turn in `set_turn`, read via `turn_calls()`) alongside
  the existing turn-total `usage` accumulator — so a mid-turn model swap (`set_model` + `ModelChanged`)
  attributes each call's tokens to the model that was actually active for it. `FlowEngine::run_turn_cancellable`
  appends one `CallUsage` per call (skipping zero-usage calls) right before `end_turn`, on every
  termination path (cancelled or completed).
- **Sub-agent rollup** — `flux_runtime::Spawner::spawn`/`spawn_scoped` now return `SpawnOutcome { text,
  model, usage }` instead of a bare `String` (contained blast radius: `flux-orchestrate`'s `LocalSpawner`
  is the only real implementer). `TaskTool::execute` records a `subagent.usage` observation `{role, model,
  usage}` onto the SHARED evidence log (`ctx.evidence`) when the child billed anything — the same
  snapshot-then-diff-scoped side-channel `turn.iteration` already uses for turn-local facts that can't
  ride `ToolResult`. `FlowEngine` snapshots `by_kind("subagent.usage").count()` at turn start
  (`subagent_base`), diffs it at turn end (`subagent_calls_since`), folds the child usage into the
  parent's `turn_usage()` total AND emits a `CallUsage` attributed to the sub-agent's own model — so
  `cost_summary` prices the child's spend under the model that generated it while the parent's turn total
  still reflects the real spend. `plan_and_dispatch`/`plan_and_dispatch_waves` and `flux-eval`'s
  `change_implement` op (direct `Spawner::spawn` callers) updated to unwrap `.text`; three test-mock
  `Spawner` impls updated to return `SpawnOutcome`.
- **cost_summary projection** — `flux-events/src/projection.rs`: `ModelCost { model, usage, calls, cost }`
  + `cost_summary(events, pricing) -> Vec<ModelCost>`, folding `CallUsage` events by model (field-wise sum,
  ALL tiers including `reasoning_tokens` — unlike `Usage::accumulate`, this is a cost rollup, not a
  context-window occupancy figure). A stream with NO `CallUsage` events (an old log, pre-C-06) falls back
  to summing `TurnEnded.usage` per turn, attributed to that turn's `TurnStarted.model` — coarser but never
  zero. `EventStore::cost_summary`/`cost_summary_all` (+ new `all_streams()` enumeration primitive) wrap it
  per-session and across every session.
- **`flux usage`** — new `Commands::Usage` (`flux-cli/src/main.rs`): `run_usage` (opens the real store +
  `flux_credentials::load_pricing_table()`) delegates to store-parameterized `run_usage_with` (testable
  without touching `$HOME`), printing the latest session's per-model rows then an all-sessions total.
- **Server endpoint** — `GET /sessions/:id/usage` + `GET /usage` (`flux-server/src/lib.rs`), both backed by
  `cost_summary`/`cost_summary_all`; `usage_json`/`model_cost_json` helpers serialize every `Usage` tier.
  `POST /sessions/:id/messages`'s usage block now uses `usage_json` too, so it no longer drops
  cache_creation/cache_read/reasoning.
- **Cache-aware surfacing** — CLI `usage_annotation` now also shows `cache_creation_input_tokens` ("cache
  write") and `reasoning_tokens` (previously silently dropped); cost stays a separate suffix
  (`cost_annotation`/`cost_inline`, already existed). TUI `ChatState` gained `tokens_cache_read`/
  `tokens_cache_write`/`cost_usd`/`cost_model` fields, a `with_cost` builder, and a `record_usage` method
  the `UiEvent::Usage` handler now calls instead of only summing input/output; `header_line` shows the
  combined cache figure + running `$` cost. `flux_tui::run` gained an `Option<model_spec>` parameter
  (CLI's `run_tui` now passes the resolved spec through); pricing loaded via
  `flux_credentials::load_pricing_table()`.
- New crate deps added (all pass the `flux-codegate` layering lint): `flux-orchestrate` → `flux-evidence`;
  `flux-server` → `flux-credentials`, `flux-events` (+ dev-deps `flux-provider`/`flux-system`/
  `flux-tools`/`async-trait` for its usage-endpoint router-fixture test); `flux-tui` → `flux-credentials`.
- Deviation from the plan: sub-agent usage rides the shared **evidence log** (`ctx.evidence`), not a
  `Spawner` return value threaded through `ToolContext`/`ToolResult` — `ToolResult` is a plain string and
  changing it would have been a much larger blast radius. The evidence-log side-channel is an existing,
  precedented mechanism (`turn.iteration`, `skill.activated`) for exactly this "turn-scoped fact that
  isn't part of a tool's own return value" shape.

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

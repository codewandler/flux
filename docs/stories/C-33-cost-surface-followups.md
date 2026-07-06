---
id: C-33
title: "Cost-surface follow-ups — app-run/journey turns, TUI unpriced parity, GoalSink live spec"
pillar: Core
status: done
epic: multipass-agent-loop
note: "C-30 wired every CliSink surface; still cost-less: `flux app run` journey/agent-target turns (no CliSink at all) and the TUI's unpriced-marker parity (its cumulative header would silently lie once any turn is unpriced — needs a `$?` state, flux-tui/src/lib.rs record_usage)"
---

# Cost-surface follow-ups

## Goal
Finish the cost-display coverage C-30 started: (a) `flux app run` / journey / agent-target turns
render no per-turn cost anywhere (the path has no CliSink; costs land only in `flux usage`); (b)
the TUI prices turns but silently skips table misses — once any turn is unpriced its cumulative
header total is a lie; give it the `$?` state (mirror flux-cli's `cost_suffix` rules,
`flux-tui/src/lib.rs` `record_usage`); (c) `/goal`'s GoalSink captures the spec once per turn —
fine today, but re-derive per iteration if `/model` becomes reachable mid-goal.

## Acceptance
- [x] App-run/journey turn completions carry the same cost annotation contract (spec TBD with the
      app-run surface owner — the sink seam differs).
- [x] TUI: a pricing-table miss on any turn switches the header cost segment to a `$?` state
      instead of silently under-reporting; test beside the existing cost test.
- [x] Failing-first tests for each surface touched.

## Progress
- 2026-07-03 filed as the C-30 follow-up (scope decision: all CliSink surfaces in C-30; these
  non-CliSink surfaces deferred).
- 2026-07-04 note: once [C-34](C-34-openrouter-reported-cost.md) (provider-reported cost) lands,
  item (b)'s TUI-`$?` parity only concerns **non-reporting** providers — OpenRouter models will
  carry a reported cost and never hit the table-miss path. (a) and (c) unaffected.
- 2026-07-06 implemented all three parts (C-34 already released, confirming the note above):
  - **Predicate hoist**: `flux_core::is_metered_cloud_spec` (`crates/flux-core/src/pricing.rs`,
    exported from `lib.rs`) is the single "is this a metered cloud spec" rule — a table miss there
    hides real spend and must surface as `$?`, vs. local `ollama*`/mock, which never does.
    `flux-cli`'s `unpriced_marker_applies` (main.rs ~4061) now thin-delegates onto it.
  - **(b) TUI `$?` parity** (`crates/flux-tui/src/lib.rs`): added `cost_unpriced: bool` to
    `ChatState`; `record_usage` (~L392) sets it on a metered-cloud table miss (no row, no
    `reported_cost_usd`); `header_line` (~L780) renders `$X.XXXX+? (unpriced)` when part of the
    run priced and part didn't, or bare `$? (unpriced)` when none of it did. Failing-first test
    `unpriced_metered_cloud_turn_switches_header_to_question_mark` (also proves an `ollama/*`/`mock`
    spec never flips the marker) — reverted the fix, watched it fail on `assert!(state.cost_unpriced)`,
    restored, green.
  - **(a) app-run/journey/agent-target cost surfacing**: `flux_flow::AgentSink::turn_end` was a
    no-op sink everywhere in `flux-app`. `JourneyRun` (`crates/flux-app/src/app.rs`) gained
    `usage: Option<Usage>` + `model: String` (the canonical `provider/model` spec — the actual
    engine's for an `agent`-bound trigger's `run_agent`, the app's default for a plain
    `run_journey`/`resume_parked`, since a journey has no single driving engine). `RecordingSink`
    now implements `turn_end` (accumulates via a new `accumulate_usage` helper — field-by-field
    sum, unlike `Usage::accumulate`'s within-turn snapshot-replace semantics, because this folds
    *separate* turns). `run_journey`/`resume_parked` wrap the caller's `&mut dyn AgentSink` in a
    small `UsageCapture` forwarding wrapper (a type-erased trait object can't be read back, and the
    passed-in sink is shared/reused across a `deliver()` call's cascaded journeys, so a fresh
    wrapper per run is what attributes usage correctly). Failing-first tests in
    `agent_target_tests`: `agent_trigger_run_carries_usage_and_model_spec` (a new
    `ReplyWithUsageProvider` mock emits a real `Chunk::Usage` so `turn_end` fires `Some`) and
    `journey_run_carries_no_usage_but_reports_the_host_default_model` — both temporarily hardcoded
    to `usage: None, model: String::new()`, watched them fail, restored, green. Surfaced at the
    operator console: `flux-channels/src/host.rs`'s `stdin_loop` (the only place `flux app run`
    JourneyRun results reach a local terminal — Slack/webhook adapters only ever pluck `.result`
    into a channel-facing payload, confirmed unaffected by grepping `JourneyRun` usage) now prints a
    dim STDERR line per completed run (`cost_line`/`cost_suffix`/`dim`), loading
    `flux_credentials::load_pricing_table()` once in `serve()`. `cost_suffix` mirrors flux-cli's
    contract exactly but is a small deliberate duplicate, not a shared dependency — flux-channels
    (L6) can't depend on flux-cli (a sibling L6 crate). Added `flux-core`/`flux-credentials` as
    direct `flux-channels` deps (both L1, layering fine — confirmed via `cargo test -p
    flux-codegate`). Unit tests `priced_turn_renders_the_dollar_figure`,
    `metered_cloud_table_miss_renders_the_unpriced_marker`, `local_spec_renders_no_cost_segment`,
    `no_usage_renders_no_cost_segment` in `host.rs` — temporarily stubbed `cost_suffix` to always
    return `""`, watched the priced/unpriced cases fail, restored, green. `flux-cli`'s `--serve`
    call site (main.rs ~5122) needed no change — `serve()` loads pricing internally.
  - **(c) GoalSink spec re-derivation — verified, no code change**: `run_goal`
    (`crates/flux-cli/src/main.rs` ~3352, inside `for i in 0..MAX`) constructs a fresh `GoalSink`
    each iteration with `cost: Some((TurnCost::spec(agent), cost.pricing.clone()))`, and
    `TurnCost::spec` (~L2871) reads `agent.provider.name()`/`agent.model` live off the `&FlowEngine`
    — never a value snapshotted once outside the loop. `/model` (main.rs ~3015) reassigns
    `agent.provider`/`agent.model` in place but only runs from the SAME sequential REPL command
    dispatch loop that `/goal` blocks on via `run_interruptible(...).await`; the whole goal loop
    holds an immutable `&agent` for its entire duration, and Rust's borrow checker forbids the
    `/model` arm's `agent.provider = …`/`agent.model = …` (which need `&mut agent`) from running
    concurrently with that borrow. So the spec re-derivation the story asks for already existed,
    and `/model` genuinely isn't reachable mid-goal today — confirmed by reading both call sites,
    not by inspection alone.
  - **Gate** (package-scoped): `cargo build/test/clippy --all-targets -D warnings` for `-p
    flux-tui -p flux-app -p flux-channels -p flux-core -p flux-cli` all green (33/24/5(+8 in other
    test binaries)/24/85 tests respectively); `cargo test -p flux-codegate` green (confirms the new
    flux-channels deps don't violate layering); `cargo fmt --all` touched only
    `crates/flux-app/src/app.rs` and `crates/flux-tui/src/lib.rs` (my own files) plus a 2-line
    `Cargo.lock` update for the new flux-channels deps — every other dirty file in the tree
    (CHANGELOG.md, compile.rs, openrouter.rs, other stories/designs) was pre-existing, untouched,
    and left alone per the story's boundaries (no CHANGELOG entry added, `docs/stories/README.md`
    not regenerated — both out of scope per the task's explicit boundaries).

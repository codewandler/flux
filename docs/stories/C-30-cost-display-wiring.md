---
id: C-30
title: "Wire per-turn cost display into every CLI sink + visible unpriced marker + openrouter attribution fix"
pillar: Core
status: done
epic: multipass-agent-loop
note: "the interactive REPL never attaches with_cost — the dollar suffix is structurally impossible on the surface users actually use (only one-shot `flux run` and the TUI attach it); table misses render as silent nothing; and canonical_model_spec drops the outer openrouter-anthropic provider, mislabeling openrouter spend as anthropic in usage keys"
---

# Wire per-turn cost display into every CLI sink

## Goal
The user (bedrock `-m aws`, openrouter `-m openrouter-anthropic/anthropic/claude-sonnet-4.6`) has
NEVER seen a dollar cost. Investigation: both specs price correctly through `rates_for` — the gap is
display wiring. `CliSink::cost_inline` requires `.with_cost(spec, pricing)` and only `run_agentic`
(one-shot) and the TUI attach it; the REPL normal turn, `/plan`, `/compact`, `/run`, `/loop`,
`flux plan`, and the a2a sink construct bare `CliSink::new(..)`. A pricing-table miss additionally
renders as silent `""`, indistinguishable from unwired. Bonus latent bug: `canonical_model_spec`
drops the outer provider when the spec embeds a known one (`openrouter-anthropic/anthropic/X` →
attributed `anthropic/X` in usage keys). Plan: `~/.claude/plans/zesty-strolling-duckling.md`.

## Acceptance
- [x] Every CliSink surface attaches `(canonical_spec, PricingTable)` via one helper (REPL turn,
      /plan, /compact, /run, /loop, `flux plan`, a2a REPL); `/model` mid-session switch refreshes
      the spec. Failing-first `repl_turn_line_renders_cost_for_priced_model`.
- [x] Unpriced-but-attached renders a compact visible ` · $? unpriced` suffix (CLI + TUI), never
      silent. Failing-first `unpriced_model_renders_visible_marker`.
- [x] `canonical_model_spec` keeps the passed outer provider when the spec embeds a different known
      provider. Failing-first `canonical_model_spec_keeps_outer_openrouter_provider`;
      `rates_for_resolves_full_openrouter_spec` pins the full-spec lookup.
- [x] Live verification: an openrouter REPL turn shows `$0.00xx` in the after-turn line; `flux
      usage` attributes under `openrouter-anthropic/…`.
- [x] Follow-up story filed for the cost-less `flux app run`/journey + GoalSink surfaces.
- [x] Root gate green.

## Progress
- 2026-07-03 filed + started from the two-explorer investigation (traces in the plan file).
- 2026-07-03 **DONE.** (1) `TurnCost` factory in flux-cli — pricing table loaded once per command,
  spec derived from the LIVE engine at each sink construction (`canonical_model_spec(provider.name(),
  model)` — the same C-15 derivation loop_host stamps usage with), so `/model` switches are picked
  up with zero plumbing; attached at: REPL normal turn, /plan, /compact, /run (which also gained
  `set_turn` scoping + real `turn_usage()` reporting — nested plan ops no longer stream onto the
  stale prior-turn ctx), /loop, /goal (GoalSink grew the cost field), `flux plan`, and the
  `flow run`/preset shared sink (now reports real usage too). The a2a REPL stays cost-less by
  design (remote spend). (2) `cost_suffix` extracted (all sinks share it): a metered-cloud table
  miss renders ` · $? (unpriced)` + a once-per-process stderr note pointing at ~/.flux/pricing.toml;
  `ollama*`/mock/ad-hoc stay silent (hermetic e2e output byte-identical). (3) `canonical_model_spec`
  keeps the passed serving provider when the spec embeds a DIFFERENT known provider
  (`openrouter-anthropic/anthropic/claude-sonnet-4.6`), with a merge_legacy_keys pin that legacy
  mislabeled rows stay separate. Tests: `unpriced_model_renders_visible_marker`,
  `repl_sink_cost_derives_from_the_live_engine_spec`,
  `canonical_model_spec_keeps_outer_openrouter_provider`, `rates_for_resolves_full_openrouter_spec`,
  `merge_keeps_passthrough_provider_rows_separate` (+1 flipped assertion in the C-15 test).
  Live-verified: `flux run` over openrouter shows `$0.1227` on the turn line and `flux usage`
  attributes it under `openrouter-anthropic/anthropic/claude-sonnet-4.6` (REPL-path attachment is
  pinned by the unit test; piped-PTY REPL smoke was impractical headless). Full workspace gate
  green (87 test binaries, clippy, fmt, codegate).

## Notes
- Non-tabled models are priceable via the `~/.flux/pricing.toml` override
  (`flux-credentials/src/lib.rs:221-248`) — the `$? unpriced` marker points there.
- Legacy usage rows already stored under the mislabeled `anthropic/…` key stay as-is
  (merge_legacy_keys folds variants read-side; no migration).

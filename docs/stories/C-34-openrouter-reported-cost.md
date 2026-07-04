---
id: C-34
title: "OpenRouter reported cost: price turns from the provider's own `cost`, not the table"
pillar: Core
status: done
design: docs/designs/openrouter-reported-cost.md
note: "kills `$? (unpriced)` for OpenRouter models permanently — reported `cost` from the final usage frame beats the static table; table stays as fallback; opens with two ~$0.001 live probes (Messages endpoint unverified)"
---

# OpenRouter reported cost

## Goal

Models served via OpenRouter price every turn from the provider-reported `cost` in the usage frame —
`$? (unpriced)` disappears for them with zero pricing-table maintenance. The static table remains
the fallback for non-reporting providers. Design: [openrouter-reported-cost](../designs/openrouter-reported-cost.md).

## Acceptance

- [x] **Step 0 live probes recorded in Progress** (before any code): (1) streaming chat-completions
      call (deepseek) — does the final usage frame carry `cost` with no opt-in flag? (2)
      `/api/v1/messages` call (glm) — does usage carry `cost`; if absent, does extra_body
      `usage:{include:true}` produce it? Findings select the contingency (step 5).
- [x] `flux-core/stream.rs`: `Usage.reported_cost_usd: Option<f64>`
      (`serde(default, skip_serializing_if)`), `Eq` derive dropped, `accumulate` sums it.
      Failing-first: `usage_accumulate_sums_reported_cost`.
- [x] `flux-core/pricing.rs`: `CostSource { Reported, Estimated }`, `Money.source`, reported-first
      short-circuit in `PricingTable::cost`. Failing-first:
      `reported_cost_beats_table_and_prices_untabled_models` (untabled+reported → Some(Reported);
      tabled+reported → reported wins; tabled without → exact table figure, Estimated; reported 0.0
      → Some, no `$?`).
- [x] `flux-providers/openai.rs`: `ChatUsage.cost` + `cost_details.upstream_inference_cost`, summed
      into the yielded `Chunk::Usage`. Failing-first: `chat_usage_captures_openrouter_reported_cost`
      (incl. `"cost":null` tolerance).
- [x] `flux-providers/messages/{wire,mod}.rs`: `WireUsage.cost`, mapped in `From<WireUsage>`;
      `message_delta` carry-forward keeps cost sticky + `prior_usage` refreshed after each usage
      frame. Failing-first: `messages_stream_carries_reported_cost_through_final_delta`.
- [x] *(contingent on probes)* `flux-providers/openrouter.rs`: extra_body `usage:{include:true}` via
      `OpenRouterProfile::quirks_for`. Test if taken: `profile_requests_usage_accounting`. If the
      Messages endpoint never reports cost: file the generation-lookup follow-up story instead.
      **Dead per the step-0 probes** (both endpoints report cost unconditionally) —
      `openrouter.rs` is untouched, no request-body change anywhere.
- [x] `flux-events`: pre-C-34 rows decode (`call_usage_decodes_pre_c34_rows_and_roundtrips_reported_cost`);
      `sum_usage` sums cost; `cost_summary` prices **per call** (`reported ?? table-estimate`, RowAcc
      through `merge_legacy_keys`). Failing-first: `cost_summary_prices_each_call_reported_or_table`.
- [x] `flux-cli`: `cost_suffix_prefers_reported_cost_over_unpriced_marker` (spec
      `openrouter/deepseek/deepseek-v4-flash:nitro`, builtin table, reported cost → ` · $…`, no `$?`,
      no once-note). `flux-tui`: `record_usage_accumulates_reported_cost_for_untabled_model`.
- [x] Live verify: `flux -p "say hi" -m openrouter/deepseek/deepseek-v4-flash:nitro` and
      `-m openrouter-anthropic/z-ai/glm-4.6` show a real dollar figure; `flux usage` prices both AND
      still renders pre-C-34 history from the real events.db.
- [x] Full workspace gate green (both workspaces + codegate).

## Progress

- 2026-07-04 filed from the recurring `$? (unpriced)` complaints (four user pastes, s_368-class
  sessions); design doc written same day.
- 2026-07-04 **step-0 probes run (live OpenRouter, both endpoints) — contingency step 5 is DEAD,
  and the BYOK rule changed:**
  - **Chat wire** (`deepseek/deepseek-v4-flash:nitro`, streaming, only the existing
    `stream_options:{include_usage:true}` sent): final SSE usage frame carries
    `"cost":0.0000020643,"is_byok":false,"cost_details":{"upstream_inference_cost":0.0000020643,…}`.
    No opt-in flag needed. **Gotcha vs. the original design: for non-BYOK,
    `upstream_inference_cost` EQUALS `cost`** — summing them double-counts. The frame includes
    `is_byok`; correct rule: `reported = cost + (is_byok ? upstream_inference_cost : 0)`.
  - **Messages wire** (`z-ai/glm-4.6`): non-streaming response usage carries the same
    `cost`/`is_byok`/`cost_details` block; streaming puts it on the **final `message_delta`**
    usage (`"cost":0.0005052`), while `message_start` usage has no cost — exactly the
    carry-forward shape the design anticipated. No `usage:{include:true}` injection needed on
    either endpoint; `openrouter.rs` stays untouched.
- 2026-07-04 **live-verified against the freshly installed binary** (`cargo install --path
  crates/flux-cli`, now `~/.cargo/bin/flux` 0.2.14 with this work included): `flux run --yes -m
  openrouter/deepseek/deepseek-v4-flash:nitro "say hi"` → `$0.0009`/`$0.0022` across two live calls
  (no `$? (unpriced)`); `flux run --yes -m openrouter-anthropic/z-ai/glm-4.6 "say hi"` → `$0.0160`
  (no `$?`). `flux usage` on the real `~/.flux/events.db`: the `openrouter/deepseek/…` row (76
  calls, mixing ~75 pre-C-34 unreported calls with the new reported ones) priced correctly at
  `$0.0341` via the per-call fold — proof the mixed-row regression guard (Q4b in the design) holds
  on real historical data, not just fixtures. Untabled OpenRouter models with no reported cost in
  their history (`qwen/qwen3.7-max`, `z-ai/glm-5.2`, `poolside/laguna-xs-2.1`) correctly show no
  dollar figure (unchanged pre-existing behavior) rather than a wrong number. All other
  session/all-time rows rendered without error, confirming events.db back-compat.

## Notes

- Update C-33's `note:` on completion — its TUI-`$?`-parity item then only concerns non-reporting
  providers (don't close C-33; its app-run sink + /goal items stand).
- Ripple: `Usage{..}` exhaustive literals in ~6 crates (messages/wire.rs, flux-eval,
  flux-server:686, flux-orchestrate, flux-sdk examples); ~5 `Money` literals in flux-cli tests.
- Shares files with A-34/A-35 (stream-resilience) — runs BEFORE that wave.

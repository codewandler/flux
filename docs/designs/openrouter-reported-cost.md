# Design: OpenRouter reported cost — price turns from the provider's own `cost`

**Status:** designed 2026-07-04 · **Pillar:** Core · **Stories:** [C-34](../stories/C-34-openrouter-reported-cost.md)

Extends [subscription providers & cost](subscription-providers-and-cost.md) (C-05 pricing model /
C-06 accounting): that epic *measures* usage and prices it from a static table; this story makes
cost **provider-authoritative** where the provider reports it.

## Why

The C-30 cost display renders ` · $? (unpriced)` for any metered cloud model without a
`PricingTable` row. The user's daily drivers (`openrouter/deepseek/deepseek-v4-flash:nitro`,
`openrouter-anthropic/z-ai/glm-*`, qwen variants) will never all have rows — OpenRouter serves
hundreds of models with shifting prices, and `~/.flux/pricing.toml` hand-maintenance doesn't scale.
Meanwhile OpenRouter already tells us the answer: per current docs (2026-07-04), **every** response
includes full usage accounting with `cost` (total USD credits charged) — the old
`usage: {include: true}` opt-in is deprecated/no-op on the chat-completions endpoint; for streaming
it arrives in the final SSE usage frame. `cost_details.upstream_inference_cost` covers the BYOK
share. The `/api/v1/messages` (Anthropic-compatible) endpoint's behavior is **unverified** — the
story opens with two ~$0.001 live probes that select the contingency.

## Approach

One vertical slice; the static table stays as fallback for non-reporting providers.

- **Carry:** `Usage.reported_cost_usd: Option<f64>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]` — old events.db rows decode; wire
  frames for non-reporting providers stay byte-identical). Drop `Usage`'s `Eq` derive (f64).
  `accumulate` **sums** it (`get_or_insert(0.0) += c`); a usage-less follow-up call can't erase
  recorded cost. Riding on `Usage` (vs. a new `Chunk` variant or parallel ledger) gets the events
  store, sub-agent rollup, SDK/server, and `TurnEnded` plumbing for free.
- **Parse:** chat wire — `ChatUsage.cost` + `is_byok` + `cost_details.upstream_inference_cost`
  (`serde(default)`, null-tolerant by `Option`). **Probe-corrected rule (2026-07-04): for non-BYOK
  responses `upstream_inference_cost` duplicates `cost`**, so
  `reported = cost + (is_byok ? upstream_inference_cost : 0)` — never a blind sum.
  Messages wire — `WireUsage.cost` (+ the same `is_byok`/`cost_details` rule), mapped in
  `From<WireUsage>`; extend the `message_delta` carry-forward so a cost seen once is sticky across
  later usage frames, and refresh `prior_usage` after each usage frame. Probes confirmed both
  endpoints report cost unconditionally (chat: final SSE usage frame; messages: final
  `message_delta`) — **no request-body change anywhere**, the `usage:{include:true}` contingency is
  dead.
- **Prefer:** at the single choke point `PricingTable::cost`, reported short-circuits the table:
  `Money` grows `source: CostSource::{Reported, Estimated}`; rendering unchanged (`~` stays
  subscription-only; reported `0.0` correctly prices `:free` models and kills their `$?`). Every
  sink (REPL turn line, GoalSink, `flux usage`, TUI) inherits the fix with zero sink changes.
- **Aggregate honestly:** `sum_usage` sums the field, and `cost_summary` prices **per call**
  (`reported ?? table-estimate`, folded via a small `RowAcc` through `merge_legacy_keys`) — naively
  pricing the aggregated row would let two reported calls short-circuit the table for a hundred
  legacy calls of a *tabled* model and silently under-report. Table pricing is linear in tokens, so
  pure-table rows are unchanged to the cent.

## Alternatives considered

- **Keep hand-maintaining the table** (status quo + pricing.toml): doesn't scale, always stale,
  and the provider's own number is strictly more truthful (routing/discount-aware).
- **New `Chunk::Cost` variant / parallel per-call ledger:** duplicates six existing usage seams for
  one number that is semantically usage accounting.
- **`GET /api/v1/generation` lookup after stream end:** an extra roundtrip per call and an ID to
  thread; only worth it if the Messages endpoint turns out to never report cost (then: follow-up
  story, not built now).
- **Render reported vs estimated differently (`~$`):** `~` already means subscription-equivalent;
  overloading it makes `~$2.10` ambiguous. The `source` flag exists for tests and a future C-33
  presentation pass.

## Risks & open questions

- Messages-endpoint cost unverified → probe first; worst case glm stays `$?` (status quo) and a
  follow-up story is filed. The parse side is tolerant (absent/null → `None` → table fallback), so
  doc-vs-reality drift can never produce a crash or a wrong number.
- `Usage{..}` exhaustive literals ripple across ~6 crates; `Money` literals in flux-cli tests —
  compiler-led, budgeted.
- Mid-turn `/model` switch across reporting/non-reporting providers: the live turn line prices via
  the final spec (approximate for that turn, documented); per-call `CallUsage` events stay exact.
- `record_call_usage_events` skips zero-token calls — a zero-token call with nonzero cost would be
  dropped; doesn't occur in practice.

## Acceptance / done

An OpenRouter turn on an untabled model renders a real ` · $0.000x` (no `$? (unpriced)`) on both
wires; `flux usage` prices mixed legacy+reported history per call and still decodes pre-C-34
events.db rows; non-reporting providers are byte-identical on the wire and priced from the table as
before. Failing-first tests named in the story; full gate green; live-verified against real
OpenRouter on both endpoints.

---
title: Usage & cost
description: "How flux computes token usage, pricing attribution, and the sources of cost truth across providers."
---

# Usage & cost

flux records model usage per call and rolls it up by session and provider. Use this page to understand
what the CLI summary means, where the numbers come from, and how to price models the built-in table
does not know yet.

Usage is stored in the session event log (`~/.flux/events.db`) with the canonical `provider/model`
spec that served the call. Dollar costs come from a built-in price table unless the provider reports
the actual charge; provider-reported cost wins.

## Per-turn cost

Every agent turn — in the REPL, `flux run`, and `flux app run`'s interactive console — ends with a
summary rule carrying the token breakdown and the turn's dollar cost; the [TUI](./cli.md) header shows
a running total instead:

```text
──────────────────── 4 steps · 12.4s · ctx 45.2k · out 1.3k · cache 44.1k (97% hit) · $0.0214
```

- `ctx` is the final prompt size (fresh input + cache reads + cache writes); `out` is generated tokens.
- Spend on a subscription provider (`claude/`, `codex/` — see [Providers](./providers.md)) renders as
  `~$0.0214 (sub)`: those bill a flat subscription, not per token, so the figure is the *equivalent*
  metered cost, never an actual incremental charge.
- Local and offline providers (`ollama*`, `mock`) show no cost — nothing is billed there.

## `flux usage`

`flux usage` (no flags) prints per-model tokens + cost for the current/last session, then an
all-sessions total, each with a turn-efficiency line:

```text
session: s_412
  anthropic/claude-sonnet-4-6   18 calls  ctx 512.4k · out 22.1k · cache read 471.9k · cache write 31.0k · $0.8412
  efficiency: 6 turns · 3.0 calls/turn · 1.2 iters/turn · 1.0 plans/turn · cache-read 92% · uncached-in 1.5k/turn · out 3.7k/turn

all sessions:
  anthropic/claude-sonnet-4-6  142 calls  ctx 3.9M · out 180.2k · cache read 3.5M · cache write 210.4k · $6.1180
  openai/gpt-5                  12 calls  ctx 402.1k · out 31.0k · $0.4102
                               total $6.5282
```

Rows are keyed by the canonical spec, so aliases (`sonnet`), Bedrock regional routing prefixes
(`us.anthropic.…`), and bare model ids all fold into stable per-provider keys. An OpenRouter
passthrough call keeps the *serving* provider as the outer prefix — e.g.
`openrouter-anthropic/anthropic/claude-sonnet-4.6` — so spend always lands under the provider that
bills for it, while pricing still resolves through the embedded model id.

## Unpriced models

When a metered cloud model has no pricing row, the cost is never silently guessed: the turn summary
shows ` · $? (unpriced)` and a one-time note explains the fix:

```text
note: no pricing entry for `openrouter/some/model` — add one to ~/.flux/pricing.toml to see $ costs
```

The marker fires only for known metered cloud providers, where a missing row hides real spend. Local
`ollama*` and unknown providers stay silent. OpenRouter models usually price even without a table row,
because the provider-reported cost is used directly (a `:free` model correctly shows as zero cost).

## Overriding prices: `~/.flux/pricing.toml`

The built-in table can be extended or corrected per model in `~/.flux/pricing.toml`. Overrides are
partial: any field you set replaces that tier, everything else keeps the built-in value. All rates are
USD per 1,000,000 tokens.

```toml
# Correct one tier of a known model.
[models."claude-opus-4-8"]
input = 20.0
cache_read = 2.0

# Price a model the built-in table doesn't know (set at least input + output;
# unset tiers on a new model default to 0.0).
[models."mistralai/mistral-large-2"]
input = 2.0
output = 6.0
```

Recognized fields per model: `input`, `output`, `cache_write`, `cache_read`, `reasoning`,
`audio_input`, `audio_output`. `reasoning`, `audio_input`, and `audio_output` are *surcharge* tiers —
those token counts are subsets of output/input and already billed once at the base rate, so the
built-ins keep them at `0.0`; set them only for a provider that prices those tokens apart. Key entries
by the model id as `flux usage` reports it (a bare id like `claude-opus-4-8`, or an OpenRouter
`vendor/model` id); lookup also tries the spec with its provider prefix stripped. A missing or
malformed file is ignored wholesale — a typo never takes cost reporting down, the built-ins stand.

## Related docs

- [Providers and models](./providers.md) — provider routing, subscription paths, and local providers.
- [CLI](./cli.md) — where turn summaries and `flux usage` appear.

# Design: Unify the Anthropic Messages provider — gateways become config, not codecs

**Status:** implemented 2026-07-28 (all six stories, incl. the review follow-up) · **Pillar:** Core ·
**Stories:** [C-168](../stories/C-168-parameterize-messages-codec-by-profile.md),
[C-169](../stories/C-169-route-gateway-specs-by-vendor-segment.md),
[C-170](../stories/C-170-extended-cache-ttl-through-gateways.md),
[C-171](../stories/C-171-chat-wire-cache-write-telemetry.md),
[C-172](../stories/C-172-openrouter-cache-ab-arm.md),
[C-173](../stories/C-173-inline-tool-call-salvage-on-the-messages-wire.md) (from the review)

## Baseline — measured 2026-07-28

`flux usage --harness flux --last 90d` over the local event store (32 days covered, 1263 sessions,
3931 calls, 100.4M tokens). **Hit % is `cache read / ctx`.** Only the OpenRouter-served rows are
reproduced; the `claude/*` and `codex/*` rows are subscription-billed (`sub`) and were the subject of
[llm-cache-review](llm-cache-review.md).

| model | calls | ctx | cache read | cache write | hit % | cost |
|---|---:|---:|---:|---:|---:|---:|
| `openrouter/anthropic/claude-fable-5` | 17 | 1.0M | — | — | **0%** | **$13.76** |
| `openrouter/anthropic/claude-opus-4.6` | 91 | 2.1M | — | — | **0%** | **$11.10** |
| `openrouter/openai/gpt-5.5` | 43 | 1.8M | 432.6k | — | 24% | $8.84 |
| `openrouter/google/gemini-3.5-flash` | 303 | 1.2M | 341.5k | — | 28% | $2.06 |
| `openrouter/moonshotai/kimi-k3` | 68 | 1.4M | 984.1k | — | 70% | $1.86 |
| `openrouter/deepseek/deepseek-v4-flash:nitro` | 766 | 22.5M | 16.4M | — | 73% | $2.52 |
| `openrouter-anthropic/anthropic/claude-sonnet-4.6` | 40 | 453.0k | 310.9k | 82.4k | **69%** | $0.75 |

Three facts this table establishes:

1. **The two `openrouter/anthropic/*` rows are the only ones at literally zero** — $24.86 across
   3.1M tokens. Every other vendor on the same wire caches, because their upstreams cache
   automatically; Anthropic is the one that requires explicit `cache_control` breakpoints, and the
   chat path never emits them.
2. **The fix already exists one row down.** `openrouter-anthropic/anthropic/claude-sonnet-4.6` — the
   same vendor, the same gateway, a different *provider name* — hits 69%. C-35 enabled caching there
   and nowhere else.
3. **No OpenRouter row reports a cache write at all**, including the ones plainly writing (deepseek
   reads 16.4M, so something is being written). OpenRouter reports
   `prompt_tokens_details.cache_write_tokens`; the chat codec drops it (C-171).

Unlike the cache-review epic's subject, this is **metered cash, not subscription**: of ~$13.45 real
spend in the last 14 days, the uncached OpenRouter/Anthropic path was ~82% of it.

## Validation — live, 2026-07-28

**The headline.** `openrouter/anthropic/claude-opus-4.6`, identical prompt, back to back:

| call | cache | read | write | cost |
|---|---:|---:|---:|---:|
| 1 (cold) | 0% | — | **4.9k** | $0.0473 |
| 2 (warm) | **62%** | **4.9k** | — | **$0.0194** |

**2.4x cheaper on the second call.** Note this is deliberately *not* framed as an A/B of two flux
builds: the historical row shows **zero cache writes across 91 calls**, so no amount of repetition
could ever have produced a read. The write in call 1 is the fix; the read in call 2 is the payoff.
The `flux usage` row for that model now reports both columns where it reported `—` for 32 days, and
keeps its `rpt` (reported-cost) marker.

**The 1h TTL is honoured (C-170).** Direct probe of `/api/v1/messages` with a ~7.7k-token system
prompt, unique nonce per arm so each forced a write:

| `cache_control` | `ephemeral_5m_input_tokens` | `ephemeral_1h_input_tokens` |
|---|---:|---:|
| `{"type":"ephemeral","ttl":"1h"}` | 0 | **7725** |
| `{"type":"ephemeral"}` | **7725** | 0 |

Accepted with no 4xx — including when OpenRouter routed the call to **Amazon Bedrock** upstream —
and it lands in the right tier rather than being silently ignored. That acceptance is what promoted
`extended_cache_ttl` from "unverified" to on for anthropic-served slugs; Bedrock-**direct** was not
probed (no credentialed run), so it stays off there rather than being enabled on inference. The same
responses also confirm OpenRouter reports **both** `cache_creation_input_tokens` (7725) *and* the
nested `cache_creation` split, which is what `messages/wire.rs:168` needs: the write column and the
per-TTL surcharge read from different fields, so a wire reporting only the split would silently zero
the cache-write column.

**The non-Anthropic vendors survived the endpoint move (C-169's open risk).** All still function and
report cost on the Messages wire:

| model | first call | repeat |
|---|---|---|
| `google/gemini-3.5-flash` | $0.0138, no cache figures | **64% · ↺3.9k · $0.0069** |
| `deepseek/deepseek-v4-flash` | $0.0006 | $0.0006, **no cache figures reported** |
| `z-ai/glm-4.6` | $0.0038 | — |

Gemini's automatic caching reports through unchanged. DeepSeek's initially did not — **resolved
during the C-173 probes**: on a slightly larger tool-shaped task it reported `cache 32% ↺4.1k`, so
the blank was the below-threshold hypothesis (a trivial one-shot), not a wire limitation. GLM reports
too (97% on a warm repeat).

**C-173 (from the code review): no inline tool-call leakage on this wire.** Six probes across
`z-ai/glm-4.6`, `qwen/qwen3-coder` and `deepseek/deepseek-v4-flash` — the models the
parse-resilience epic records as leaking `<tool_call>` markup on the Chat path — all returned
structured tool calls, including a compound task where each independently chose `read_many` with a
structured `paths` array. The Chat codec's salvage is Chat-wire-specific; its absence here is
correct, and the risk note below is closed.

## Why

The proximate cause is that `openrouter/anthropic/…` resolves to `OpenRouterChat` →
`build_chat_body` (`crates/flux-providers/src/openai.rs:52`, `:97`), which never consults a
`ProviderProfile` and therefore never emits `cache_control`. Its own comment says so:

> `system_text` joins segmented prompts in order (OpenAI has no cache-breakpoint notion; its implicit
> prefix caching still benefits from the segments' stable-first layout).

True for OpenAI. False for OpenRouter, which proxies Anthropic and passes `cache_control` through —
which is exactly what `OpenRouterProfile` (`openrouter.rs:49-52`) already knows.

The root cause is structural: **a `WireCodec` hardcodes its quirks profile**, so "the same protocol
over a different gateway" can only be expressed by writing another codec and another provider name.
Four codecs speak the Anthropic Messages protocol:

| codec | file:line | how it differs from `AnthropicMessages` |
|---|---|---|
| `AnthropicMessages` | `anthropic.rs:64` | — |
| `OpenRouterMessages` | `openrouter.rs:79` | pins `OpenRouterProfile`; Gemini tools projection |
| `OllamaMessages` | `ollama.rs:51` | pins `OllamaProfile` |
| `BedrockAnthropic` | `bedrock.rs:91` | **genuine wire differences** — `anthropic_version` in the body not a header, strips `model`/`stream`, AWS binary event-stream decode |

Two of the three are a single swapped line. That duplication is what produced a *second provider
name* for OpenRouter, and a second name is a fork in the road that users take wrong by default —
`openrouter/anthropic/claude-opus-4.6` is the obvious spelling and it is the bad one.

The cost is not only caching. `spec.rs:174` already records the other half:

> OpenRouter over its native Anthropic Messages endpoint — tool calls come back as structured
> `tool_use` blocks instead of leaking as `<tool_call>` text on the Chat path.

So the default spelling loses caching *and* structured tool calls.

## Approach

### Wave 1 — profile as config (C-168)

```rust
pub struct AnthropicMessages {
    profile: Arc<dyn ProviderProfile>,
    project_tools: bool,   // OpenRouter's Gemini schema view (schema.rs:13)
}
```

`build_body` becomes `build_messages_body(req, &self.profile.quirks_for(&req.model))`.
`OpenRouterMessages` and `OllamaMessages` are deleted; their modules keep the profile and credential,
which is all they were ever for. `BedrockAnthropic` stays — its differences are real wire behaviour,
not configuration, and folding three unrelated quirks into the shared codec's surface would buy
uniformity at the cost of honesty.

This wave is behaviour-preserving by construction: the existing cache suite (`messages/mod.rs`), the
OpenRouter codec tests (`openrouter.rs:167-302`) and the cross-codec sweep (`lib.rs:129-155`) must
pass **unchanged**.

### Wave 2 — one gateway, one wire (C-169)

The first design here routed by vendor segment: `openrouter/anthropic/*` to the Messages wire,
everything else to Chat. Implementing it surfaced the flaw — `docs/model.md:250-308` **recommends**
the Messages endpoint for *non*-Anthropic vendors (`z-ai/glm-4.6`, `qwen/qwen3-coder`,
`deepseek/deepseek-chat`) precisely because it returns structured `tool_use` instead of leaking
`<tool_call>` markup. Vendor routing would have made that unreachable: a rename that quietly removed
a documented capability.

So the decision is simpler and stronger. **OpenRouter's Messages endpoint is model-agnostic, so flux
uses it for every model OpenRouter proxies.** `OpenRouterChat` is deleted along with its endpoint
constant; the Chat codec survives only for `openai` and `ollama`, which genuinely speak that wire.

| spec | wire | endpoint | profile |
|---|---|---|---|
| `openrouter/**` | Messages | `/api/v1/messages` | `OpenRouterProfile` |

A spec still reads `<gateway>/<vendor>/<model_id>` — a triple — but the vendor segment is simply part
of OpenRouter's own model id rather than a flux-side selector, so `parse_model_spec` (`spec.rs:53`)
needs no change: it splits once and leaves `anthropic/claude-opus-4.6` intact. `OpenRouterProfile`
still keys `prompt_caching` on the `anthropic/` prefix, since only Anthropic-served models honour
`cache_control`; every other vendor caches automatically upstream.

`openrouter-anthropic` is removed from `KNOWN_PROVIDERS` outright — no alias, per the repo's
clean-cutover rule — with a targeted error naming the new spelling. **Breaking**
(`KNOWN_PROVIDERS` is a public const and the name is user-facing) ⇒ next MINOR.

Two things deliberately do **not** move:

- `ollama-anthropic` keeps its name. Local model ids carry no vendor prefix and ollama's Messages
  endpoint is a transport choice rather than a property of the model, so there is nothing to derive
  from. It merely loses its duplicate codec in wave 1.
- `flux_core::pricing::known_provider` keeps recognising `openrouter-anthropic`. The event store is
  append-only, and rows written under the old spelling must keep splitting the same way forever;
  dropping it there would silently reclass every historical row as a bare model id and move spend
  between `flux usage` rows retroactively. Retired names belong in the *parser*, not in the set of
  providers a user can select.

### Wave 3 — maximum caching on every transport (C-170)

`OpenRouterProfile.extended_cache_ttl` is `false`, commented "Unverified through the OpenRouter
proxy" (`openrouter.rs:53-55`). OpenRouter documents `"ttl": "1h"` and the four-breakpoint limit —
the same contract `AnthropicProfile` uses. Verify live, then enable, so the gateway gets the 1h
stable prefix alongside the rolling 5m tail (`req.cache_tail` is gated on `prompt_caching`, so it
begins applying the moment wave 2 lands). Same question for `BedrockProfile` (`bedrock.rs:72`).

The goal stated by the maintainer: **maximum caching when using a model, regardless of whether it is
reached via `anthropic` directly, the `claude` subscription, `openrouter`, or a future gateway.**

### Wave 4 — telemetry and measurement (C-171, C-172)

C-171 decodes `cache_write_tokens` on the chat wire so writes stop being invisible. C-172 fixes
`bench/cache-ab.sh:57`, whose `openrouter*` glob matches the chat path but selects `FLUX_CACHE_TAIL`
— a switch that does nothing on that wire, so both arms run identical bodies while the harness
reports "no difference".

## Alternatives considered

- **Stamp `cache_control` onto the chat body** (convert message content to a one-element parts array,
  gated on the `anthropic/` prefix). Rejected: it reaches caching parity but leaves the `<tool_call>`
  text leakage, and it adds a *third* place that knows the Anthropic cache layout instead of removing
  the second.
- **OpenRouter's top-level automatic `cache_control`** (one line; the gateway auto-places
  breakpoints). Rejected for the same reason plus a testability one — placement becomes the gateway's
  choice, so `cache_layout_contract` can't pin it, and there is no way to express the 1h stable
  prefix. Cheap, but it buys the smaller half of the win.
- **Keeping `openrouter-anthropic` as a deprecated alias.** Rejected per the repo's no-fallbacks rule;
  a clear error at spec-parse time is the migration path.
- **Routing OpenRouter by vendor segment** (Anthropic to Messages, the rest to Chat). Rejected after
  implementation: it would have stranded the documented GLM/qwen/deepseek-over-Messages route with no
  way back, trading a capability for a rename. Keeping both wires also keeps both codecs, which is
  the duplication this epic exists to remove.
- **Folding `BedrockAnthropic` in too.** Rejected: version-in-body, field stripping and binary
  event-stream decoding are wire behaviour, not config. Uniformity here would be cosmetic.

## Risks & open questions

- **Silent cost regression.** `flux-core/src/pricing.rs` (16 refs) and `flux-events/src/projection.rs`
  (5) key rows on the provider prefix. If the keys don't migrate with the spec, cost lookups degrade
  to `$? unknown model` in `flux usage` rather than failing loudly. C-169 pins this with a test.
- **A gateway rejecting `ttl:"1h"`** would 4xx *every* long-system-prompt request. This exact failure
  was caught once in review (commit `4a76315`, where the 1h quirk rode `prompt_caching` and so reached
  Bedrock and OpenRouter unverified). C-170 verifies before enabling, and the quirk is separate, so
  the blast radius is one profile.
- **The `rpt` marker asymmetry is unexplained.** `WireUsage` decodes OpenRouter's `cost`
  (`messages/wire.rs:111-136`, `:158`) exactly as `ChatUsage` does, yet `openrouter-anthropic` rows
  show table-estimated cost while `openrouter/*` rows show reported. After C-169 that becomes the path
  all Anthropic-via-gateway spend rides on, so C-171 answers it.
- **A/B methodology.** The cache is content-addressed and org-scoped, so arm 2 reads arm 1's writes; a
  naive same-prompt A/B is worthless. Alternate arm order and confirm step counts match before
  believing any pair — see [llm-cache-review](llm-cache-review.md) for how this misled the last epic
  twice.
- **`project_tools` as a bool** is honest for two gateways; a third with a different schema dialect
  turns it into an enum. Not worth generalizing before there is a second case.
- **The Chat wire's inline tool-call salvage did not come along.** `<tool_call>` / `<function=`
  recovery (`openai.rs:508-620`) is reachable only from `map_chat_stream`; `messages/` has none. So
  `openrouter/z-ai/glm-4.6` had that recovery before C-169 and does not now. The wire guarantees a
  *well-formed* tool call arrives as a block — it cannot stop a model from writing tool-ish text into
  a content block. Untested either way, and the fix is not a straight port: the salvage would land in
  the codec Anthropic-direct shares, where `<tool_call>` in prose is legitimate text a coding agent
  writes when explaining tool syntax. **Closed 2026-07-28**: verified across glm/qwen/deepseek with
  single and compound tool tasks — all structured, zero leakage. See
  [C-173](../stories/C-173-inline-tool-call-salvage-on-the-messages-wire.md) and the validation
  section above.

## Acceptance / done

The union of the five stories' acceptance, plus:

- `openrouter/anthropic/*` builds an Anthropic Messages body carrying `cache_control`, and a measured
  cache hit rate materially above zero is recorded in this document (C-169, verified via C-172).
- Exactly one codec implements the Anthropic Messages protocol for the non-Bedrock transports, and
  adding a gateway requires no new `WireCodec` (C-168).
- `flux usage` prices the new spec — no `$? unknown model` row — and shows cache reads **and** writes
  for OpenRouter (C-169, C-171).
- Tool calls over `openrouter/anthropic/*` arrive as structured blocks, not `<tool_call>` text.
- The standard gate stays green: `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`,
  `cargo test -p flux-codegate`.

---
id: C-171
title: Decode cache-write tokens on the chat wire and explain the missing reported-cost marker
pillar: Core
status: done
epic: messages-provider-unification
design: docs/designs/messages-provider-unification.md
note: "Every OpenRouter row in `flux usage` has a blank cache-write column — including deepseek, which reads 16.4M tokens so is plainly writing. `map_chat_stream` (openai.rs:350-372) maps only `cached_tokens`; OpenRouter also reports `prompt_tokens_details.cache_write_tokens`. Same class as the Responses-wire drop fixed in 4a76315."
---

# Decode cache-write tokens on the chat wire and explain the missing reported-cost marker

## Goal
Make cache writes visible on the OpenAI chat wire so gateway caching can be measured rather than
inferred, and resolve why Messages-wire OpenRouter rows show table-estimated cost. Serves Core: after
C-169 the chat wire still serves every non-Anthropic vendor through OpenRouter, and a blank column
reads as "no writes" when it means "not decoded".

## Acceptance
- [ ] Failing-first test: an SSE usage frame carrying
      `prompt_tokens_details: {cached_tokens, cache_write_tokens}` maps to a `Usage` with both
      `cache_read_input_tokens` and `cache_creation_input_tokens` populated. Follow the existing
      `last_usage(sse)` helper pattern (`messages/mod.rs:1018`); chat-usage tests live at
      `openai.rs:1497` and `:1534`.
- [ ] `PromptTokensDetails` (`openai.rs:295-299`) grows `cache_write_tokens`, `#[serde(default)]` so
      every non-OpenRouter backend that omits it still decodes.
- [ ] `input_tokens` arithmetic stays correct — `prompt_tokens` is the whole prompt including the
      cached prefix, and the existing `saturating_sub(cached)` must not double-count writes. Pin the
      relationship with the test, not by inspection.
- [ ] Investigate the `rpt` marker asymmetry: `WireUsage` decodes OpenRouter's `cost` / `is_byok` /
      `cost_details` (`messages/wire.rs:111-136`, `:158`) exactly as `ChatUsage` does, yet
      `openrouter-anthropic` rows show table-estimated cost while `openrouter/*` rows show reported.
      Determine whether `/api/v1/messages` omits `cost`, and record the finding in the design doc.
- [ ] If the Messages endpoint does report cost and flux is dropping it, fix that too — after C-169
      this is the path all Anthropic-via-gateway spend rides on, so a wrong number here is the number
      the maintainer sees.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. `PromptTokensDetails` grew `cache_write_tokens`; `map_chat_stream` maps it to
  `cache_creation_input_tokens` and subtracts both tiers from fresh input, with the same
  not-a-subset guard the Responses wire uses (trust the total, drop the breakdown, warn).
- Two new tests: `chat_usage_captures_cache_write_tokens` and
  `chat_usage_keeps_the_prompt_total_when_the_tiers_are_not_subsets`, plus a `last_chat_usage`
  helper. `context_tokens()` still equals the prompt total in both.
- **The `rpt` question is answered:** OpenRouter's Messages endpoint *does* report `cost` /
  `is_byok` / `cost_details` (seen directly in the probe response), and post-cutover
  `openrouter/anthropic/*` rows in `flux usage` carry the `rpt` marker. The old absence on
  `openrouter-anthropic` rows was historical — those rows predate the C-34 Messages-side decode. No
  code change needed.
- Scope note: after C-169 the chat wire no longer serves OpenRouter at all, so this fix now benefits
  `openai` (the models with explicit cache breakpoints) rather than the gateway.

## Follow-up

- ~~DeepSeek reports no cache split on the Messages wire~~ — **RESOLVED 2026-07-28 during the C-173
  probes**: on a slightly larger tool-shaped task `deepseek-v4-flash` reported `cache 32% ↺4.1k`
  through the Messages wire. The earlier blank was the below-threshold hypothesis (a trivial "Say
  OK" prompt), not a wire limitation. Nothing to fix.

## Notes
- Independent of C-168/C-169 — can land in any order.
- `cache_discount` is documented by OpenRouter and read nowhere in the repo. Not required here; note
  whether it would be a better signal than deriving savings from token counts.
- `flux_core::Usage` (`flux-core/src/stream.rs:14-59`) already has the fields; only the chat decode is
  missing. `cache_creation_1h_input_tokens` is a *subset* of the write total — if OpenRouter ever
  reports a per-TTL split, carry it forward across delta frames the way the Anthropic codec does.

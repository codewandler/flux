---
id: C-134
title: Cache the conversation tail — a rolling breakpoint on the last message block
pillar: Core
status: ready
priority: 4
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "flux never stamps cache_control anywhere in `messages` (only system_field/segmented_system_field do, mod.rs:127,140; ContentBlock has no carrier) — so the cacheable prefix stops where the system prompt ends and the whole growing transcript is re-priced at full input rate every round"
---

# Cache the conversation tail — a rolling breakpoint on the last message block

## Goal
Extend the cached prefix past the system prompt into the conversation, so a multi-round turn stops
re-paying full input rate for a transcript it already sent. This is the headline fix of the epic and
the one with the largest expected effect on subscription spend and latency.

## Acceptance
- [ ] A cache-control carrier reaches the Messages codec without putting an Anthropic-specific field
      on `flux_core::ContentBlock`. Preferred shape (design): a flag on `flux_provider::Request`
      (e.g. `cache_tail: bool`) that `build_messages_body` consumes by stamping the last content
      block of the last message. Any other shape must be justified in Progress.
- [ ] Failing-first test in `crates/flux-providers/src/messages/mod.rs`: a request with a
      multi-message conversation and `cache_tail` set produces `cache_control: {"type":"ephemeral"}`
      on the final content block of the final message, and no other message block carries one.
- [ ] **The ≤4 breakpoint cap becomes a union budget.** `cache_breakpoints`
      (`messages/mod.rs:178`) currently caps only system segments and has no knowledge of a message
      breakpoint. It takes the non-system breakpoint count as input and caps
      `system + messages ≤ MAX_CACHE_BREAKPOINTS`. Test: the subscription-claude intent layout
      (identity prefix + `INTENT_SYSTEM` + index + base = 4 today, `staged.rs:2264`) plus a tail
      breakpoint yields exactly 4 `cache_control` keys in the built body, never 5 — reuse
      `count_cache_control` from the A-23 tests.
- [ ] The test pins **which** system breakpoint is dropped under pressure, not just the count. The
      existing "keep the largest" rule should extend naturally, but the choice determines what
      survives a tool-set change and must be asserted, not incidental.
- [ ] The 20-block lookback is addressed explicitly: a round appending more than 20 content blocks
      (a wide parallel tool call is one assistant message of N `tool_use` blocks plus one user
      message of N `tool_result` blocks) must not silently miss the previous round's entry. Either
      bound the per-round block count or place an intermediate breakpoint — state which in Progress
      and cover it with a test.
- [ ] Non-Anthropic codecs are unaffected: `openai`, `openrouter` (chat), `ollama`, and `bedrock`
      bodies are byte-identical with and without the flag. Test asserts this for at least the
      Responses path, which shares `Request`.
- [ ] Live-validated with the C-133 harness against `claude/*`: cache_read share on the fixed
      multi-round turn improves against the recorded baseline; before/after in the design doc.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- (not started)

## Notes
- Blocked on C-133 for the baseline number, not for the implementation.
- Anthropic renders `tools` → `system` → `messages`. That is why the system breakpoints are worth
  keeping despite the budget squeeze: the earliest one also covers the tool catalog.
- `bedrock.rs` builds its own body (`crates/flux-providers/src/bedrock.rs:1521` constructs
  `system_segments: Vec::new()`) — check whether it should inherit the tail breakpoint in this story
  or a follow-up. Out of epic scope unless it is free.
- Minimum cacheable prefix differs by model (512 tokens on Opus 5; 1024 on Sonnet 5 / Opus 4.8;
  higher on older generations). A tail breakpoint below the minimum silently does not cache.
  `segmented_system_field` has no size gate at all today — `CACHE_MIN_CHARS` (`mod.rs:120`) guards
  only the unsegmented path. Whether to add one is deliberately left to the C-133 measurement.

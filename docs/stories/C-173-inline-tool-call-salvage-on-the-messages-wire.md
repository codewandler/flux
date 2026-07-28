---
id: C-173
title: Decide whether inline tool-call salvage belongs on the Messages wire
pillar: Core
status: done
epic: messages-provider-unification
design: docs/designs/messages-provider-unification.md
note: "C-169 moved every OpenRouter model to the Messages wire, but the `<tool_call>` / `<function=` text salvage (openai.rs:508-620) is reachable only from `map_chat_stream` — `messages/` has none. Models that leak tool syntax as content (qwen/deepseek/glm, per the parse-resilience epic) used to be recovered on the chat spelling and now are not."
---

# Decide whether inline tool-call salvage belongs on the Messages wire

## Goal
Establish whether a model can still leak tool calls as text through OpenRouter's Messages endpoint,
and if so give that wire a recovery path — without breaking legitimate prose on Anthropic-direct.
Serves Core: C-169 traded a recovery path for a better default, and that trade was made implicitly
rather than measured.

## The gap

`INLINE_TOOL_OPEN` / `<function=` salvage lives in `crates/flux-providers/src/openai.rs:508-620` and
is called only from `map_chat_stream`. `map_messages_stream` has no equivalent (grep for `tool_call>`
under `messages/` returns nothing). Before C-169, `openrouter/z-ai/glm-4.6` rode the Chat wire and got
that recovery; it now rides Messages and does not.

The counter-argument, which is why this is a question and not a bug: the Messages wire exists
*precisely* so tool calls come back as structured `tool_use` blocks, and the previously-recommended
spelling (`openrouter-anthropic/z-ai/glm-4.6`) never had salvage either. The wire guarantees that a
**well-formed** tool call arrives as a block — it cannot stop a model from writing tool-ish text into
a content block instead.

## Acceptance
- [ ] Evidence first: drive `z-ai/glm-4.6`, `qwen/qwen3-coder` and a deepseek slug over
      `/api/v1/messages` with a tool-shaped task and record whether any emits `<tool_call>` /
      `<function=` markup as text. If none do, close this story with that recorded — the salvage is
      Chat-wire-specific and its absence is correct.
- [ ] If they do leak: the salvage becomes reachable from `map_messages_stream`, **gated** rather
      than universal. It must NOT apply to Anthropic-direct or the `claude` subscription — a coding
      agent legitimately writes `<tool_call>` in prose when explaining tool syntax, and salvaging
      that would eat real assistant text. The gate belongs with the other per-transport divergences
      (a `MessagesQuirks` field, resolved by profile), not a model-substring check at the parse site.
- [ ] Failing-first test: a Messages SSE fixture whose text block contains `<tool_call><function=…>`
      yields a `tool_use` block under a gateway profile, and stays verbatim text under
      `AnthropicProfile`.
- [ ] `docs/model.md`'s OpenRouter section states the outcome either way, since it used to promise
      recovery on the chat path.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28, closed on the evidence path: **no leakage observed — the salvage is
  Chat-wire-specific and its absence on the Messages wire is correct.** No code change.
- Probes: two tool-shaped tasks per model (single read; then a compound "read both files" task)
  against `openrouter/z-ai/glm-4.6`, `openrouter/qwen/qwen3-coder`,
  `openrouter/deepseek/deepseek-v4-flash` — six runs total. Every run returned a **structured**
  tool call that flux executed (`read`, and on the compound task all three independently chose
  `read_many` with a structured `paths` array — exactly the argument shape that leaks when a model
  falls back to inline markup). Zero `<tool_call>` / `<function=` text in any transcript; every
  task answered correctly.
- `docs/model.md`'s OpenRouter section now records the verification.
- Bonus finding while probing: the DeepSeek cache question from C-171's follow-up is resolved —
  `deepseek-v4-flash` reported `cache 32% ↺4.1k` on the compound task, so the earlier blank was the
  below-threshold hypothesis (a trivial prompt), not a wire limitation. GLM also reports (97% on a
  warm repeat).

## Notes
- Surfaced by review of C-169, not by a user report — no leak has been observed on the Messages wire
  yet. Written as "verify, then act" rather than "port the code" for that reason.
- Related: the parse-resilience epic (`docs/designs/`… qwen double-encodes `emit_plan` `ast`,
  deepseek malformed args) is the record of which models leak and how.
- A-30 (stringified-AST fallback) is the adjacent recovery at the plan layer; this one is at the
  codec layer, before the plan is ever parsed.

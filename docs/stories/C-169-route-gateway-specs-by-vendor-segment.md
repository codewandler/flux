---
id: C-169
title: Serve every OpenRouter model over the Messages wire and retire openrouter-anthropic
pillar: Core
status: done
epic: messages-provider-unification
design: docs/designs/messages-provider-unification.md
note: "BREAKING. `openrouter/anthropic/*` — the obvious spelling — landed on the chat codec, which emits no `cache_control` and leaks tool calls as `<tool_call>` text; measured 0% cache across 3.1M tokens / $24.86 in 32 days, while `openrouter-anthropic/*` hit 69%. OpenRouter's Messages endpoint is model-agnostic, so it becomes the only wire and `OpenRouterChat` is deleted."
---

# Serve every OpenRouter model over the Messages wire and retire openrouter-anthropic

## Goal
Make `openrouter/<vendor>/<model>` — the spelling users actually type — the one that caches and
returns structured tool calls, by using OpenRouter's Messages endpoint for every model it proxies and
deleting the second provider name that existed to reach it. Serves Core: the largest metered-cash
waste visible in `flux usage`, and one fewer fork in the road.

## Acceptance
- [ ] `openrouter` resolves to the Messages codec for every model. `OpenRouterChat`, `openrouter_api`,
      `openrouter_from_env` and the chat `OPENROUTER_ENDPOINT` const are deleted; the Chat codec
      survives only for `openai` and `ollama`, which genuinely speak that wire.
- [ ] Failing-first test: an `openrouter/anthropic/*` request builds a body containing
      `cache_control`, and a non-Anthropic slug on the same codec keeps its tool schemas verbatim
      unless it is a Gemini id (the projection is keyed on the model, not the transport).
- [ ] `openrouter-anthropic` is removed from `KNOWN_PROVIDERS` (`spec.rs:17`) and its dispatch arm.
      No alias, per the clean-cutover rule.
- [ ] `parse_model_spec` returns a targeted error for an `openrouter-anthropic/…` spec naming the new
      spelling, not the generic "unknown provider" list. Pinned by a test on the message.
- [ ] The triple survives parsing: `openrouter/z-ai/glm-4.6` parses to provider `openrouter`, model
      `z-ai/glm-4.6` — the vendor segment is part of OpenRouter's own model id, not a flux selector.
- [ ] **Pricing/projection keys — the silent-failure risk.** `flux-core/src/pricing.rs` and
      `flux-events/src/projection.rs` key rows on the provider prefix; an unrecognised key degrades to
      `$? unknown model` in `flux usage` instead of erroring. `known_provider` therefore **keeps**
      `openrouter-anthropic` (the event store is append-only — historical rows must keep splitting the
      same way), while `KNOWN_PROVIDERS` drops it. Pinned by a test that both spellings price to the
      same row.
- [ ] Live surfaces swept: `flux-cli/src/{args.rs,main.rs,auth_cmd.rs}`, `docs/model.md`,
      `website/docs/agent/{providers,cost}.md`, `README.md`, `docs/architecture.md`,
      `examples/eval-synthetic.flux`, `bench/{run-ttff,run-tbench-compare,run-tbench-loop,run-synthetic-loop}.sh`.
- [ ] **Not** rewritten: `CHANGELOG.md`, closed stories, `docs/archive/`, and `bench/**/results/**` —
      historical records keep the spelling that was true when written.
- [ ] `WHATS-NEW.md` gains an `### Action needed` entry naming the `openrouter-anthropic/…` →
      `openrouter/…` rename, in customer language with no story ids or crate names.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. `openrouter` resolves to the Messages codec for every model;
  `OpenRouterChat`, `openrouter_api`, `openrouter_from_env` and the chat endpoint const are deleted.
  `openrouter-anthropic` removed from `KNOWN_PROVIDERS`; a spec naming it now fails with the new
  spelling via a `RETIRED_PROVIDERS` table.
- **Scope changed mid-implementation** (see Notes): the vendor-segment routing table was written,
  then removed on evidence when the docs sweep showed `docs/model.md` recommends the Messages
  endpoint for non-Anthropic vendors. One wire for the gateway is simpler and strictly better.
- Pricing/projection guard in place: `known_provider` keeps the retired name (append-only event
  store), `KNOWN_PROVIDERS` drops it, pinned by `rates_for_resolves_the_vendor_routed_openrouter_spec`.
- Swept CLI help/auth mapping, `docs/model.md`, `docs/architecture.md`, `README.md`, website
  providers+cost, `examples/eval-synthetic.flux` and the four bench scripts. Historical records
  (CHANGELOG, closed stories, `docs/archive/`, `bench/**/results/**`) deliberately untouched.
- CHANGELOG + WHATS-NEW (`### Action needed`) entries written.
- **Live-verified:** `openrouter/anthropic/claude-opus-4.6` cold call writes 4.9k cache tokens where
  it previously wrote nothing; warm call reads them back at 62%, $0.0473 → $0.0194.

## Notes
- Depends on C-168 — the shared codec has to take a profile before a gateway can pick one.
- Breaking: `KNOWN_PROVIDERS` is a public const and the provider name is user-facing ⇒ next MINOR
  (0.31.0) per the pre-1.0 rule in `AGENTS.md`.
- **Scope changed during implementation.** The first design routed by vendor segment (Anthropic to
  Messages, the rest to Chat). That would have stranded the GLM/qwen/deepseek-over-Messages route
  `docs/model.md:250-308` recommends — the Messages endpoint is model-agnostic and returns structured
  `tool_use` for every vendor, which is why the docs prefer it. One wire for the whole gateway is both
  simpler and strictly better; it also deletes the second codec instead of keeping it.
- Open risk carried by C-171: whether OpenRouter's Messages endpoint reports `cached_tokens` and
  `cost` for non-Anthropic vendors the way the Chat wire did. deepseek (766 calls), gemini (303) and
  kimi (68) change endpoint here, and their measured caching must not silently vanish from `flux usage`.
- Measured baseline in the design doc: `openrouter/anthropic/claude-fable-5` $13.76 and
  `openrouter/anthropic/claude-opus-4.6` $11.10, both at 0% cache read.

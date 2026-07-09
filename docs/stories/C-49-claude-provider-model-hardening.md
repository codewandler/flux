---
id: C-49
title: Claude provider hardening — model-gated quirks, fable alias, spec parsing
pillar: Core
status: done
priority:
note: e2e sweep 2026-07-09 found haiku 400s (adaptive thinking), `claude/fable` 404s, `claude/` reaches the wire
---

# Claude provider hardening — model-gated quirks, fable alias, spec parsing

## Goal
Every documented `claude/<alias-or-id>` spec either works or fails client-side with an
actionable message. An e2e sweep (2026-07-09) found `claude/haiku` 400s because the Messages
body builder sends `thinking: {"type": "adaptive"}` (and would send `output_config.effort` /
sampling params) to models that reject them; `claude/fable` 404s (no alias); `claude/` sends an
empty model id to the API; the `sonnet` alias pins the previous-generation model.

## Acceptance
- [x] `claude/haiku` (and `anthropic`/`aws`/`openrouter-anthropic` haiku ids) run: adaptive
      thinking and effort are omitted for models that reject them, gated per model id in one
      shared capability helper (`messages::quirks`). Failing-first: body-builder tests asserting
      a haiku request with thinking on carries **no** `thinking`/`output_config` key.
- [x] `temperature`/`top_p` are omitted for models that reject sampling params (Fable 5,
      Opus ≥ 4.7, Sonnet ≥ 5), and still emitted for models that accept them (≤ 4.6, haiku).
- [x] `claude/fable` resolves to `claude-fable-5`; bare `fable` works like the other short
      aliases. `sonnet` resolves to the current Sonnet (`claude-sonnet-5`); `haiku` uses the
      undated alias id. The pricing mirror (`flux-core::pricing::resolve_alias`) stays in
      lock-step; all resolved ids remain priced.
- [x] `claude/` (empty model) fails client-side with an actionable error; bare `claude`
      defaults to the subscription's sonnet, mirroring bare `codex`. The unknown-bare-word
      error no longer suggests `claude/claude`.
- [x] Gate green: build, workspace tests, clippy -D warnings, fmt, codegate.
- [x] Live e2e re-run of the failing matrix rows passes: `claude/haiku`, `claude/fable`,
      `claude/sonnet`, `claude/opus`, `claude/claude-fable-5`, `claude/claude-sonnet-5`,
      bare `claude`, `claude/` (client-side error).

## Progress
- [x] E2E sweep + root-cause (2026-07-09, pre-story): quirks profiles ignore the model;
      `resolve_model` lacks `fable`; CLI spec parsing lets empty models through.
- [x] Capability helper + model-gated profiles (anthropic, bedrock, openrouter) — 2026-07-09.
- [x] Alias + pricing mirror + CLI spec parsing + docs (docs/model.md, website agent/claude-code page + providers refresh) — 2026-07-09.
- [x] Gate + live e2e verify — 2026-07-09: build/test/clippy/fmt/codegate + website build green; live: claude/haiku→"2", claude/fable→"2", claude/sonnet→sonnet-5 "2", bare claude→"2", claude/opus→"2", claude/ → client-side error.

## Notes
- Root causes: `AnthropicProfile::quirks_for` (crates/flux-providers/src/anthropic.rs) and
  `BedrockProfile`/`OpenRouterProfile` hard-code `thinking_adaptive: true`;
  `build_messages_body` emits `top_p` unconditionally; `build_provider` (flux-cli) forwards
  empty models.
- Anthropic capability ground truth (2026-07 API): adaptive thinking is 4.6-family+ (Fable 5,
  Opus 4.8/4.7/4.6, Sonnet 5/4.6); Haiku 4.5 and every ≤ 4.5 model reject it. `output_config.
  effort` errors on Haiku 4.5/Sonnet 4.5. `temperature`/`top_p`/`top_k` are rejected on
  Fable 5, Opus 4.8/4.7, Sonnet 5.
- Deliberate non-goals: Bedrock alias table keeps its region-catalog ids (no invented
  `sonnet-5`/`fable` inference-profile ids); ollama profile untouched (local models).
- `aws/haiku` regression (C-09 had live-verified it) is fixed by the same gating; not
  re-verified live here (no AWS credentials on this box).

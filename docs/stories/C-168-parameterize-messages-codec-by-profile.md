---
id: C-168
title: Parameterize the Messages codec by profile so gateways stop needing their own codec
pillar: Core
status: done
epic: messages-provider-unification
design: docs/designs/messages-provider-unification.md
note: "`OpenRouterMessages` (openrouter.rs:79) and `OllamaMessages` (ollama.rs:51) differ from `AnthropicMessages` (anthropic.rs:64) by exactly one line — which `ProviderProfile` they pin — plus OpenRouter's Gemini tools projection. That duplication is what produced a second provider name for OpenRouter, and the second name is the one nobody types."
---

# Parameterize the Messages codec by profile so gateways stop needing their own codec

## Goal
Make `AnthropicMessages` take its quirks profile as configuration instead of hardcoding it, and delete
the two codecs that existed only to swap that line. Serves Core: one implementation of the Anthropic
Messages protocol to change when the protocol moves, and a gateway becomes `(endpoint, credential,
profile)` data rather than new code.

## Acceptance
- [ ] `AnthropicMessages` carries `profile: Arc<dyn ProviderProfile>` and a `project_tools: bool` (for
      OpenRouter's Gemini schema view, `schema.rs:13`); `build_body` calls
      `build_messages_body(req, &self.profile.quirks_for(&req.model))`.
- [ ] `OpenRouterMessages` (`openrouter.rs:77-95`) and `OllamaMessages` (`ollama.rs:49-63`) are
      deleted. Their modules keep `OpenRouterProfile` / `OllamaProfile` and their credentials — that is
      what those modules are for.
- [ ] Constructors updated: `anthropic_from_env`, `claude_oauth`, `openrouter_anthropic_from_env`,
      `ollama_anthropic_api` each build the shared codec with their profile.
- [ ] `wire_headers` still returns `anthropic-version` for all three transports (Bedrock is untouched
      and keeps returning none — its version rides in the body).
- [ ] **Behaviour-preserving by construction:** the existing cache suite (`messages/mod.rs:844-1340`),
      the OpenRouter codec tests (`openrouter.rs:167-302`) and the cross-codec sweep
      (`lib.rs:129-155`) pass **unchanged** — no test edits in this story beyond the one added below.
- [ ] New test: the cross-codec sweep asserts `project_tools` per provider — OpenRouter projects the
      adversarial schema, Anthropic-direct and ollama do not — so the flag can't silently flip.
- [ ] `BedrockAnthropic` is left alone (`bedrock.rs:91`); its differences are wire behaviour, not
      config. Say so in a code comment so the asymmetry reads as deliberate.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. `AnthropicMessages` now carries `profile: Arc<dyn ProviderProfile>` and an
  optional `ToolProjection` fn pointer (`anthropic.rs`); `AnthropicMessages::direct()` is the
  Anthropic/`claude` constructor. `OpenRouterMessages` and `OllamaMessages` deleted, replaced by
  `openrouter_messages_codec()` / `ollama_messages_codec()` returning the shared codec under their
  profile. The projection is a fn pointer so the gateway module owns it — `anthropic.rs` never
  imports `openrouter_tools`.
- Behaviour-preserving as required: the whole pre-existing suite passed with no assertion changes
  (only call sites updated for the type change). Added the ollama row to the cross-codec sweep in
  `lib.rs` so `project_tools` is pinned per transport.
- `BedrockAnthropic` left alone, with the reason stated in the `anthropic.rs` module doc.

## Notes
- Pure refactor. No spec-resolution changes here — that is C-169, which depends on this one.
- `ProviderProfile` is already a trait with `quirks_for(&self, model: &str) -> MessagesQuirks`
  (`messages/quirks.rs:53-55`), so no trait surgery is needed; the four profiles are unchanged.
- Watch `flux-codegate`: the codec now holds an `Arc<dyn ProviderProfile>`, so check the layering lint
  is happy with the trait object crossing module boundaries inside the crate.

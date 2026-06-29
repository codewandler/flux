---
id: C-03
title: Codex provider hardening — account-id, usage tiers, reasoning continuity
pillar: Core
status: backlog
priority:
design: docs/designs/subscription-providers-and-cost.md
theme: subscription-providers-cost
---

# Codex provider hardening — account-id, usage tiers, reasoning continuity

## Goal
Make the `codex` provider (ChatGPT/Codex subscription over the Responses API on the ChatGPT backend)
correct against the live backend's quirks: a reliable `chatgpt-account-id`, full token capture, and
reasoning continuity across a multi-turn tool loop. Foundation for C-07 (which reuses the codec/headers).

## Acceptance
- [ ] **account-id fallback.** `import_codex` resolves `account_id` from the `id_token` JWT claims (the
      `chatgpt_account_id` claim) when top-level `tokens.account_id` is absent. Failing-first test
      `import_codex_reads_account_id_from_id_token` (fixture `auth.json` with the id only inside the
      `id_token`) asserts the header-bearing account id is populated. (`crates/flux-credentials/src/lib.rs`)
- [ ] **usage tiers.** `map_responses_stream` populates cache + reasoning token fields from
      `response.usage.input_tokens_details.cached_tokens` / `output_tokens_details.reasoning_tokens` (not
      just input/output). Failing-first test `responses_usage_captures_cache_and_reasoning` over a fixture
      `response.completed` event. (`crates/flux-providers/src/openai.rs`)
- [ ] **reasoning continuity.** Under `store:false`, the codex request opts into
      `include:["reasoning.encrypted_content"]` and `build_responses_body` echoes prior encrypted reasoning
      items back into `input` so a multi-turn tool loop keeps reasoning context. Failing-first test
      `codex_body_echoes_encrypted_reasoning` asserts a reasoning item round-trips into the next request.
- [ ] **missing-account-id is a clear error**, not a silent 401: `codex_oauth`/`OpenAiCred` surfaces a
      typed error when `send_account_id` is set but no account id resolved. Test `codex_requires_account_id`.
- [ ] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- (not started)

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
- Touch points: `crates/flux-credentials/src/lib.rs` (`import_codex`, `jwt_expiry_ms` neighbour — add a
  `jwt_claim`/account extractor), `crates/flux-providers/src/openai.rs` (`build_responses_body`,
  `map_responses_stream`, `codex_oauth`/`OpenAiCred`).
- Reuse: the existing `jwt_expiry_ms` base64url-JWT decode (generalize to read a string claim);
  `OpenAiResponses{codex:true}` already toggles `store:false`/effort/summary.
- The usage-tier work overlaps C-05's codec normalization — land the codex side here, the OpenAI-Chat side
  in C-05.
- Verify against a real `~/.codex/auth.json` before closing (do **not** commit token values).

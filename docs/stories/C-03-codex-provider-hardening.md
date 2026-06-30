---
id: C-03
title: Codex provider hardening — account-id, usage tiers, reasoning continuity
pillar: Core
status: done
epic: subscription-providers-and-cost
theme: subscription-providers-cost
design: docs/designs/subscription-providers-and-cost.md
note: "`account_id` from the `id_token` JWT, cache+reasoning token capture, reasoning continuity under `store:false`"
---

# Codex provider hardening — account-id, usage tiers, reasoning continuity

## Goal
Make the `codex` provider (ChatGPT/Codex subscription over the Responses API on the ChatGPT backend)
correct against the live backend's quirks: a reliable `chatgpt-account-id`, full token capture, and
reasoning continuity across a multi-turn tool loop. Foundation for C-07 (which reuses the codec/headers).

## Acceptance
- [x] **account-id fallback.** `import_codex` resolves `account_id` from the `id_token` JWT claims (the
      `chatgpt_account_id` claim, top-level or nested under `https://api.openai.com/auth`) when top-level
      `tokens.account_id` is absent. Test `import_codex_reads_account_id_from_id_token` (hermetic fixture
      `auth.json` under a temp `HOME`, id only inside the `id_token`) asserts the account id is populated.
      (`crates/flux-credentials/src/lib.rs`)
- [ ] **usage tiers.** → **C-05**. `map_responses_stream` populating cache + reasoning token fields was
      reassigned to C-05 (cost-model + codec normalization) to keep the `Usage` struct / usage-token
      parsing in one owner. Not done here.
      **Update (live smoke):** the cache + reasoning token capture is in fact present in committed
      code (`map_responses_stream` lines 897–915, test `responses_usage_captures_cache_and_reasoning`),
      and a live `codex/gpt-5.5` turn reports `cache 5.1k (15% hit)` on the real wire — so the *capture*
      half is done. The pricing/cost roll-up half remains C-05.
- [x] **reasoning continuity.** Under `store:false`, the codex request opts into
      `include:["reasoning.encrypted_content"]` and `build_responses_body` echoes prior encrypted reasoning
      items (`Thinking`/`RedactedThinking` → `{"type":"reasoning","encrypted_content":…}`, pushed before the
      function_call) back into `input` so a multi-turn tool loop keeps reasoning context. Test
      `codex_body_echoes_encrypted_reasoning` asserts the reasoning items + `include` round-trip (and that a
      non-codex body does neither).
- [x] **missing-account-id is a clear error**, not a silent 401: `OpenAiCred::apply` surfaces a typed
      `Error::Auth` when `send_account_id` is set but the token source resolves no account id. Test
      `codex_requires_account_id`.
- [x] **model resolution is provider-internal, not CLI-embedded (live-smoke finding).** A live
      `codex/gpt-5-codex` turn died with HTTP 400 ("`gpt-5-codex` is not supported when using Codex with a
      ChatGPT account") because the ChatGPT-subscription backend serves the `gpt-5.5` family and the
      `*-codex` ids are legacy. The CLI had no codex model-alias resolution (unlike `anthropic`/`claude`)
      and passed the id through verbatim, and the codec tests hardcoded the dead `gpt-5-codex`. Fixed by
      (a) giving `codex` its own provider module `flux_providers::codex` (it was a `providers::openai`
      misfit) that owns `DEFAULT_MODEL = "gpt-5.5"` + `resolve_model` (empty or `*-codex` → `gpt-5.5`, else
      pass-through), and (b) moving `anthropic`/`claude` alias resolution out of the CLI into
      `flux_providers::anthropic::resolve_model` so every surface (CLI/SDK/server/TUI/L3 sub-agent
      spawner) shares one owner — the CLI keeps only the bare-`codex` shorthand policy. Tests
      `resolve_model_*` in both provider modules. Live smoke confirms bare `codex`, legacy
      `codex/gpt-5-codex`, and `codex/gpt-5.5` all resolve to `gpt-5.5` and complete.
- [x] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- **Done (this worktree).** Closed the three in-lane gaps with failing-first tests:
  - `flux-credentials`: generalized the JWT decode into `jwt_payload(token) -> Option<Value>` (refactored
    `jwt_expiry_ms` onto it) + new `account_id_from_id_token`; `import_codex` now reads `tokens.id_token`
    and falls back to its `chatgpt_account_id` claim (top-level or nested) when `tokens.account_id` is
    absent/empty. Test `import_codex_reads_account_id_from_id_token` (hermetic temp `HOME`, unsigned
    fixture JWT — no network, no real files).
  - `flux-providers` (`build_responses_body`): codex bodies set `include:["reasoning.encrypted_content"]`
    and echo assistant `Thinking`/`RedactedThinking` blocks as Responses `reasoning` input items
    (encrypted payload = `signature`/redacted `data`), pushed inline so they precede their `function_call`.
    Test `codex_body_echoes_encrypted_reasoning`.
  - `flux-providers` (`OpenAiCred::apply`): a codex credential with no resolvable account id now errors
    with a typed `Error::Auth` instead of silently omitting the `chatgpt-account-id` header. Test
    `codex_requires_account_id`.
- Gate: `cargo build/test -p flux-credentials -p flux-providers` green (7 + 26 tests), `clippy -D warnings`
  clean, `cargo fmt` applied, `cargo test -p flux-codegate` green.
- **Live smoke (this session).** Ran `flux run -m codex/...` against the real ChatGPT backend
  (`~/.codex/auth.json` present; no token values committed). It surfaced one real bug the unit tests
  couldn't: `codex/gpt-5-codex` died with HTTP 400 ("`gpt-5-codex` is not supported when using Codex
  with a ChatGPT account") — the backend serves the `gpt-5.5` family; `*-codex` ids are legacy. Fixed by
  giving `codex` its own provider module (`flux_providers::codex`) owning `DEFAULT_MODEL` +
  `resolve_model`, and moving `anthropic`/`claude` alias resolution out of the CLI into
  `flux_providers::anthropic::resolve_model` (one owner for every surface). After the fix, bare `codex`,
  legacy `codex/gpt-5-codex`, and `codex/gpt-5.5` all resolve to `gpt-5.5` and complete; the cache-token
  capture shows live (`cache 5.1k (15% hit)`).
- **Not done (out of lane):** the pricing/cost roll-up of cache + reasoning tiers is **C-05** (it touches
  the `Rates`/cost-model, C-05's owner). The *capture* itself (codec → `Usage`) is done and live-verified.

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
- Touch points: `crates/flux-credentials/src/lib.rs` (`import_codex`, `jwt_payload`/`account_id_from_id_token`
  next to `jwt_expiry_ms`); `crates/flux-providers/src/openai.rs` (`build_responses_body`,
  `OpenAiCred::apply`, the shared `OpenAiResponses` codec — `OpenAiCred`/`Secret`/`CODEX_ENDPOINT` are now
  `pub(crate)` so the sibling `codex` module can assemble the credential); `crates/flux-providers/src/codex.rs`
  (new — the `codex` provider module: `oauth`, `DEFAULT_MODEL`, `resolve_model`);
  `crates/flux-providers/src/anthropic.rs` (`resolve_model` — moved out of the CLI);
  `crates/flux-cli/src/main.rs` (now calls the provider-owned resolvers; keeps only the bare-`codex`
  shorthand policy); `crates/flux-core/src/pricing.rs` (its `resolve_alias` mirror is **layer-forced** —
  L0 cannot depend on L1 `flux-providers`; comment updated to point at the canonical home).
- Reuse: the existing `jwt_expiry_ms` base64url-JWT decode (generalized to `jwt_payload`);
  `OpenAiResponses{codex:true}` already toggles `store:false`/effort/summary.
- The usage-tier work overlaps C-05's codec normalization — landed **all** of it in C-05 (this story did
  not touch the `Usage` struct or `map_responses_stream`'s usage emission, per the C-05 boundary).
- Verify against a real `~/.codex/auth.json` before closing the epic (do **not** commit token values).

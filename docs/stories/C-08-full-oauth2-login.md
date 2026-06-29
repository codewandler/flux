---
id: C-08
title: Full OAuth2 login — codex PKCE (+ claude parity)
pillar: Core
status: backlog
priority:
design: docs/designs/subscription-providers-and-cost.md
theme: subscription-providers-cost
---

# Full OAuth2 login — codex PKCE (+ claude parity)

## Goal
The explicit **later stage**: a flux-native interactive OAuth2 login for codex (`flux auth login codex`)
so a user can authenticate flux directly instead of logging into the Codex CLI first. claude already has a
PKCE login; this brings codex to parity and consolidates the flow.

## Acceptance
- [ ] **codex authorize URL.** Add the codex authorize endpoint + redirect constants (currently only
      `CODEX_CLIENT_ID` + `CODEX_TOKEN_URL` exist) and a `codex_authorize_url(pkce, state)`. Failing-first
      test `codex_authorize_url_has_pkce_and_state`.
- [ ] **code exchange.** `codex_exchange_and_store(code, state, verifier)` exchanges the callback code for
      tokens (PKCE) and persists under the `codex` provider, with the same CSRF state-binding as claude.
      Failing-first test `codex_oauth_rejects_state_mismatch_before_any_network`.
- [ ] **`flux auth login codex`** runs the flow (today it bails, pointing at the Codex CLI). Test
      `auth_login_codex_runs_pkce_flow` (behind the existing local-callback harness pattern).
- [ ] **import path still works** — login is additive; `~/.codex/auth.json` import remains the default.
- [ ] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- (not started — explicitly deferred to a later stage per the epic; import + refresh cover the near term.)

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
- Touch points: `crates/flux-credentials/src/lib.rs` (codex constants + `codex_authorize_url` /
  `codex_exchange_and_store`, mirroring `anthropic_authorize_url`/`anthropic_exchange_and_store`),
  `crates/flux-cli/src/main.rs` (`login_codex`, `AuthAction`).
- Reuse: the whole claude PKCE machinery (`generate_pkce`/`generate_state`/CSRF state-binding/
  `parse_token_resp`) is provider-agnostic — codex needs only its own authorize URL + redirect + the
  form-vs-json exchange shape (`CodexRefresher` already uses form-encoding).
- Confirm codex's public OAuth authorize URL / redirect against the upstream codex client before building.

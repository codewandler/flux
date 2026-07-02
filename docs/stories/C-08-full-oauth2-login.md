---
id: C-08
title: Full OAuth2 login — codex PKCE (+ claude parity)
pillar: Core
status: done
priority: 6
epic: subscription-providers-and-cost
theme: subscription-providers-cost
design: docs/designs/subscription-providers-and-cost.md
note: flux auth login codex now runs a real PKCE flow — upstream-verified constants (auth.openai.com/oauth/authorize, localhost:1455/auth/callback, id_token_add_organizations for the account id), CSRF state-binding shared with claude and checked BEFORE any network IO, least-privilege scope (connectors scopes deliberately dropped), account_id from id_token claims like the import path; import stays the default
---

# Full OAuth2 login — codex PKCE (+ claude parity)

## Goal
The explicit **later stage**: a flux-native interactive OAuth2 login for codex (`flux auth login codex`)
so a user can authenticate flux directly instead of logging into the Codex CLI first. claude already has a
PKCE login; this brings codex to parity and consolidates the flow.

## Acceptance
- [x] **codex authorize URL.** Add the codex authorize endpoint + redirect constants (currently only
      `CODEX_CLIENT_ID` + `CODEX_TOKEN_URL` exist) and a `codex_authorize_url(pkce, state)`. Failing-first
      test `codex_authorize_url_has_pkce_and_state`.
- [x] **code exchange.** `codex_exchange_and_store(code, state, verifier)` exchanges the callback code for
      tokens (PKCE) and persists under the `codex` provider, with the same CSRF state-binding as claude.
      Failing-first test `codex_oauth_rejects_state_mismatch_before_any_network`.
- [x] **`flux auth login codex`** runs the flow (today it bails, pointing at the Codex CLI). Test
      `auth_login_codex_runs_pkce_flow` (behind the existing local-callback harness pattern).
- [x] **import path still works** — login is additive; `~/.codex/auth.json` import remains the default.
- [x] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- **Done (2026-07-02).** Constants beside the existing CODEX_CLIENT_ID/CODEX_TOKEN_URL, provenance
  in code comments (verified against upstream openai/codex `codex-rs/login/src/server.rs`):
  `CODEX_AUTHORIZE_URL = https://auth.openai.com/oauth/authorize`, redirect
  `http://localhost:1455/auth/callback`, scope `openid profile email offline_access`
  (upstream's connectors scopes deliberately dropped — least privilege, documented at the const;
  widening is a one-line change if the backend ever rejects the narrower grant). The authorize URL
  carries upstream's `id_token_add_organizations=true` + `codex_cli_simplified_flow=true`
  (the former puts the ChatGPT account id into the id_token claims); the telemetry `originator`
  param is omitted.
  - `codex_exchange_and_store(code, state, verifier)`: form-encoded PKCE grant against the token
    URL, `id_token` claims → `account_id` exactly like the import path, persisted via the same
    `save_stored`; `_at(token_url, …)` is the documented test seam. CSRF binding factored into a
    shared `bind_callback_state` (claude's exchange refactored onto it, behavior unchanged) and
    checked BEFORE any network IO.
  - `flux auth login codex` binds 127.0.0.1:1455, serves a confirmation page, 404s non-callback
    paths, surfaces provider `error=` params, and forwards code#state into the same binding shape
    claude uses. Import stays the default (`load_stored → import_codex` order unchanged);
    `codex_token_source`'s no-credential error now mentions the login command.
  - Tests (failing-first): `codex_authorize_url_has_pkce_and_state`,
    `codex_oauth_rejects_state_mismatch_before_any_network`,
    `codex_exchange_persists_under_codex_with_account_id` (loopback stub + HOME_LOCK),
    `auth_login_codex_runs_pkce_flow` (hermetic: stub token endpoint + injected callback),
    `parse_codex_callback_extracts_code_and_state`. Full workspace gate green (the combined
    clippy leg ran after the concurrent flux-lang agent landed).
- **Residuals:** no live end-to-end run against the real auth.openai.com (needs a human with a
  ChatGPT account — suggest a manual `flux auth login codex` smoke); `wait_for_codex_callback`
  blocks without a timeout (matches the claude flow's blocking read).

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
- Touch points: `crates/flux-credentials/src/lib.rs` (codex constants + `codex_authorize_url` /
  `codex_exchange_and_store`, mirroring `anthropic_authorize_url`/`anthropic_exchange_and_store`),
  `crates/flux-cli/src/main.rs` (`login_codex`, `AuthAction`).
- Reuse: the whole claude PKCE machinery (`generate_pkce`/`generate_state`/CSRF state-binding/
  `parse_token_resp`) is provider-agnostic — codex needs only its own authorize URL + redirect + the
  form-vs-json exchange shape (`CodexRefresher` already uses form-encoding).
- Confirm codex's public OAuth authorize URL / redirect against the upstream codex client before building.

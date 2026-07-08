---
id: D-81
title: Host-side OAuth token exchange + resolve-and-refresh for plugins
pillar: Core
status: done
design: docs/designs/plugin-oauth.md
epic: plugin-oauth
note: "the host performs every /oauth/token grant + auto-refresh; resolve an OAuth2 purpose from the credential store → the plugin only ever gets a fresh bearer. Unblocks a downstream consumer's OAuth-wrapping plugin."
---

# Host-side OAuth token exchange + resolve-and-refresh for plugins

## Goal
Make the host do all OAuth token work so a plugin stays a pure bearer consumer — it keeps calling
`host.secret(purpose)` and always gets a fresh access token, never touching `/oauth/token`.

## Acceptance
- [ ] `SystemHostCaps.resolve_purpose` (`crates/flux-plugin/src/lib.rs:1205`) resolves an OAuth2 purpose
      from the credential store, returning the stored access token and **auto-refreshing** it via the
      declared `token_path` when stale (generalize `RefreshingToken`, `flux-credentials/src/lib.rs:450`).
- [ ] The host performs every `/oauth/token` grant host-side: `password`, `refresh_token`,
      `authorization_code`, `client_credentials`. A plugin needs **no** `http` grant for OAuth.
- [ ] Failing-first test: a plugin op requesting an OAuth2 purpose gets a bearer from a mock token
      endpoint; an expired token triggers a refresh; env fallback still works when there's no store entry.

## Progress
- 2026-07-08 **DONE.** flux-credentials gained generic host-side OAuth: `oauth_token_grant` (a
  form-encoded RFC-6749 grant → `OAuthToken`, covering refresh/authorization_code/password/
  client_credentials), `resolve_stored_bearer` (load → refresh-if-stale via the token endpoint →
  persist → bearer), and `save_token`/`load_token`. flux-plugin's `resolve_purpose` (made **async**,
  rippling through `resolve_auth` + the `secret`/`http.do` handlers) resolves an OAuth2 purpose from
  the credential store keyed `plugin:<name>:<purpose>`, builds the token URL from the method's
  **declared** endpoint (SSRF- + host-allow-list-guarded, never plugin-supplied), registers the bearer
  with the redactor, and falls back to the declared env secret when no token is stored. flux-plugin
  gained a flux-credentials dep (L4→L1, legal). Tests: `resolve_stored_bearer_...` (flux-credentials,
  stored/refresh/absent) + `oauth2_purpose_resolves_stored_bearer_else_env_fallback` (flux-plugin).

## Notes
- Design: [plugin-oauth.md](../designs/plugin-oauth.md). **This unblocks** a downstream consumer's
  OAuth-wrapping plugin (declare-only OAuth config).

---
id: D-82
title: Generalize `flux auth login` to plugins (PKCE browser-callback + password)
pillar: Core
status: done
design: docs/designs/plugin-oauth.md
epic: plugin-oauth
note: "extend `flux auth login` beyond claude|codex to any OAuth2 plugin — PKCE loopback-callback (reuse wait_for_codex_callback) + --password; write the plugin token store."
---

# Generalize `flux auth login` to plugins (PKCE browser-callback + password)

## Goal
Give a CLI user a real login for an OAuth2 plugin: `flux auth login <plugin>` runs the browser-callback
PKCE flow (or password grant) and stores the tokens — no pasted bearer.

## Acceptance
- [ ] `AuthAction::Login` (`crates/flux-cli/src/main.rs:7296`) accepts a plugin name (not just
      `claude|codex`) and drives PKCE from the plugin's declared config: `generate_pkce`/`generate_state`,
      an authorize URL built from the manifest (not the per-provider `codex_authorize_url`), a
      generalized `wait_for_codex_callback` (`main.rs:7362`) on the declared port/path, code exchange,
      token store.
- [ ] A `--password` variant prompts for credentials and runs the password grant into the store.
- [ ] After `flux auth login <plugin>`, `flux plugin call <plugin> <read-op>` succeeds with no env token.
- [ ] `flux plugin login <plugin>` alias.

## Progress
- 2026-07-08 **DONE.** `flux auth login <plugin>` (and the `flux plugin login <name>` alias) now
  accepts any installed plugin: `run_auth` falls through past `claude`/`codex` to `login_plugin`,
  which loads the plugin's manifest, finds its OAuth2 method, resolves the declared endpoint, and
  drives either the browser PKCE `authorization_code` flow — a generic `oauth_authorize_url`
  (flux-credentials) + a generalized loopback callback listener (`wait_for_oauth_callback`, on the
  manifest's redirect port/path, with a 5-min timeout) + code exchange — or the `--password` grant
  (an `rpassword` hidden prompt), storing tokens under `plugin:<name>:<purpose>` so a later
  `flux plugin call` resolves them (D-81) with no env token. The exchange core
  (`plugin_oauth_code_grant`) mirrors `codex_login_flow`'s closure-injection shape for hermetic
  testing. Test: `plugin_oauth_code_grant_builds_pkce_url_and_exchanges` (flux-cli).

## Notes
- Design: [plugin-oauth.md](../designs/plugin-oauth.md). Reuses the provider-only machinery in
  `crates/flux-credentials` + `crates/flux-cli`. Optional: a browser-open (providers print the URL today).

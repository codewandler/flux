---
id: D-82
title: Generalize `flux auth login` to plugins (PKCE browser-callback + password)
pillar: Core
status: backlog
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
- Proposed. Depends on D-80 (manifest) + D-81 (store + resolve/refresh).

## Notes
- Design: [plugin-oauth.md](../designs/plugin-oauth.md). Reuses the provider-only machinery in
  `crates/flux-credentials` + `crates/flux-cli`. Optional: a browser-open (providers print the URL today).

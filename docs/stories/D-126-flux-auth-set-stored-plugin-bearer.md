---
id: D-126
title: "`flux auth set` — stored bearer tokens for plugin auth purposes (no-env configuration)"
pillar: Core
status: done
note: "plain (non-OAuth2) plugin auth purposes resolve ONLY from process env today; operators need a configure-in-advance path when the session env can't carry secrets"
---

# `flux auth set` — stored bearer tokens for plugin auth purposes (no-env configuration)

## Goal
An operator can configure a plain bearer credential for a plugin auth purpose **in advance** —
`flux auth set slack bot_token` — stored in the existing `~/.flux/credentials.toml` (0600) under the
same `plugin:<name>:<purpose>` key the plugin-OAuth flow (D-82) uses, so a later `flux` session
resolves it **without the secret being present in the process environment**. Today `resolve_purpose`
consults the credential store only for OAuth2-backed methods; plain methods are env-or-nothing.

## Acceptance
- [ ] Failing-first test: a plain (non-OAuth2) auth method resolves from a stored token when the
      declared env keys are unset; a stored token wins over env (matching the OAuth store-first
      precedent); the resolved value is registered with the secret sink (redaction).
- [ ] `flux auth set <plugin> [<purpose>]` prompts for the token (hidden; reads a line from stdin
      when not a tty, so it scripts), validates the plugin + purpose exist in the installed
      manifest, defaults `<purpose>` when the plugin declares exactly one auth method, and stores
      the value — never echoing it. `--clear` removes a stored token.
- [ ] `flux plugin status <name>` shows a stored plain bearer as the active resolution
      (`✓ <purpose> — stored token (flux auth set …)`), stored-over-env precedence visible.
- [ ] Live e2e: with `SLACK_BOT_TOKEN`/`SLACK_USER_TOKEN` **removed from the environment**,
      `flux plugin call slack slack.test` succeeds via stored tokens.

## Progress
- 2026-07-10 filed from the "configure slack auth without env access" request; pairs with D-125
  (which unblocks the live slack proof).
- 2026-07-10 **DONE.** Failing-first test `plain_purpose_resolves_stored_bearer_over_env`
  (injected `CredentialStore` + `SecretSink` spy; asserts store-over-env precedence, env fallback,
  and sink registration). Shipped: `resolve_purpose` store-first for plain methods;
  `flux_credentials::delete_token` (+ `write_store` atomic-write refactor shared with
  `save_stored`); `flux auth set <plugin> [<purpose>] [--clear]` (manifest-validated, hidden
  prompt / piped-stdin line, purpose defaulting when exactly one method); `describe_auth_resolution`
  shows stored plain bearers and points unconfigured purposes at `auth set` vs `auth login`; the
  purpose-resolution error now names both configuration paths. Live e2e: env removed →
  `slack.test` fails with the new pointer; `printf '%s\n' "$TOK" | flux auth set slack bot_token`
  (+ user_token) → `slack.test`/`slack.info`/`slack.channel.list` all `ok` with **no SLACK_* env**;
  `--clear` verified and the token re-set. CHANGELOG + WHATS-NEW + usage.md updated (additive →
  patch bump next cut). Not committed (awaiting instruction).

## Notes
- Store: `flux-credentials` `save_token`/`load_token` (`OAuthToken { access, refresh: None,
  expires_at_ms: None }` is a valid plain-bearer shape; `resolve_stored_bearer` refresh path is
  skipped when `refresh` is `None`).
- Resolution seam: `SystemHostCaps::resolve_purpose` (`crates/flux-plugin/src/lib.rs:925`).
- Status display: `describe_auth_resolution` (`crates/flux-cli/src/main.rs:8012`) currently checks
  the store only when `oauth2.is_some()`.
- Deliberately NOT a general secret manager: one bearer string per `(plugin, purpose)`, same file
  and key shape as plugin OAuth.

# Design — Plugin OAuth (generalize the provider-only auth flow to plugins)

**Status:** Proposed — epic shell · **Pillar:** Core · **Epic:** `plugin-oauth` · **Stories:** D-80..D-83

## Why

flux plugins that wrap an OAuth2 API (many third-party integrations) have no way to log a user
in. Plugin auth today is **static env→secret** resolution (`SystemHostCaps.resolve_purpose`,
`crates/flux-plugin/src/lib.rs:1205`): a plugin declares an `AuthMethod` and the host hands back an env
value or injects it into `http.do`. A plugin *could* POST `/oauth/token` itself via `http.do`, but it
**cannot** run a PKCE browser-callback (no browser, no inbound listener) and **cannot** persist a token
cache (no `fs.write`). More importantly, **it shouldn't** — reimplementing OAuth per plugin is exactly
the boilerplate flux exists to remove.

flux already implements the *entire* flow — **but only for its own LLM providers.** `flux auth login
{claude|codex}` runs PKCE with a loopback callback on `127.0.0.1:1455`; `~/.flux/credentials.toml`
(0600) + `RefreshingToken` is a token store with in-place refresh (`crates/flux-credentials`,
`crates/flux-cli`). This epic **generalizes that provider-only machinery to plugins**, so a plugin only
*declares* its OAuth endpoints and stays a pure bearer consumer.

## Principle

**A plugin performs no OAuth token exchange — not password, not refresh, not `authorization_code`, not
even via `http.do`.** Every `/oauth/token` call, the PKCE flow, the callback listener, and the token
store/refresh are host functionality. The plugin keeps calling `host.secret(purpose)` and always gets a
fresh bearer.

## What changes (stories)

- **D-80 — OAuth2 auth-method in the manifest.** Extend `AuthMethod`/`PluginCapabilities`
  (`crates/flux-plugin/src/lib.rs`) with an OAuth2 block for a purpose: `authorize_path` + `token_path`
  (resolved against a declared `EndpointSpec`), `client_id`, `scopes`, supported `grants`, and a
  loopback `redirect` (port + path). Backward-compatible: a method with no OAuth2 block behaves exactly
  as today.
- **D-81 — Host token exchange + resolve-and-refresh.** `SystemHostCaps.resolve_purpose` performs
  **every** `/oauth/token` grant host-side (`password`, `refresh_token`, `authorization_code`,
  `client_credentials`) and resolves an OAuth2 purpose from the credential store, auto-refreshing a
  stale access token (generalize `RefreshingToken`, `crates/flux-credentials/src/lib.rs:450`). The
  plugin gets only a fresh bearer; no `http` grant for OAuth is needed. **Unblocks the consumer's
  declaration story.**
- **D-82 — `flux auth login <plugin>` (+ `flux plugin login` alias).** Generalize `AuthAction::Login`
  beyond `claude|codex` (`crates/flux-cli/src/main.rs:7296`): drive PKCE from the plugin's declared
  config — reuse `generate_pkce`/`generate_state`, build the authorize URL from the manifest (not the
  per-provider `codex_authorize_url`), generalize `wait_for_codex_callback` (`main.rs:7362`) to the
  declared port/path, exchange the code, store the tokens. Add a `--password` variant (prompt →
  password grant → store). Optional browser-open (providers print the URL today — acceptable).
- **D-83 — Backend-abstracted credential store (file + Vault).** A `CredentialStore` trait; the file
  backend (`~/.flux/credentials.toml`, 0600) is the dev/CLI default; a **Vault backend** for
  deployment; the backend is **host-injectable** (a host app supplies a Vault-backed store the same way
  it supplies custom `HostCapabilities`). Generalize `save_stored`/`store_path`/the `TokenSource` keying
  (`crates/flux-credentials/src/lib.rs`) to `plugin+purpose[+account]`. Also unblocks the UI-configured
  Integrations pillar (per-customer OAuth tokens → Vault, never a file on a pod).

## Relates to

- The provider machinery this generalizes: `crates/flux-credentials`, `crates/flux-cli`
  (`AuthAction::Login`, `wait_for_codex_callback`).
- The plugin auth vocabulary it extends: `crates/flux-plugin/src/lib.rs` (`AuthMethod`, `AuthScheme`,
  `EndpointSpec`, `SystemHostCaps`), and `docs/designs/request-auth-seam.md`.
- First consumer: a downstream consumer's OAuth-wrapping plugin + its platform Integrations layer
  (the Vault store). Consumer-specific specifics live in that consumer's own adoption stories.

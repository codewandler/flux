---
id: D-80
title: OAuth2 auth-method in the plugin manifest
pillar: Core
status: done
design: docs/designs/plugin-oauth.md
epic: plugin-oauth
note: "extend AuthMethod/PluginCapabilities with an OAuth2 block (authorize/token paths, client_id, scopes, grants, loopback redirect); backward-compatible. First step of plugin-oauth."
---

# OAuth2 auth-method in the plugin manifest

## Goal
Let a plugin **declare** that a purpose is OAuth2-backed, so the host can drive login/refresh for it —
the vocabulary the rest of `plugin-oauth` builds on.

## Acceptance
- [ ] `AuthMethod` (or a sibling on `PluginCapabilities`, `crates/flux-plugin/src/lib.rs`) gains an
      OAuth2 block for a purpose: `authorize_path` + `token_path` (resolved against a declared
      `EndpointSpec`), `client_id`, `scopes`, supported `grants` (authorization_code+PKCE / password /
      refresh_token / client_credentials), and a loopback `redirect` (port + path).
- [ ] Backward-compatible: a method with no OAuth2 block resolves exactly as today (env→secret). A
      manifest round-trip test covers both shapes.
- [ ] host-kit exposes the declaration (a plugin author sets it via `PluginBuilder`).

## Progress
- 2026-07-08 **DONE.** Added the OAuth2 vocabulary to the plugin manifest (`crates/flux-plugin`):
  `OAuth2Spec` (endpoint / authorize_path / token_path / client_id / scopes / grants / loopback
  redirect) + `OAuthGrant` + `OAuthRedirect`, an optional `AuthMethod.oauth2` field
  (`skip_serializing_if`, all-default → legacy manifests round-trip unchanged), an
  `AuthMethod::oauth2()` constructor, and host-kit re-exports so an author declares it via
  `.auth(AuthMethod::oauth2(...))`. The resulting access token injects as `Bearer`. Test:
  `manifest_oauth2_and_legacy_auth_round_trip` (backward-compat + OAuth2 round-trip).

## Notes
- Design: [plugin-oauth.md](../designs/plugin-oauth.md). First consumer: a downstream consumer's
  OAuth-wrapping plugin.

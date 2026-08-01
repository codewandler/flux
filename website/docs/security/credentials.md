---
title: Credentials & secrets
description: How flux stores provider and plugin tokens, keeps secret values invisible to the model, and configures plugin auth with flux auth login / flux auth set.
---

# Credentials & secrets

This page covers outbound authentication: provider keys, subscription credentials, plugin OAuth
tokens, secret references, and redaction. It explains where credentials live and how flux keeps secret
values out of model-visible context and logs.

For provider selection, see [Providers and models](../agent/providers.md). For dispatch gating, see
[Safety & approvals](../agent/safety.md).

## References are locations, not values

The core idea is that the parts of flux that plan and reason never handle a secret value — they
handle a **reference** to one. A reference is an address:

- `env/ANTHROPIC_API_KEY` — an environment variable,
- `plugin/<plugin>/<instance>/<slot>` — a plugin-scoped credential,
- `kubernetes/<ns>/<name>/<key>` — a cluster secret.

Model-visible calls carry these **names**. The host resolves a reference only at an IO boundary. On a
host-mediated HTTP path it injects the value without returning it to the plugin. An explicitly
declared `secret` or `credential` callback can instead materialize the value into trusted native
plugin code, while `conn.authenticate` keeps it host-side for a supported database handshake. None
of those paths returns the value to the model. Resolved credentials redact themselves in debug
output, and the resolved-endpoint form used for host-mediated requests has no serialization at all.

> Model-visible calls carry secret *names*. They never receive a secret *value*.

## The redactor

Resolution is a last line of defence, not the only one. Every value the host materializes at runtime
is registered with a **redactor** that scrubs it from *all* model-visible tool output and logs — on
both success and error. The redactor works two ways:

- **Registered values** — the exact secret string is replaced with `[redacted]` wherever it appears.
- **Credential shapes** — even a value that was never registered is caught if it looks like a
  credential: prefixes such as `sk-ant-`, `sk-`, `xoxb-`, `ghp_`, `github_pat_`, `AKIA`, `AIza`,
  `ya29.`, and JWT-shaped `eyJ…` tokens. Matching is punctuation-aware, so glued forms like
  `api_key=sk-ant-…` are caught too.

This is what "[secrets stay invisible to the model](../agent/safety.md)" means concretely.

## Where credentials are stored

flux stores tokens for both **providers** and **plugins** in a single file:

```text
~/.flux/credentials.toml
```

Entries are keyed by purpose — `claude` and `codex` for provider subscriptions, and
`plugin:<name>:<purpose>` for a plugin, where the purpose is the one the plugin's own manifest
declares (for example `plugin:gitlab:personal_token` or `plugin:slack:bot_token` —
`flux plugin status <name>` prints the purposes it accepts). Each entry holds the access token and, for OAuth entries, the refresh
token, an expiry, and an optional account id.

:::note
The default store is **plaintext, protected by file permissions — not encrypted at rest.** The file
is written `0600` (owner-only) by creating a temp file at that mode and atomically renaming it over
the target, so there is never a world-readable window and a crash can't truncate your other tokens.
Filesystem permissions are the whole at-rest protection under the default backend. An embedding
host that needs centralized storage can inject the Vault backend described below; the stock CLI and
server do not select it automatically.
:::

## Storage backends

Credential storage is a pluggable library boundary (`CredentialStore`), so an embedding application
can move tokens off local disk:

| Backend | Where tokens live | When to use |
|---|---|---|
| File (default) | `~/.flux/credentials.toml`, `0600` | Local, single-user development |
| Vault (embedder-provided) | HashiCorp Vault KV-v2 at `<addr>/v1/<mount>/data/<prefix>/<key>` | Host applications where per-customer tokens must not sit in a file on a pod |

`VaultCredentialStore::from_env()` reads standard Vault environment (`VAULT_ADDR`, `VAULT_TOKEN`)
plus optional `FLUX_VAULT_MOUNT` (default `secret`) and `FLUX_VAULT_PREFIX` (default `flux`). It
authenticates with an `X-Vault-Token` header and maps `:` separators in a key to Vault path segments.
An embedder must construct the store and inject it into its plugin host with
`SystemHostCaps::with_credential_store`; `from_env()` is a constructor helper, not automatic CLI or
server configuration. The stock `flux auth` commands and server assembly continue to use the file
store even when those variables are set. Pair an injected store with
[principal-mode server auth](./server-auth.md) for a multi-tenant deployment.

## Logging in a provider

```bash
flux auth status                 # what is configured, and from where
flux auth login claude           # Claude subscription (browser OAuth)
flux auth login codex            # ChatGPT/Codex subscription (browser OAuth)
```

Credential precedence for providers: an **environment variable** wins (`ANTHROPIC_API_KEY`,
`OPENROUTER_API_KEY`, …), then a **stored credential** from `flux auth login`, then an **imported CLI
credential** (from `~/.claude` / `~/.codex`). See the provider table in
[Providers and models](../agent/providers.md).

## Logging in a plugin (OAuth)

A plugin whose manifest declares an **OAuth2** auth method is logged in the same way:

```bash
flux auth login <plugin>             # browser + loopback-callback PKCE (default)
flux auth login <plugin> --password  # resource-owner password grant instead
flux plugin login <plugin>           # equivalent alias
```

:::note Token-only manifests use `auth set`
Against a plugin that declares no OAuth2 auth method, the command refuses with
`plugin <name> declares no OAuth2 auth method`. Use `flux auth set <plugin> <purpose>` instead.
:::

The security-relevant part is **who runs the OAuth flow**: flux does, not the plugin. The plugin only
*declares* an `oauth2` block in its manifest (a token endpoint, scopes, and which grants it supports).
The host:

1. builds the authorize/token URLs from the plugin's **declared** endpoint — the token host is
   host-controlled and egress-gated, never a URL the plugin hands over;
2. runs the PKCE (S256) exchange, verifying the CSRF state binding **before any network call**;
3. stores the resulting token under `plugin:<name>:<purpose>` and **refreshes it automatically** near
   expiry.

The plugin never handles the `/oauth/token` exchange itself. At call time, a host-mediated
`http.do` request can inject the fresh bearer without returning it to the plugin. A trusted plugin
that explicitly asks for its declared auth purpose through `secret` does receive the raw bearer;
that distinction is documented under [Plugin capability sandbox](./plugin-sandbox.md).

## Storing a plain plugin token (`flux auth set`)

Most plugins authenticate with a plain bearer token (a Slack `xoxb-…`, a GitLab `glpat-…`) rather
than an OAuth flow. Those purposes resolve from the env vars the manifest declares — or from a
token you store **once, in advance**:

```bash
flux auth set slack bot_token        # hidden prompt; never echoed
pass show slack/bot | flux auth set slack bot_token   # or pipe it from a secret manager
flux auth set slack bot_token --clear                 # remove it again
```

`auth set` validates the plugin and purpose against the installed manifest (the purpose argument is
optional when the plugin declares exactly one), then writes the token to the same
`~/.flux/credentials.toml` store under `plugin:<name>:<purpose>`. Later sessions resolve it with no
env var in the picture — useful when the shell that launches flux can't (or shouldn't) carry
secrets in its environment.

Resolution order for a plugin purpose: a **stored token wins**, the declared **env keys are the
fallback** — the same order OAuth-backed purposes use. `flux plugin status <name>` always shows
which source is active, without printing a value. Like every host-resolved secret, a stored token is
registered with the redactor. It may reach the wire as host-injected HTTP auth, be materialized into
trusted plugin code through a declared `secret`/`credential` callback, or remain host-side for
`conn.authenticate`; it never becomes model-visible context.

## Related docs

- [Providers and models](../agent/providers.md) — credential sources by provider.
- [Plugin capability sandbox](./plugin-sandbox.md) — plugin secret references and OAuth declarations.
- [Plugin trust & signing](./plugin-trust.md) — installed plugin integrity.

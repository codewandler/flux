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

The model plans against these **names**. Only the host, at the moment of an actual IO call, resolves
a reference into the real value, uses it, and discards it. A resolved credential is modelled so it
can't leak by accident: it redacts itself in debug output, and the resolved-endpoint form the host
builds to make a request has **no serialization at all** — there is no code path that could turn it
back into text the model could see.

> The model plans against secret *names*. It never receives a secret *value*.

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
`plugin:<name>:<purpose>` for a plugin (for example `plugin:gitlab:api_token` or
`plugin:slack:bot_token`). Each entry holds the access token and, for OAuth entries, the refresh
token, an expiry, and an optional account id.

:::note
The default store is **plaintext, protected by file permissions — not encrypted at rest.** The file
is written `0600` (owner-only) by creating a temp file at that mode and atomically renaming it over
the target, so there is never a world-readable window and a crash can't truncate your other tokens.
Filesystem permissions are the whole at-rest protection under the default backend. If you need
encryption or centralized storage, use the Vault backend below.
:::

## Storage backends

Credential storage is a pluggable backend (`CredentialStore`), so a deployment can move tokens off
local disk without changing anything else:

| Backend | Where tokens live | When to use |
|---|---|---|
| File (default) | `~/.flux/credentials.toml`, `0600` | Local, single-user development |
| Vault | HashiCorp Vault KV-v2 at `<addr>/v1/<mount>/data/<prefix>/<key>` | Multi-tenant / server deployments where per-customer tokens must not sit in a file on a pod |

The Vault backend reads standard Vault environment (`VAULT_ADDR`, `VAULT_TOKEN`) plus optional
`FLUX_VAULT_MOUNT` (default `secret`) and `FLUX_VAULT_PREFIX` (default `flux`); it authenticates with
an `X-Vault-Token` header and maps the `:` separators in a key to Vault path segments. Pair it with
[Principal-mode server auth](./server-auth.md) for the hardened multi-tenant profile.

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

Plugins that talk to an OAuth2-protected API can be logged in the same way:

```bash
flux auth login gitlab           # browser + loopback-callback PKCE (default)
flux auth login gitlab --password  # resource-owner password grant instead
flux plugin login gitlab         # equivalent alias
```

The security-relevant part is **who runs the OAuth flow**: flux does, not the plugin. The plugin only
*declares* an `oauth2` block in its manifest (a token endpoint, scopes, and which grants it supports).
The host:

1. builds the authorize/token URLs from the plugin's **declared** endpoint — the token host is
   host-controlled and egress-gated, never a URL the plugin hands over;
2. runs the PKCE (S256) exchange, verifying the CSRF state binding **before any network call**;
3. stores the resulting token under `plugin:<name>:<purpose>` and **refreshes it automatically** near
   expiry.

The plugin never touches the `/oauth/token` endpoint and never sees the resulting token — at call
time the host injects a fresh bearer on the plugin's behalf. The declaration side of this
(`oauth2` manifest fields) lives on the [Plugin capability sandbox](./plugin-sandbox.md) page.

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
which source is active, without printing a value. Like every host-resolved secret, a stored token
is registered with the redactor and reaches the wire only as a host-injected header.

## Related docs

- [Providers and models](../agent/providers.md) — credential sources by provider.
- [Plugin capability sandbox](./plugin-sandbox.md) — plugin secret references and OAuth declarations.
- [Plugin trust & signing](./plugin-trust.md) — installed plugin integrity.

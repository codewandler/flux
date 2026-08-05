---
title: Credentials & secrets
description: Which Flux credential paths prevent disclosure, which contain materialized values with redaction, and where the limits are.
---

# Credentials & secrets

This page covers outbound authentication: provider keys, subscription credentials, plugin OAuth
tokens, secret references, and redaction. Flux has two secret-handling models. Some paths prevent a
value from entering a process or component at all; other paths materialize plaintext and rely on
containment. The distinction matters because only the first makes a missed scrub harmless.

For provider selection, see [Providers and models](../agent/providers.md). For dispatch gating, see
[Safety & approvals](../agent/safety.md).

## Prevention and containment

**Prevention** means the protected component never receives the value. A missed boundary fails as an
authentication error or a refused response, rather than publishing the credential. **Containment**
means a value is plaintext in the Flux process and controls such as narrow interfaces, scopes, and
redaction keep it from travelling farther. A missed containment control can leak, so Flux does not
describe the two as equivalent.

Use this table to classify your own secret:

| Your secret and path | Model | What actually holds the value |
|---|---|---|
| A vendor credential behind an Exchange/platform-sourced connector operation | **Prevention at the Flux-process boundary.** The connector executor applies the vendor credential; Flux sends an operation plus arguments. A response carrying a recognized credential shape is refused at ingest, not merely redacted. | Exchange/the external executor. Flux still holds its own authenticated session or Service Account bearer for that service. |
| A PostgreSQL password used through `conn.authenticate` | **Prevention at the plugin boundary.** The host completes the PostgreSQL startup and SCRAM/MD5/cleartext handshake and hands the plugin a post-authenticated connection. | The Flux host materializes the password; the plugin and model do not. Other raw-socket protocols are not covered unless they have their own host terminator. |
| An endpoint represented as `EndpointRef` and used by a host-mediated request | **Prevention at the model/serialization boundary.** The reference contains a credential location, never a value. The host-only `ResolvedEndpoint` type has no serialization implementation and its debug view omits header values. | The Flux host may materialize injected headers at IO. An explicit, granted `secret` or `credential` callback can instead hand a value to trusted native plugin code. |
| A `$secret` marker in `http.request` | **Containment in the Flux process, late resolution at guarded HTTP send.** The model names an allowlisted environment key; the host resolves and registers it. Optional destination, principal, and header/query scopes are enforced before the value is read. | The Flux host and outgoing request. The value is not returned as tool output. |
| `secret "NAME"` in a Flux program's agent, channel, or datasource settings | **Containment.** At program load the environment value replaces the marker as plaintext and is registered with the redactor. It remains in the resolved settings for that process. | The Flux process and the trusted adapter consuming those settings. This path has no destination scope and does not re-resolve after rotation. Moving applicable local secrets to egress substitution is tracked in [C-458](https://github.com/codewandler/flux/blob/main/docs/stories/C-458-substitute-at-egress.md). |
| A provider key, stored plugin token, or OAuth token resolved locally | **Containment, with a narrower recipient where available.** Host-mediated HTTP and `conn.authenticate` avoid handing it to plugin code; an explicitly granted native-plugin secret callback does hand it to trusted code. | The Flux host; possibly a trusted plugin when its manifest and operator grant permit materialization. |
| A credential pasted into a prompt or read from a file/path Flux did not resolve as a secret | **Neither.** Flux does not know that arbitrary text is a secret. | The prompt/file consumer, model context where used, and potentially the durable log. Do not paste credentials into prompts; a prompt-path control is tracked in [C-432](https://github.com/codewandler/flux/blob/main/docs/stories/C-432-browser-credentials-never-come-from-the-prompt.md). |

The process-safe reference forms are locations, not values. Examples include:

- `env/ANTHROPIC_API_KEY` — an environment variable,
- `plugin/<plugin>/<instance>/<slot>` — a plugin-scoped credential,
- `kubernetes/<ns>/<name>/<key>` — a cluster secret.

When a model-visible operation uses one of these forms, its argument carries the location. That does
not turn every string in a prompt or settings bag into a protected reference; only the paths above
resolve and register values.

## The redactor

The `Redactor` redacts exact values it has been told about. A credential pasted into a prompt, or read
from a file Flux did not resolve as a secret, is not registered and is not guaranteed to be redacted.
This is the most important limit to understand.

On the paths wired to it, the redactor scrubs registered values from model-visible tool output,
observations, and many log/export fields on both success and error. It also recognizes several common
credential shapes:

- **Registered values** — the exact secret string is replaced with `[redacted]` wherever it appears.
- **Credential shapes** — an unregistered value may still be caught if it looks like a
  credential: prefixes such as `sk-ant-`, `sk-`, `xoxb-`, `ghp_`, `github_pat_`, `AKIA`, `AIza`,
  `ya29.`, JWT-shaped `eyJ…` tokens, PEM private-key blocks, URL authority passwords, and sufficiently
  long opaque assignment values whose names look secret-bearing. Shape matching is deliberately
  incomplete to avoid censoring ordinary text; a short, all-digit, encoded, or unfamiliar credential
  can pass it.

Redaction is a backstop, not proof that a value never existed. Flux has shipped containment bugs,
including a redaction failure that once fell back to the original value; that failure path now fails
closed, but prevention remains the stronger property.

## The durable-log exception

Raw prompt text and assistant answers reach the durable event log with **no redactor in their write
path**. This applies both to conversation `Message` entries and to the `TurnStarted.user_input` /
`TurnEnded.answer` fields used to build turn summaries. The event store is append-only, so later
redaction cannot undo that write.

`flux export` applies a fresh redactor before rendering those fields, but a read-only export does not
know which arbitrary values were registered during the historical run. It can catch only credential
shapes. Therefore:

- never paste a credential into a prompt;
- do not assume an answer echoing an unknown credential is safe in the durable log;
- use a supported secret reference/resolution path before the turn starts.

Other recorded fields such as run traces, observations, and plan source are redacted at record time
and scrubbed again during export. That stronger handling does not erase the prompt/answer exception.

## Naming, destination scope, rotation, and audit

These controls cover different questions:

- **Which secret may be named?** `http.request` accepts only entries in `[web] allowed_secrets` (or
  its environment fallback), and plugin `grants.secrets` lists keys individually. Both checks happen
  before reading the value.
- **Where and by whom may it be sent?** An `http.request` allowlist entry can declare `to=`, `by=`,
  and `in=header|query`; declared axes are default-deny and the destination is matched after DNS
  resolution against the address the request is pinned to. A bare entry remains explicitly
  unscoped. Program-level `secret "NAME"` values and channel-adapter settings do not get this
  destination/principal scope merely because they are registered with the redactor. [C-459](https://github.com/codewandler/flux/blob/main/docs/stories/C-459-scope-a-secret.md)
  records that coverage boundary.
- **Does rotation reach a running process?** OAuth sources refresh near expiry, but a local program
  secret resolved from an environment variable is read once at load. Changing or revoking it requires
  a restart; the old plaintext remains in the live resolved settings and redactor store.
- **Can the audit answer where it was used?** Cross-plugin credential resolution records consumer,
  provider, and reference location without recording the value. That is the only end-to-end secret-use
  audit hop today. Program-secret resolution, `$secret` HTTP injection, a plugin's direct secret read,
  and ordinary environment seeding do not produce a complete use record.

Rotation/revocation and complete use auditing remain tracked in
[C-460](https://github.com/codewandler/flux/blob/main/docs/stories/C-460-rotation-revocation-audit.md).

The public `Sensitivity` classification type is not currently read by enforcement and must not be
treated as a scope or policy control.

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

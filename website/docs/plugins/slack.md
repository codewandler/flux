---
title: Slack plugin
description: "Step-by-step setup for the slack plugin: install, provide bot/user tokens (env or stored), verify, and call an operation."
---

# Slack plugin

A worked setup for the `slack` plugin — messaging, threads, search, reactions, channels, users,
files, bookmarks, presence, and emoji against the Slack Web API. This page walks through the exact
sequence using only the `flux` CLI. For the general plugin mechanics (capability grants, trust
model, everyday commands), see [Using plugins](./using-plugins.md).

:::note
Requires flux **0.14.7 or newer** — earlier versions composed Slack API URLs incorrectly (every
operation returned a 404), and `flux auth set` (step 2, option B) first shipped in 0.14.7.
:::

## 1. Install

```bash
flux plugin install slack
```

This resolves the newest signed `plugins-v*` pack release, verifies the index signature and the
archive's sha256, and unpacks the binary into the versioned store. Confirm it landed:

```bash
flux plugin status slack
```

```text
slack            ~/.flux/plugins/bin/slack/0.1.0/flux-plugin-slack   v0.1.0  [ok]  [verified]
    manifest:  v0.1.0  30 op(s)  ·  2 auth purpose(s)  ·  1 endpoint(s)  ·  2 datasource(s)  ·  caps: http, secret(2), blob
    auth:      · bot_token — not configured (env: SLACK_BOT_TOKEN, or `flux auth set slack bot_token`)
    auth:      · user_token — not configured (env: SLACK_USER_TOKEN, or `flux auth set slack user_token`)
    endpoint:  · slack.endpoint — env not set, defaults to https://slack.com/api
```

`ok`/`verified` only proves the binary launched and its hash matches the signed descriptor. The
`auth:` lines are the wiring itself: how each declared purpose would resolve right now (never the
value) — a stored token, or which env var is set.

## 2. Provide the tokens

Slack has two token kinds, and the plugin declares one auth purpose for each. Both come from a
Slack app of your own (create one at [api.slack.com/apps](https://api.slack.com/apps), install it
to your workspace):

| Purpose | Token | Used by |
|---|---|---|
| `bot_token` | Bot token (`xoxb-…`), from *OAuth & Permissions* | Most operations: messages, channels, users, files, reactions, bookmarks |
| `user_token` | User token (`xoxp-…`), granted via user scopes | `slack.search`, `slack.mentions`, `slack.unreads`, and presence |

You only need the purposes your operations use — a bot token alone covers messaging and reading
channels; add a user token when you want workspace search, mentions, or unread tracking.

**Option A — environment variables** (simplest when your shell already carries them):

```bash
export SLACK_BOT_TOKEN="xoxb-…"
export SLACK_USER_TOKEN="xoxp-…"   # optional; search/mentions/unreads/presence only
```

**Option B — store them once with `flux auth set`** (no env vars needed in any later session;
requires flux ≥ 0.14.7):

```bash
flux auth set slack bot_token      # hidden prompt — or pipe it in:
pass show slack/bot | flux auth set slack bot_token
flux auth set slack user_token
```

Stored tokens live in `~/.flux/credentials.toml` (created `0600`), the same store plugin OAuth
logins use, keyed `plugin:slack:bot_token`. A stored token **wins over the env var**; `--clear`
removes it. Either way, re-run `flux plugin status slack` and the lines flip to `✓`:

```text
    auth:      ✓ bot_token — stored token (`flux auth set slack bot_token`)
    auth:      ✓ user_token — stored token (`flux auth set slack user_token`)
```

Note what `status` never shows: the token value itself. The plugin subprocess never sees these as
OS environment variables at all — it is spawned with a cleared environment and requests the token
by purpose over an IPC capability call; the host resolves it (stored token first, then declared
env) and injects it as a bearer header on the plugin's behalf. See
[Credentials & secrets](../security/credentials.md) for the full resolution path.

The endpoint needs no configuration: `slack.endpoint` defaults to `https://slack.com/api`. Set
`SLACK_API_URL` only when routing through a proxy or a mock.

## 3. Verify

```bash
flux plugin call slack slack.test
```

`slack.test` calls Slack's `auth.test` with **each configured token** — the cheapest end-to-end
check that tokens, endpoint, and workspace all line up:

```json
{
  "count": 2,
  "status": "ok",
  "tokens": [
    { "ok": true, "role": "user", "team": "acme", "user": "jane.doe", "user_id": "U0…" },
    { "ok": true, "role": "bot",  "team": "acme", "user": "acme-bot", "bot_id": "B0…", "user_id": "U0…" }
  ]
}
```

A missing credential fails with the exact fix in the message
(``no credential for purpose `bot_token` — set a declared env key (tried ["SLACK_BOT_TOKEN"]) or
store one with `flux auth set slack bot_token` ``); an `invalid_auth` comes from Slack itself and
means the token value is wrong or revoked, not that the wiring is broken.

## 4. Call a real operation

Any of the plugin's declared operations works the same way — `flux plugin call slack <op> [json]`
(`--arg key=value` also works), or let an agent call them once the plugin is installed:

```bash
flux plugin call slack slack.channel.list --arg limit=10
flux plugin call slack slack.message.send '{"channel": "C0123456789", "text": "hello from flux"}'
flux plugin call slack slack.search '{"query": "deploy failed"}'        # needs user_token
flux plugin call slack slack.message.send '{"channel": "…"}' --dry-run  # validate input, send nothing
```

Write operations (`slack.message.send`, reactions, uploads, …) are policy-gated like every other
tool when an agent calls them; `--dry-run` validates the input against the operation's schema
without invoking it.

## Recap

| Step | Command | Failure mode if skipped |
|---|---|---|
| Install | `flux plugin install slack` | `no such plugin \`slack\`` |
| Tokens | `export SLACK_BOT_TOKEN=…` or `flux auth set slack bot_token` | ``no credential for purpose `bot_token` — …`` |
| Verify | `flux plugin call slack slack.test` | (this *is* the verification step) |
| User-token ops | also set/store `user_token` | ``no credential for purpose `user_token` — …`` |

## Related docs

- [Using plugins](./using-plugins.md) — install, pin, capability grants, and the trust model shared
  by every plugin.
- [Credentials & secrets](../security/credentials.md) — stored tokens, `flux auth set`, and how a
  token resolves without the plugin ever seeing raw environment variables.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — the manifest fields behind these
  grants.

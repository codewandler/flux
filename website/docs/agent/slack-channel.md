---
title: Slack channel setup
description: "Create a Slack app in Socket Mode and wire its tokens to a flux `channel slack` so a program answers in Slack."
---

# Slack channel setup

A [program](./programs.md) reaches Slack through a `channel slack` declaration. The adapter connects
over Slack **Socket Mode** (an outbound WebSocket — no public URL or inbound webhook to host), listens
for mentions, and posts each run's answer back into the originating thread. The adapter is compiled
into the stock `flux` binary; no special build flags are needed.

This page walks through creating the Slack app and obtaining the two tokens the channel needs:

| setting | token | where it comes from |
|---|---|---|
| `bot_token secret "SLACK_BOT_TOKEN"` | Bot token, `xoxb-…` | **OAuth & Permissions** → after install |
| `app_token secret "SLACK_APP_TOKEN"` | App-level token, `xapp-…` | **Basic Information** → App-Level Tokens |

Both are supplied as [`secret` references](./programs.md#secrets-are-references-never-plaintext) — the
host resolves them from the environment at load and redacts them from logs.

## 1. Create the app

1. Go to [api.slack.com/apps](https://api.slack.com/apps) → **Create New App** → **From scratch**.
2. Name it and pick your workspace.

## 2. Enable Socket Mode

1. **Settings → Socket Mode** → toggle **Enable Socket Mode** on.
2. When prompted, generate an **app-level token** with the `connections:write` scope. Copy the
   `xapp-…` value — this is your `SLACK_APP_TOKEN`. (You can also create it later under **Basic
   Information → App-Level Tokens**.)

## 3. Add bot scopes

Under **OAuth & Permissions → Scopes → Bot Token Scopes**, add at least:

- `app_mentions:read` — receive `@bot` mentions.
- `chat:write` — post replies.

Add `channels:history` (and `message.channels` events, below) only if you want the bot to see
non-mention messages in channels it is in.

## 4. Subscribe to events

Under **Event Subscriptions**, toggle **Enable Events** on, then under **Subscribe to bot events** add:

- `app_mention` — fires when someone `@`-mentions the bot.
- `message.channels` — optional; add it (together with the `channels:history` scope from step 3) only
  if you want the bot to act on plain channel messages, not just mentions.

With Socket Mode enabled there is **no Request URL to configure** — events arrive over the socket.

## 5. Install to the workspace

**OAuth & Permissions → Install to Workspace** → authorize. After install, copy the **Bot User OAuth
Token** (`xoxb-…`) — this is your `SLACK_BOT_TOKEN`. Invite the bot into a channel with `/invite @yourbot`.

## 6. Run the program

Export both tokens (plus your model provider credentials) and run:

```bash
export SLACK_BOT_TOKEN=xoxb-…
export SLACK_APP_TOKEN=xapp-…
export ANTHROPIC_API_KEY=sk-ant-…

flux app run crates/flux-app/examples/support-bot.flux
```

The program runs as a daemon until Ctrl-C. `@`-mention the bot in a channel it is in; it reads the
message, searches its docs, and replies in the thread. The runnable
[`support-bot.flux`](https://github.com/codewandler/flux/blob/main/crates/flux-app/examples/support-bot.flux)
example is the complete source.

## Restricting access

By default anyone can reach the bot. Narrow it on the `channel slack` declaration:

- `allow_users ["U0123…"]` — only these Slack user ids may trigger the agent.
- `allow_channels ["C0123…"]` — only these channels.

Empty lists (the default) allow everyone.

## Troubleshooting

- **`channel slack … built with --no-default-features`** — the adapter is on by default; you only see
  this if the binary was built with `--no-default-features`. Rebuild without it.
- **Program exits naming an environment variable** — a `secret "NAME"` reference is unset. Export the
  variable before running; the error names the variable, never its value.
- **No messages arrive** — confirm Socket Mode is on, the app-level token has `connections:write`, the
  bot has `app_mentions:read` + `chat:write`, the app is subscribed to `app_mention`, and the bot has
  been invited into the channel.

## Related docs

- [Multi-agent programs](./programs.md) — the `channel`, `agent`, `trigger`, and `journey` declarations this channel plugs into.
- [Datasources](./datasources.md) — giving the bot indexed docs to answer from.
- [Credentials & secrets](../security/credentials.md) — how `secret` references resolve and stay out of logs and model context.
- [Slack plugin](../plugins/slack.md) — the other direction: an agent *calling* the Slack Web API rather than being hosted on it.

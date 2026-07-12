# Getting started with flux

flux is a deterministic agent platform. You describe an app — its agents, the channels they are
reached on, the datasources they answer from, and the journeys that run per event — as a single
native `.flux` program, then run it with `flux app run <program.flux>`.

## Install

Download a release binary from the releases page, or build from source with `cargo build -p flux-cli`.
The Slack channel adapter is compiled in by default.

## Your first program

A minimal support bot needs four declarations: an `agent`, a `channel` it listens on, a `datasource`
it answers from, and a `trigger` that wakes the agent on each message. See `support-bot.flux` in this
directory for the complete example.

Run it:

```
flux app run crates/flux-app/examples/support-bot.flux
```

The program keeps running as a daemon, listening on its channels, until you press Ctrl-C.

## Secrets

Never inline a token. Reference an environment variable instead: `bot_token secret "SLACK_BOT_TOKEN"`.
The host resolves the reference from the environment once at load, and redacts the value from all logs.

---
title: Multi-agent programs
---

# Multi-agent programs

A single agent is one loop. A **multi-agent program** is a whole application: several agents, the
channels they are reached on, the data they answer from, and the flows that run when something happens.
In flux you declare all of that in one native Flux-Lang `.flux` file — no YAML, no glue code, no
separate config format.

## The file *is* the app

A program file is a set of typed module declarations. Each one names a part of the system and carries
its settings inline as ordinary Flux-Lang values:

- **`agent`** — a model, its tool allow-list, its datasources, and a description.
- **`channel`** — a surface the app is reached on (CLI, Slack, HTTP/A2A, …).
- **`datasource`** — grounded knowledge an agent answers from (e.g. a Markdown corpus).
- **`trigger`** — an event to listen for, and the journey to run when it fires.
- **`journey`** — a named flow that does the work. A journey is an ordinary Flux-Lang flow.

Here is a complete Slack support bot — an agent, its channel, a docs datasource, and the journey that
runs per message:

```flux
agent assistant
  model "claude-sonnet-4-6"
  tools [search, send]
  datasources [docs]
  description "answers support questions from the docs"

channel slack
  bot_token secret "SLACK_BOT_TOKEN"
  app_token secret "SLACK_APP_TOKEN"

datasource docs
  kind "markdown"
  path "./docs"

trigger on_message
  on "slack"
  run answer
  agent assistant

journey answer
  agent assistant
  flow
    $hits = search($text)
    return $hits
```

That is the entire application. The `flow` body is regular Flux-Lang — see the
[language overview](../language/overview.md) and [flows & syntax](../language/flows-and-syntax.md).

## Secrets are references, never plaintext

Notice `secret "SLACK_BOT_TOKEN"`. A `secret` declaration is a **reference to an environment variable**,
resolved by the host at load time. Tokens and keys never live inline in the file, so a program is safe
to commit and share. Set the referenced variables in the environment before you run.

## How a program runs

At load, the host wires the modules onto an **event bus**. **Triggers** subscribe to named events —
`"startup"`, `"user_input"`, a channel name like `"slack"` — and each dispatches its **journey** with
the event payload in scope (for example `$text` for an incoming message). Journeys run as flows through
the same safety envelope as everything else in flux: authorization, approval, then guarded IO.

Inside a journey, agents coordinate through a small set of orchestration operations:

- **`emit`** — publish an event onto the bus for other triggers to pick up.
- **`send`** — deliver a message out on a channel (as in `send({ "channel": "cli", "message": … })`).
- **`ask`** — put a question to another agent (or a human) and wait for the reply.
- **`spawn`** — start a sub-agent to work in parallel and hand back a result.

This is what makes it multi-agent: independent agents react to events and hand work to one another,
rather than one loop doing everything.

## Running a program

```bash
flux run app.flux        # or: flux app run app.flux
```

Programs run headless, so they are **deny-by-default**: the orchestration verbs (`emit`/`send`/`ask`/
`spawn`) and read-only builtins are pre-allowed, but anything that changes the world (`write`, `bash`,
`git_*`, …) is **denied** outright — there is no human at a prompt to approve it. Pass `--yes` to run a
trusted, pre-authored program under an allow-all approver instead — it approves every step, destructive
ones included. To expose the program as a long-running HTTP/A2A daemon instead of a one-shot run:

```bash
flux app run --serve 127.0.0.1:8787 --yes
```

See [Agent-to-agent (A2A)](./a2a.md) for reaching a running program from other agents.

## Where to go next

- [Concepts](../concepts.md) — the plan-not-transcript model and the one safety envelope every operation runs through.
- [Language overview](../language/overview.md) and [flows & syntax](../language/flows-and-syntax.md) — the Flux-Lang you write journey bodies in.
- [Agent-to-agent (A2A)](./a2a.md) — reaching programs over the network.
- Runnable examples live in the [flux repository](https://github.com/codewandler/flux).

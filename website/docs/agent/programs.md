---
title: Multi-agent programs
description: "Defining multi-agent program files, channels, and journey composition in a single Flux-Lang source."
---

# Multi-agent programs

A single agent is one loop. A multi-agent program is a whole application: permissions, agents,
channels, datasources, triggers, and journeys declared in one Flux-Lang `.flux` file.

Use programs when you want more than one prompt-response turn: a Slack bot, an A2A service, a
scheduled workflow, or an event-driven assistant that coordinates multiple agents.

## The file *is* the app

A program file is a set of typed module declarations. Each one names a part of the system and carries
its settings inline as ordinary Flux-Lang values:

- **`permissions`** — the app-wide operation ceiling, declared as exact `allow`/`deny` names.
- **`agent`** — a model, its model-visible tools, datasources, description, and optional capability narrowing.
- **`agent_loop`** — a named outer loop, authored as an ordinary flow body, that an agent selects
  with `loop "<name>"`. An agent that declares none gets the built-in adaptive loop. See
  [`agent_loop`](../language/modules-and-programs.md#agent_loop--authoring-the-outer-loop).
- **`channel`** — a surface the app is reached on (CLI, Slack, HTTP/A2A, …).
- **`datasource`** — grounded knowledge an agent answers from (e.g. a Markdown corpus) — see [Datasources](./datasources.md).
- **`trigger`** — an event to listen for, and what runs when it fires: an **agent** (the model drives
  the turn) or a **journey** (a fixed flow).
- **`journey`** — a named flow that does the work. It may name an owning agent whose model, persona,
  datasource scope, and capability narrowing the flow inherits.

Here is a complete Slack support bot — an agent, its channel, a docs datasource, and an agent-bound
trigger that answers each message ([`crates/flux-app/examples/support-bot.flux`](https://github.com/codewandler/flux/blob/main/crates/flux-app/examples/support-bot.flux)):

```flux
agent assistant
  model "claude-sonnet-5"
  thinking true
  effort "medium"
  tools [search]
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
  agent assistant
```

That is the entire application. On each Slack mention the agent reads the message, calls `search` over
the indexed docs, and its answer is posted back into the thread. The trigger is **agent-bound** (it
names an `agent`, not a `run` journey), so the model drives the turn. See the [Slack channel setup
guide](./slack-channel.md) for creating the Slack app and its tokens.

`thinking` and `effort` are agent settings, so they also apply to cognition operations, completion
rendering, and compaction owned by that agent. Omit them for the default (`thinking false`, no effort
hint); accepted effort values are `"low"`, `"medium"`, `"high"`, `"xhigh"`, and `"max"`.

Program agents use Flux's small universal harness protocol and default to the `general` profile. The
agent `description` is authored instructions after that protocol; it never replaces the harness.
Select `profile "coding"` when the agent should also receive Flux's coding lifecycle. A longer
persona can use `instructions "…"` instead of `description`, or load workspace-confined files with
`instruction_files ["bot/PERSONA.md"]`.

For deterministic, fixed-step work a trigger can run a **journey** — a named Flux-Lang flow — instead
of an agent (`run <journey>` with no trigger-level `agent`). A journey may still declare `agent
<name>` inside its own body: that supplies the execution context without giving the model control of
the graph. See the [language overview](../language/overview.md) and [flows &
syntax](../language/flows-and-syntax.md).

## Capabilities in app source

Programs can make their headless authority explicit:

```flux
permissions
  allow [search, "ai.reason", send]
  deny [write, edit, bash]

agent guide
  tools [search]
  datasources [handbook]
  allow [search, "ai.reason", send]
```

The top-level `allow` list is a hard app ceiling. An agent-level list intersects it; app and agent
denies are combined and always win. Missing `allow` inherits the parent/default set, while `allow []`
is explicitly empty. Entries are exact operation names, so dotted names such as `ai.reason` are
quoted. Local `.flux/config.toml` rules remain the place for subject-scoped approvals.

`tools` is deliberately separate from `allow`: `tools` is the catalog an open-ended agent may choose
from, while `allow` governs calls already authored into journeys. Neither local approval rules nor
`--yes` can restore an operation removed by the app or agent ceiling.

## Secrets are references, never plaintext

Notice `secret "SLACK_BOT_TOKEN"`. A `secret` declaration is a **reference to an environment variable**,
resolved by the host at load time. Tokens and keys never live inline in the file, so a program is safe
to commit and share. Set the referenced variables in the environment before you run.

## How a program runs

At load, the host wires the modules onto an **event bus**. **Triggers** subscribe to named events —
`"startup"`, `"user_input"`, a channel name like `"slack"` — and each dispatches its **agent** or
**journey** with the event payload in scope (for example `$text` for an incoming message). An
agent-bound trigger wakes a model turn; a journey-bound one runs a flow — both through the same safety
envelope as everything else in flux: authorization, approval, then guarded IO.

Inside a journey, agents coordinate through a small set of orchestration operations:

- **`emit`** — publish an event onto the bus for other triggers to pick up.
- **`send`** — deliver a message out on a channel (as in `send({ "channel": "cli", "message": … })`).
- **`ask`** — put a question to another agent (or a human) and wait for the reply.
- **`spawn`** — run a named journey to completion and hand back its result.

This is what makes it multi-agent: independent agents react to events and hand work to one another,
rather than one loop doing everything.

## Running a program

```bash
flux run app.flux        # or: flux app run app.flux
```

Programs without an explicit `permissions` declaration retain the safe legacy posture: orchestration
verbs and read-only builtins are pre-allowed, while other operations need approval and are denied in a
headless run. `--yes` auto-approves those remaining prompts for a trusted legacy program. With an
explicit app ceiling, `--yes` applies only inside that ceiling and can never widen it. Destructive
operations still pass the runtime's risk and authorization checks. To expose the program as a
long-running HTTP/A2A daemon instead of a one-shot run:

```bash
flux app run app.flux --serve 127.0.0.1:8787 --yes
```

See [Agent-to-agent (A2A)](./a2a.md) for reaching a running program from other agents.

## Where to go next

- [Concepts](../concepts.md) — the authored Flux-Lang outer loop, provider-native model stages, and
  the one safety envelope every operation runs through.
- [Language overview](../language/overview.md) and [flows & syntax](../language/flows-and-syntax.md) — the Flux-Lang you write journey bodies in.
- [Agent-to-agent (A2A)](./a2a.md) — reaching programs over the network.
- Runnable examples live in the [flux repository](https://github.com/codewandler/flux).

## Related docs

- [Modules, composite ops & programs](../language/modules-and-programs.md) — the language-level declarations.
- [Agent-to-agent (A2A)](./a2a.md) — exposing a program over the network.
- [Operations](../language/ops.md) — `emit`, `send`, `ask`, and `spawn`.

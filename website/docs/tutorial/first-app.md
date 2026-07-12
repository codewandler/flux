---
title: 3. Build a local docs assistant
description: Combine a Flux agent, CLI channel, Markdown datasource, triggers, and a deterministic startup journey in one app.
---

# Build a local docs assistant

A Flux app describes a whole event-driven application in one `.flux` file. For the capstone, you
will connect a model-backed agent to the terminal and give it searchable access to the handbook.

## Create `assistant.flux`

Create `assistant.flux` beside `brief.flux`:

```flux
agent guide
  tools [search]
  datasources [handbook]
  description "For every question, call search before answering. Answer only from the Northstar handbook. If search finds no answer, say so."

channel cli

datasource handbook
  kind "markdown"
  path "./docs"

trigger welcome
  on "startup"
  run show-welcome

trigger questions
  on "user_input"
  agent guide

journey show-welcome
  flow
    send({"channel": "cli", "message": "Northstar handbook assistant ready. Ask a question."})
    return ""
```

This is both application configuration and executable Flux-Lang:

- `agent guide` defines the model-facing role and grants only the `search` operation over the
  declared datasource. The runtime scopes that operation to `handbook`; an omitted `source` is
  filled in automatically, and another source is rejected.
- `channel cli` turns each non-empty terminal line into a `user_input` event.
- `datasource handbook` indexes the Markdown files under `./docs` when the app starts.
- `trigger questions` routes each input event to the agent. It is agent-bound, so it does not need a
  fixed journey.
- `trigger welcome` runs the deterministic `show-welcome` journey on startup. Its body is an
  ordinary Flux-Lang flow, executed through the same runtime as `brief.flux`.

## Run the app

Start it with your configured model:

```bash
flux app run assistant.flux -m sonnet
```

After the welcome message, type a complete question and press Enter:

```text
What happens to my edits if I work offline?
```

The agent is instructed to call its only operation, `search`, retrieve the relevant handbook
passage, and answer that offline edits synchronize after the device reconnects. Try another
standalone question:

```text
What are the support hours and timezone?
```

Press **Ctrl-C** to stop the app.

:::note
The local CLI delivers each line as an event without a conversation or thread identifier, so make
each question self-contained. Channels such as Slack carry thread identifiers and can preserve a
session across follow-up messages.
:::

## Follow one question through the app

```text
terminal line
    -> user_input event
    -> questions trigger
    -> guide agent
    -> guarded search over handbook
    -> terminal result
```

The model decides how to phrase the answer, while its role instructs it to ground every answer with
the granted `search` operation. It still cannot open arbitrary files, query an undeclared
datasource, or perform an undeclared effect. The app host indexes the datasource, the runtime scopes
and dispatches the operation, and the channel renders the result.

The datasource path is resolved relative to `assistant.flux`, not the directory from which you
launch flux. If you edit either handbook file, restart the app to rebuild its in-memory index.

## You built a Flux app

Starting from basic terminal commands, you have now:

- previewed a model-generated plan before it ran;
- watched an effect cross the approval boundary;
- authored a typed, parameterized Flux-Lang flow;
- bounded exactly what an explicit reasoning step could see; and
- connected an agent, channel, datasource, triggers, and journey into a runnable app.

## Where to go next

- [A ten-minute Flux-Lang tour](../language/tour.md) — branching, iteration, concurrency, and guard
  rails.
- [Editor setup](../language/editors.md) — syntax highlighting, diagnostics, completion, hover, and
  formatting for `.flux` files.
- [Multi-agent programs](../agent/programs.md) — more agents, channels, events, and journeys.
- [Slack channel setup](../agent/slack-channel.md) — move the same docs-assistant pattern into Slack.
- [Agent-to-agent](../agent/a2a.md) — expose an agent over HTTP/A2A.
- [Safety and approvals](../agent/safety.md) — configure the envelope that guarded every exercise.

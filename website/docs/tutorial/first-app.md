---
title: 3. Make the docs assistant reliable
description: Start with model-controlled retrieval, observe the gap, then move determinism into an agent-owned journey.
---

# Make the docs assistant reliable

A Flux app describes a whole event-driven application in one `.flux` file. For the capstone, you
will first build the obvious docs assistant: give an agent `search`, tell it to use the handbook, and
route terminal questions to it. Then you will test the assumption hidden in that design and move the
reliable steps into an explicit journey.

This is the central Flux idea in miniature: **the model can interpret a question, but it should not
be the runtime for steps your application must perform.**

## Part A: let the agent decide how to answer

Create `assistant.flux` beside `brief.flux`:

```flux runnable="first-app-a" title="First app · Part A"
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
    send(channel: "cli", message: "Northstar handbook assistant ready. Ask a question.")
    return ""
```

Start it with your configured model:

```bash
flux app run assistant.flux -m sonnet
```

After the welcome message, ask both questions:

```text
What happens to my edits if I work offline?
What are the support hours and timezone?
```

The first answer should say that offline edits synchronize after the device reconnects. The second
should report Monday–Friday, 09:00–17:00 Central European Time. Your run may answer both correctly.
It may also skip `search`, claim the handbook lacks a fact that is present, or answer from prior model
knowledge. Repeat a question if you want to probe that behavior.

That variability is the experiment, not a tutorial failure. The built-in authored adaptive loop
routes model judgment through provider-native operation schemas and host-frozen action batches, and
`tools [search]` plus the datasource declaration give it a narrow live capability. But the
description still expresses an application invariant as prompt advice: the model decides whether
the question needs `search`. A successful answer does not change that structure.

```text
terminal line
    -> user_input event
    -> guide agent
    -> model may choose search
    -> model answers
```

Press **Ctrl-C** to stop the app.

## Part B: require retrieval in a journey

Replace `assistant.flux` with this version:

```flux runnable="first-app-b" title="First app · Part B"
permissions
  allow [search, "ai.reason", send]
  deny [write, edit, bash]

agent guide
  tools [search]
  datasources [handbook]
  allow [search, "ai.reason", send]
  description "Answer only from the Northstar handbook. If the supplied results do not contain the answer, say so."

channel cli

datasource handbook
  kind "markdown"
  path "./docs"

trigger welcome
  on "startup"
  run show-welcome

trigger questions
  on "user_input"
  run answer-question

journey show-welcome
  flow
    send(channel: "cli", message: "Northstar handbook assistant ready. Ask a question.")
    return ""

journey answer-question
  agent guide
  flow
    hits = search(query: "{text}", source: "handbook")
    ctx grounding
      purpose "answer one question from the Northstar handbook"
      budget 6000
      include hits
    answer = ai.reason(ask: "Question: {text}\nAnswer only from the handbook results. If they do not contain the answer, say so.", ctx: grounding)
    send(channel: "cli", message: "{answer}")
    return ""
```

Run the same command and ask the same two questions again:

```bash
flux app run assistant.flux -m sonnet
```

Every completed `answer-question` run now follows the authored graph:

```text
terminal line
    -> user_input event
    -> answer-question journey (owned by guide)
    -> guarded search over handbook
    -> bounded grounding context
    -> guide model reasons over those results
    -> send answer to terminal
```

The model still interprets the retrieved passage and chooses the wording. It no longer chooses
whether retrieval happens: `search` is an executable node before `ai.reason`. If search or reasoning
fails, the journey fails instead of silently taking a different route.

## What ownership and permissions mean

`journey answer-question` is a **journey**, not an open-ended agent turn. Its `flow` is the fixed
operation graph; `agent guide` supplies the model, role instructions, datasource boundary, and the
agent-level capability narrowing used by that graph.

The two permission layers have different jobs:

- Top-level `permissions` is the app-wide ceiling. Operations omitted from `allow`, or named in
  `deny`, are not present in an app run—even with `--yes`.
- Agent `allow` can narrow that ceiling for the agent and its owned journeys; it cannot widen it.
- `tools` controls what an open-ended agent may choose from. `allow` controls what authored journey
  nodes may execute. That is why the journey can call `ai.reason` and `send` without advertising
  either operation to the free-running agent in Part A.
- These lists contain exact operation names. Your local `.flux/config.toml` still handles
  subject-scoped approval rules such as a particular command or path.

The declarations make the harmless model-backed path run headlessly without `--yes`. Destructive
effects retain the runtime's risk checks; app permissions never bypass authorization, approval, or
guarded IO.

## Flow versus journey

A **flow** is an executable graph with inputs, values, control flow, and operation calls. The
`answer-handbook` flow from the previous lesson can be parsed and run on its own.

A **journey** places a flow inside an application lifecycle: it has a name, can be selected by a
trigger or `spawn`, receives event data such as `{text}`, and may have an owning agent. The flow says
*what executes*; the journey says *when, in which app context, and as whom*.

:::note
The local CLI delivers each line as an event without a conversation or thread identifier, so make
each question self-contained. Channels such as Slack carry thread identifiers for agent-bound turns.
This journey starts a fresh deterministic run for every event by design.
:::

The datasource path is resolved relative to `assistant.flux`, not the directory from which you
launch flux. If you edit either handbook file, restart the app to rebuild its in-memory index.

## You built a Flux app

Starting from basic terminal commands, you have now:

- watched typed intent, native-schema exploration, and action-batch approval;
- authored a typed, parameterized Flux-Lang flow;
- observed why a prompt is not a control-flow guarantee;
- moved mandatory retrieval into a deterministic journey;
- bounded exactly what the reasoning step could see; and
- declared the app and agent capabilities in the application source.

## Related docs

- [A ten-minute Flux-Lang tour](../language/tour.md) — branching, iteration, concurrency, and guard
  rails.
- [Multi-agent programs](../agent/programs.md) — more agents, channels, events, and journeys.
- [Datasources](../agent/datasources.md) — beyond the markdown index: what `datasource` can point at
  and how it is governed.
- [Slack channel setup](../agent/slack-channel.md) — move the same docs-assistant pattern into Slack.
- [Safety and approvals](../agent/safety.md) — the envelope that still guards every declared operation.

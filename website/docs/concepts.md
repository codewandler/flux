---
sidebar_position: 3
title: Concepts
description: "Core mental model for flux: plan-first execution, symbolized values, and deterministic evidence flow."
---

# Concepts

These are the core ideas that make flux different from a normal chat-driven agent. Read this page
before the agent, language, or security guides; the rest of the docs use this vocabulary.

## Plan, not transcript

A flux turn is not primarily a chat transcript. The model emits a typed plan. The runtime executes
that plan node by node, records evidence, and returns the result.

## Symbols, not raw output

Flux-Lang plans refer to symbols such as `$src` or `$tests`. A symbol names an immutable stored value.
The runtime owns the value store. The model sees summaries, transcripts, and explicit context packs
instead of every raw tool output being replayed into the prompt.

## One safety envelope

Every production operation runs through the same chain:

```text
authorization -> approval -> guarded IO
```

This applies to built-in tools, plugin operations, sub-agent work, app journeys, and model-routed
plans. There is no separate "trusted shortcut" for a tool call.

## Operations do, datasources know

An **operation** is the universal callable unit — the system's verbs. Reading a file, running a
test, calling a plugin, asking the model to rank items: each is an operation in one catalog, and
each crosses the safety envelope above. A **datasource** is the agent's knowledge layer — an
indexed store of records (workspace docs, integration data) the agent looks things up in.

The two meet cleanly: a datasource is *read through* operations. Retrieval (`search`, `get`, …) is
just more read-only ops in the same catalog, so knowledge access is governed exactly like action.
See [Datasources](./agent/datasources.md) and [Operations](./language/ops.md).

## A multi-pass loop

A turn is not one blind guess. The loop first **orients** (one planner call that either answers, emits
a plan, or asks for a small read-only look), optionally **gathers** context in bounded read-only
rounds, then **plans, executes, and revises** — feeding each result back so the next step is grounded
in what actually happened. A trivial request still costs a single planner call. See
[The agent loop](./agent/agent-loop.md).

## Evidence & durability

As a turn runs, flux records an auditable trail — tool calls, destructive markers, plan attempts (with
each plan's fingerprint and readable graph), and compaction — and flushes it durably to the session's
event log. Sessions are event-sourced and resumable, and long sessions compact older turns into a
summary rather than dropping history. You can always explain what the agent did and why it was allowed.

## Local-first

flux is designed to run on your machine first. Secrets stay local, provider credentials are explicit,
and plugins receive only the host capabilities declared in their manifests.

## Related docs

- [The agent loop](./agent/agent-loop.md) — how a turn orients, gathers, plans, and revises.
- [Flux-Lang overview](./language/overview.md) — the plan language behind the model boundary.

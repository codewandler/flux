---
sidebar_position: 3
title: Concepts
description: "Core mental model for flux: typed stages, authored flows, symbolized values, and deterministic evidence flow."
---

# Concepts

These are the core ideas that make flux different from a normal chat-driven agent. Read this page
before the agent, language, or security guides; the rest of the docs use this vocabulary.

## Authored control, not model-generated code

A flux turn is driven by an authored Flux-Lang outer loop. Inside provider-native typed stages, the
model interprets intent, gathers evidence, and proposes literal calls to visible operations. The host
captures effectful proposals and freezes them into an immutable action batch; only an approved batch
is recorded and executed.

The default conversational loop does not ask the model for per-turn executable Flux. A separate,
explicit [`op.register`](./agent/saved-flows.md#register-an-operation-during-a-turn) operation may
accept exactly one agent-proposed composite operation, but the host analyzes, scopes, and guards that
source before installing it. It extends the available vocabulary; it does not replace the authored
outer loop.

## Symbols, not raw output

Flux-Lang flows refer to symbols such as `src` or `tests`. A symbol names an immutable stored value.
The runtime owns the value store. The model sees summaries, transcripts, and explicit context packs
instead of every raw tool output being replayed into the prompt.

## One safety envelope

Every production operation runs through the same chain:

```text
authorization -> approval -> guarded IO
```

This applies to evidence reads, approved action batches, built-in tools, plugin operations, sub-agent
work, and app journeys. There is no separate "trusted shortcut" for a model-native call.

## Operations do, datasources know

An **operation** is the universal callable unit — the system's verbs. Reading a file, running a
test, calling a plugin, asking the model to rank items: each is an operation in one catalog, and
each crosses the safety envelope above. A **datasource** is the agent's knowledge layer — an
indexed store of records (workspace docs, integration data) the agent looks things up in.

The two meet cleanly: a datasource is *read through* operations. Retrieval (`search`, `get`, …) is
just more read-only ops in the same catalog, so knowledge access is governed exactly like action.
See [Datasources](./agent/datasources.md) and [Operations](./language/ops.md).

## An adaptive typed loop

A turn is not one blind guess. A typed intent stage narrows the live operation catalog; exploration
uses exact provider-native schemas to gather safe evidence or capture effects; the host freezes an
immutable batch; approval produces a one-shot receipt; and execution reports return to the same
native ledger for local correction. Questions suspend and resume the authored flow. See
[The agent loop](./agent/agent-loop.md).

## Evidence & durability

As a turn runs, flux records an auditable trail—intent, selected capabilities, tool calls, proposed
action batches, approval events, execution reports, authored/host-derived flows, and compaction—and flushes it durably to the session's
event log. Sessions are event-sourced and resumable, and long sessions compact older turns into a
summary rather than dropping history. You can always explain what the agent did and why it was allowed.

## Local-first

flux keeps runtime state and credential storage local by default. When you choose a remote model
provider, flux intentionally sends it the prompt, conversation, and selected context or workspace
excerpts needed for that call; local-first is not a zero-egress guarantee. Provider credentials stay
at the host boundary, and plugin host callbacks are limited to the capabilities declared in their
manifests. Trusted native plugins still carry the [plugin trust boundary](./security/plugin-trust.md).

## Related docs

- [The agent loop](./agent/agent-loop.md) — how intent, exploration, approval, execution, and repair compose.
- [Flux-Lang overview](./language/overview.md) — the authored language around model boundaries.

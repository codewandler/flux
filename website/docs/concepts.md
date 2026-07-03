---
sidebar_position: 3
title: Concepts
---

# Concepts

## Plan, not transcript

A flux turn is not primarily a chat transcript. The model emits a plan. The runtime executes that plan
node by node, records evidence, and returns the result.

## Symbols, not raw output

Flux-Lang plans refer to symbols such as `$src` or `$tests`. A symbol names an immutable stored value.
The runtime owns the value store; the model sees summaries, transcripts, and explicit context packs
rather than repeatedly receiving every raw output.

## One safety envelope

Every production operation runs through the same chain:

```text
authorization -> approval -> guarded IO
```

This applies to built-in tools, plugin operations, sub-agent work, app journeys, and model-routed
plans. There is no separate "trusted shortcut" for a tool call.

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

flux is designed to run on your machine. Secrets stay local, provider credentials are explicit, and
plugins receive only the host capabilities declared in their manifests.

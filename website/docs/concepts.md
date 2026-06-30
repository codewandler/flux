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

## Local-first

flux is designed to run on your machine. Secrets stay local, provider credentials are explicit, and
plugins receive only the host capabilities declared in their manifests.

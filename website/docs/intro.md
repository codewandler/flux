---
sidebar_position: 1
title: Overview
---

# What is flux?

flux is a deterministic agent platform built on one thesis: **the LLM is not the runtime**.

Instead of letting a model schedule tools one call at a time, flux asks the model to compile a request
into a typed, readable Flux-Lang plan. A deterministic Rust runtime executes that plan through one
mandatory safety envelope:

```text
authorization -> approval -> guarded IO
```

The result is a plan you can inspect, replay, and reason about. The model proposes work. The runtime
decides what is allowed and performs the work.

## The three pillars

- **Agent**: the local coding agent, CLI/TUI, SDK, and server surfaces.
- **Flux-Lang**: the plan language and reference interpreter.
- **Improvement loop**: the eval and self-improvement harness used to improve flux itself.

## Public docs vs project docs

This site is the public documentation for users and integrators. It explains stable concepts, getting
started paths, Flux-Lang syntax, SDK entry points, and plugin authoring rules.

The repository also has internal contributor docs under `docs/` and crate-level `docs/` directories.
Those are design records, story boards, roadmap notes, and implementation references. They are useful
when contributing to flux, but they are intentionally more detailed and more volatile than this site.

## Start here

- New to flux: read [Getting started](./getting-started.md).
- Interested in the language: read [Flux-Lang overview](./language/overview.md).
- Embedding flux: read [FlowClient](./sdk/flow-client.md).

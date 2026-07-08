---
sidebar_position: 1
title: Overview
description: "Public entry point for flux docs: what flux is, what is new, and where to learn next."
---

# What is flux?

flux is a deterministic agent platform for building and running tool-using agents without giving the
model direct control of the machine.

The thesis is simple: **the LLM is not the runtime**. The model compiles a request into a typed,
readable Flux-Lang plan. A deterministic Rust runtime then executes that plan through one mandatory
safety envelope:

```text
authorization -> approval -> guarded IO
```

The result is an agent run you can inspect, replay, and reason about. The model proposes work; the
runtime decides what is allowed and performs the work.

## The three pillars

- **Agent**: the local coding agent — CLI/TUI, an embeddable Rust SDK, an HTTP/[A2A](./agent/a2a.md)
  server, and [multi-agent programs](./agent/programs.md).
- **Flux-Lang**: the plan language and reference interpreter.
- **Improvement loop**: the eval and self-improvement harness used to improve flux itself.

## Public docs vs project docs

This site is the public documentation for users and integrators. It covers stable concepts, the CLI,
Flux-Lang, the SDK, plugins, and the security model.

The repository also contains internal contributor docs under `docs/` and crate-level `docs/`
directories. Those are design records, story boards, roadmap notes, and implementation references.
They are useful when contributing, but they are more detailed and more volatile than this site.

## Start here

- New to flux: read [Getting started](./getting-started.md).
- How a turn works: read [Concepts](./concepts.md) and [The agent loop](./agent/agent-loop.md).
- What flux is allowed to do: read [Safety & approvals](./agent/safety.md).
- Interested in the language: read [Flux-Lang overview](./language/overview.md).
- Embedding flux: read the [SDK overview](./sdk/overview.md).
- Replay, fork, and diff past runs: read [Time Machine](./agent/time-machine.md).
- Something not working: check [Troubleshooting](./troubleshooting.md).

## Related docs

- [Getting started](./getting-started.md) — install flux and run the mock provider.
- [Concepts](./concepts.md) — understand plans, symbols, evidence, and the safety envelope.

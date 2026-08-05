---
sidebar_position: 1
title: Overview
description: "Public entry point for flux docs: what flux is, what is new, and where to learn next."
---

# What is flux?

flux is a deterministic agent platform for building and running tool-using agents without giving the
model direct control of the machine.

The thesis is simple: **the LLM is not the runtime**. Provider-native typed stages interpret intent,
gather evidence, and propose literal operation calls inside an authored Flux-Lang outer loop. The
host freezes effectful proposals into action batches, then a deterministic runtime executes approved
calls through one mandatory safety envelope:

```text
authorization -> approval -> guarded IO
```

The default conversational loop never asks the model to generate per-turn executable Flux. Authored
control flow owns order, bounds, approval, and stopping; the model supplies bounded judgment. The
result is a run you can inspect, replay, and reason about. There is one explicit source-generation
seam for reusable vocabulary: [`op.register`](./agent/saved-flows.md#register-an-operation-during-a-turn)
accepts exactly one agent-proposed composite operation, then analyzes, scopes, and guards it before
installation.

## The core product

- **Agent**: the local coding agent — CLI/TUI, an embeddable Rust SDK, an HTTP/[A2A](./agent/a2a.md)
  server, and [multi-agent programs](./agent/programs.md).
- **Flux-Lang**: the authored workflow language and reference interpreter.

## Public docs vs project docs

This site is the public documentation for users and integrators. It covers stable concepts, the CLI,
Flux-Lang, the SDK, plugins, and the security model.

The deployed site follows `main`, and redeploys on every push to it. That means these pages describe
the current state of the source — including changes that have **not yet been released**, so a page may
document behaviour your installed build does not have. Check
[What's new](./whats-new.md) for the customer-facing history: entries under a version heading are
released, and anything under `Unreleased` is on `main` only. `flux --version` tells you which build
you are actually running.

The repository also contains internal contributor docs under `docs/` and crate-level `docs/`
directories. Those are design records, story boards, roadmap notes, and implementation references.
They are useful when contributing, but they are more detailed and more volatile than this site.

## Choose your path

- **Try flux:** [install it and run the mock provider](./getting-started.md), then
  [build your first Flux app](./tutorial.md).
- **Use the coding agent:** start with the [CLI](./agent/cli.md), choose a
  [model provider](./agent/providers.md), and add [project context](./agent/project-context.md).
- **Understand execution:** read [Concepts](./concepts.md), [Infrastructure](./infrastructure.md), and
  [The agent loop](./agent/agent-loop.md).
- **Present flux to your team:** step through the
  [interactive engineering presentation](/presentation/) — the runtime boundary, a guarded live
  demo, connectors, and Exchange in about 20 minutes.
- **Build and integrate an app:** define a [multi-agent program](./agent/programs.md), learn
  [Flux-Lang](./language/overview.md), then connect [channels](./channels/overview.md) or
  [plugins](./plugins/using-plugins.md).
- **Embed or serve an agent:** use the [Rust SDK](./sdk/overview.md), the
  [HTTP API](./agent/http-api.md), or [A2A](./agent/a2a.md).
- **Deploy safely:** set the [approval and permission boundary](./agent/safety.md), then review the
  [security model](./security/overview.md).
- **Diagnose or update:** check [Troubleshooting](./troubleshooting.md) and
  [What's new](./whats-new.md).

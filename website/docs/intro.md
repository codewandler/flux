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
- **[Improvement loop](./agent/improvement.md)**: the eval and self-improvement harness used to
  measure flux against benchmarks. Unlike the other two, this pillar is **de-prioritized and on
  hold** — the harness runs and its evidence is trustworthy, but a repeatable grader-confirmed gain
  is unproven. Treat it as a measurement tool, not a shipped capability.

## Public docs vs project docs

This site is the public documentation for users and integrators. It covers stable concepts, the CLI,
Flux-Lang, the SDK, plugins, and the security model.

The deployed site follows the latest published release. Changes on `main` are built as previews but
do not replace the stable site until a release is published. See [What's new](./whats-new.md) for the
customer-facing history and `flux --version` for your installed version.

The repository also contains internal contributor docs under `docs/` and crate-level `docs/`
directories. Those are design records, story boards, roadmap notes, and implementation references.
They are useful when contributing, but they are more detailed and more volatile than this site.

## Start here

- New to flux: [build your first Flux app](./tutorial.md) in the guided beginner tutorial.
- Need only installation and command examples: read [Getting started](./getting-started.md).
- How a turn works: read [Concepts](./concepts.md) and [The agent loop](./agent/agent-loop.md).
- How the pieces fit together: see [Infrastructure](./infrastructure.md).
- What flux is allowed to do: read [Safety & approvals](./agent/safety.md).
- Interested in the language: read [Flux-Lang overview](./language/overview.md).
- Embedding flux: read the [SDK overview](./sdk/overview.md).
- Measuring and improving behavior: read [Evaluation and improvement](./agent/improvement.md).
- Replay, fork, and diff past runs: read [Time Machine](./agent/time-machine.md).
- Something not working: check [Troubleshooting](./troubleshooting.md).

## Related docs

- [Build your first Flux app](./tutorial.md) — go from a guarded agent run to a local docs assistant.
- [Getting started](./getting-started.md) — install flux and run the mock provider.
- [Concepts](./concepts.md) — understand plans, symbols, evidence, and the safety envelope.
- [Infrastructure](./infrastructure.md) — see the runtime, safety boundary, and crate layers at a glance.
- [What's new](./whats-new.md) — see release-by-release user-visible changes.

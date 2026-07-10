---
title: Infrastructure
description: How flux separates planning from execution, routes every effect through one safety envelope, and keeps the Rust workspace strictly layered.
---

# Infrastructure

flux is built around one boundary: **the LLM is not the runtime**. The model proposes a typed,
readable Flux-Lang plan; deterministic Rust code decides what is allowed and performs the work.

![flux architecture: the planner, deterministic runtime, safety envelope, guarded IO, providers, surfaces, three pillars, and strict crate layers](/img/architecture_v0.png)

The diagram combines two views. The arrows show how a turn executes, while the bottom strip shows
which workspace layers may depend on which others.

## How a turn moves through the system

1. A surface such as the CLI, TUI, SDK, or HTTP server assembles an agent and sends the request to
   the flow engine.
2. The model acts as a compiler front-end. Through a provider-neutral wire layer, it returns either
   prose or a typed Flux-Lang plan.
3. `flux-flow` analyzes and executes that plan. Every effectful plan node is dispatched through the
   same runtime gateway; the model never calls filesystem, process, or network APIs directly.
4. Results and evidence return to the session and become grounded feedback for the next pass through
   the loop.

## One safety envelope

Every operation—built-in tool, capability, plugin operation, or sub-agent call—crosses the same
non-bypassable chain:

```text
capability scope -> hooks -> authorization -> permission rules -> approval -> guarded IO
```

`flux-system` owns the guarded edge. It is the only production path used by built-in operations and
plugin host callbacks for real filesystem, process, and network IO. That keeps workspace
confinement, argv-vector process launching, network egress checks, approval, and secret redaction on
one auditable route. The opt-in `bash` operation deliberately runs `sh -c` through this path.

Plugin executables themselves are trusted native dependencies and are not OS-sandboxed by default.
Their manifest constrains host callbacks; it cannot constrain arbitrary syscalls from malicious
code unless opt-in OS-level sandboxing (`[sandbox]`) is enabled — see
[OS process sandboxing](./security/os-sandbox.md).

## Strict crate layers

The Cargo workspace is stratified from L0 contracts through L6 surfaces. A crate may depend only on
its own layer or a lower one, and `flux-codegate` enforces the rule in CI. The safety core stays small
and inner; user-facing surfaces, capabilities, and extensions cannot become alternate paths around
the runtime.

The same shared machinery supports flux's three co-equal pillars: the **Agent**, **Flux-Lang**, and
the **Improvement loop**.

## Go deeper

- [Concepts](./concepts.md) explains plans, symbols, evidence, and the agent loop.
- [Safety & approvals](./agent/safety.md) describes policy, approval, and guarded IO in detail.
- [OS process sandboxing](./security/os-sandbox.md) describes the opt-in bubblewrap/Seatbelt layer
  underneath the envelope.
- [Flux-Lang execution model](./language/execution-model.md) follows a plan from analysis through
  deterministic execution.
- The repository's [contributor architecture](https://github.com/codewandler/flux/blob/main/docs/architecture.md)
  contains the complete crate map and implementation invariants.

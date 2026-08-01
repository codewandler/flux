---
title: Infrastructure
description: How authored control flow, provider-native typed stages, action batches, and one safety envelope fit together.
---

# Infrastructure

flux is built around one boundary: **the LLM is not the runtime**. Provider-native typed stages
interpret intent and propose literal operation calls inside an authored Flux-Lang outer loop. The
host freezes effects into action batches; deterministic Rust code decides what is allowed and
performs the work.

![Flux runtime architecture: surfaces enter an authored Flux-Lang loop, provider-native typed stages propose literal calls, the host freezes an action batch, and approved calls cross authorization, approval, and guarded IO](/img/runtime-architecture.svg)

The arrows show ownership during one conversational turn. The bottom strip shows the workspace's
inward dependency direction, from user-facing surfaces to pure contracts.

## How a turn moves through the system

1. A surface such as the CLI, TUI, SDK, or HTTP server assembles a `FlowEngine` around the authored
   agent loop and sends it the request.
2. A typed intent stage narrows the live, wired, permitted operation catalog. Provider-native
   exploration then uses those operations' exact schemas: safe reads gather evidence through the
   executor, while effectful calls are captured as literal `{op, input}` data.
3. The host validates the captured inputs and freezes them into an immutable action batch. Approval
   binds a one-shot receipt to that exact batch and its caller, session, policy, and authority.
4. The executor consumes a matching receipt and sends each call through authorization, approval,
   and guarded IO. Its execution report returns to the same native ledger, where the authored loop
   can present the result, ask a question, or make a bounded correction.

The default conversational loop never asks the model to produce per-turn executable Flux. Models
supply bounded semantic judgment; authored Flux-Lang owns order, branches, iteration limits,
suspension, approval, and stopping. [`op.register`](./agent/saved-flows.md#register-an-operation-during-a-turn)
is the narrow exception for reusable vocabulary: it accepts exactly one agent-proposed composite
operation, then the host analyzes it against the live catalog, applies its requested scope, and
guards any persistent write. The registered operation's inner calls still cross the same envelope.

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

This shared machinery underpins the **Agent**, **Flux-Lang**, and SDK. The experimental Improvement
loop remains on hold and is documented under Direction rather than treated as a co-equal product
pillar.

## Go deeper

- [Concepts](./concepts.md) explains typed stages, symbols, evidence, and the authored agent loop.
- [Safety & approvals](./agent/safety.md) describes policy, approval, and guarded IO in detail.
- [OS process sandboxing](./security/os-sandbox.md) describes the opt-in bubblewrap/Seatbelt layer
  underneath the envelope.
- [Flux-Lang execution model](./language/execution-model.md) follows an authored flow from analysis through
  deterministic execution.
- The repository's [contributor architecture](https://github.com/codewandler/flux/blob/main/docs/architecture.md)
  contains the complete crate map and implementation invariants.

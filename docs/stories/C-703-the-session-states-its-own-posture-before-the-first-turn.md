---
id: C-703
title: "The session states its own posture before the first turn"
pillar: "Core"
status: backlog
priority: 2
epic: first-class-hosts
areas: [flux-runtime]
design: docs/designs/the-substrate-seam.md
note: "EnvContext renders exactly `Working directory` + `OS` (context.rs:193); the agent is told nothing about confinement, approvals, permissions, egress or which host its effects land on"
---

# The session states its own posture before the first turn

## Goal

The environment the agent is given at startup is two lines: working directory and OS
(`EnvContext`, `crates/flux-runtime/src/context.rs:193`). Everything that actually decides what a
turn can accomplish is invisible to it — whether it is confined, whether effects land on this
machine or a selected host, what the approval posture is, which permission deny rules apply, what
egress is admitted, which tools are disabled, and what the workspace roots are. So the agent plans
against an envelope it cannot see, and discovers the edges by being refused.

Stating the posture up front is cheap and it composes with C-702: that story stops offering what
cannot work, this one explains what *can*. Both must respect the same constraint the context
machinery already documents — assembly happens once at surface startup and sits in the
cache-stable prompt prefix, so a layer that varied within a session would invalidate it. That fits:
host selection is session-immutable by Decision 0018, the sandbox posture is fixed at startup, and
permissions come from configuration.

## Acceptance

- [ ] A posture context layer states, in plain language: where effects land (the native machine or
      the named binding, with its confinement), the workspace roots and any read-only additions,
      the approval posture, the permission and policy summary with deny rules named, the egress
      posture including whether private destinations are admitted, and any disabled tool families.
- [ ] The layer is session-stable by construction and contributes to the cache-stable prefix; a
      test asserts it does not vary between turns in one session.
- [ ] No secret material appears: credentials are named by reference exactly as every other surface
      renders them, and a test asserts no resolved value can reach the layer.
- [ ] The same summary is reachable by the operator on demand, so what the agent was told and what
      the human can inspect are the same text rather than two drifting descriptions.
- [ ] The layer stays short enough to be worth its tokens — it states the envelope, not the whole
      configuration file — and a test pins a bound on its rendered size.

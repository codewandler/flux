---
id: C-569
title: "Every agent start resolves and snapshots an explicit loop binding"
pillar: Core
status: ready
priority: 0
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-agent, flux-flow, flux-runtime, flux-orchestrate, flux-cli]
note: "general omission resolves to builtin adaptive; task/Fleet/backend starts carry a versioned binding and never inherit a parent's loop implicitly"
---

# Resolve behavior before starting the agent

## Goal

Make loop selection a required resolved field of the common agent-start contract so every top-level,
sub-agent, Fleet and served start says exactly which behavior harness it is running.

## Acceptance

- [ ] Failing first, a start-path census proves role/task and Fleet child constructors can currently
      reach a running engine with an independently defaulted loop and no durable loop identity. The
      fixed census covers CLI/SDK, roles through `task`, nested children, Fleet writers/reviewers/
      decision agents, app agents and served/A2A task starts.
- [ ] A resolved `AgentLoopBinding` carries logical profile/revision, runner kind, immutable source
      reference/digest, entry point and required runtime features. Start/status/terminal receipts
      expose bounded identity and digest metadata, never loop source or prompts.
- [ ] An ordinary omitted selector resolves to the explicit versioned adaptive preset before start.
      A sub-agent resolves its role/request policy and never implicitly copies the parent's loop or
      context. Fleet task roles require an explicit policy-selected binding.
- [ ] Missing profiles, changed digests, invalid source, missing operations and unsupported runtime
      features refuse before the first model call with the exact mismatch.
- [ ] Message, restart, resume, rework and recovery reconstruct the admitted binding. File/config/
      role changes affect new starts only; switching a live worker requires an explicit new
      admission/session transition.
- [ ] Capability and budget inheritance remain narrow-only and are not represented as context or
      loop inheritance. Existing role-specific authored loops and top-level `--loop` behavior remain
      compatible after resolution.
- [ ] Focused unit/conformance tests and the full gate pass.

## Progress

- (not started)

## Notes

- This is the prerequisite for C-567 and the stable identity C-543 displays and switches.

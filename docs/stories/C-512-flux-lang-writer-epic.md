---
id: C-512
title: "Flux-Lang writer — a specialist agent for checked authored automation (epic)"
pillar: Core
status: done
priority: 0
epic: flux-lang-writer
areas: [flux-agent, flux-lang, docs]
note: "EPIC — a capability-scoped repository role authors canonical .flux programs, validates them without executing effects as a shortcut, and stays discoverable through a source-linked agent catalogue"
---

# Flux-Lang writer — a specialist agent for checked authored automation

## Goal

Give Flux a named `flux-lang-writer` role that can turn an operator's automation request into
canonical, reviewable Flux-Lang while preserving the central boundary that authored control flow—not
the model transcript—is the runtime. The role must be easy to discover, source-auditable, and honest
about which checks prove syntax, analysis, and execution behavior.

## Acceptance

- [x] A tracked `.flux/agents/flux-lang-writer.md` role specializes in creating and editing
      workspace-relative `.flux` sources, reads the repository's Flux-Lang contract before acting,
      and keeps every write and validation command inside the parent's capability/policy floor.
- [x] The role validates syntax and analysis without running an effectful program merely as a
      checker; execution is performed only when the task asks for it and the normal authorization,
      approval, guarded-IO, and sandbox boundaries apply.
- [x] Public documentation inventories every embedded built-in role and every tracked project role,
      explains their authority source, and links each entry to its canonical GitHub source (C-513).
- [x] A deterministic census test fails when an embedded or tracked role is added, removed, renamed,
      or left without a source link in the public catalogue (C-513).
- [x] The catalogue distinguishes shipped embedded roles, tracked repository roles, and ignored
      local/user overrides; it never presents local scaffolding as code available from GitHub.

## Progress

- 2026-08-04: Filed at owner direction with C-513 as the immediate first delivery.
- 2026-08-04: Closed after C-513 delivered all five epic outcomes: the tracked capability-scoped
  writer, validation-versus-execution boundary, complete source-linked catalogue, deterministic
  census test and explicit local-override distinction.

## Notes

- The role narrows intent and instructions; its tool list does not replace Flux authorization,
  approval, sandboxing, or guarded IO.
- Flux-Lang syntax and generated mirrors remain governed by `crates/flux-lang/AGENTS.md`.

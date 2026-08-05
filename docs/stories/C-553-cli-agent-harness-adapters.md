---
id: C-553
title: "Codex, Claude, Hermes and Pi run through typed local task-agent adapters"
pillar: Core
status: backlog
epic: task-agent-backends
design: docs/designs/task-agent-backends.md
areas: [flux-orchestrate, flux-cli, flux-process]
note: "follow-up adapters — local harness CLIs are task backends, not special fleet coordinators"
---

# Codex, Claude, Hermes and Pi run through typed local task-agent adapters

## Goal

Let an admitted worker execute through an installed Codex, Claude, Hermes or Pi CLI while preserving
the generic task-agent lifecycle and fleet evidence rules.

## Acceptance

- [ ] Each adapter discovers executable/version/capabilities and refuses unsupported session or
      steering behavior before dispatch.
- [ ] Launch is argv-only with a closed environment, explicit cwd/worktree, model/config and bounded
      resources; credentials are resolved by the child harness rather than copied into receipts.
- [ ] Start, follow-up, resume, cancel and terminal outputs map to C-552 receipts with stable sessions.
- [ ] Temporary coordinator instructions/config are confined and deleted only under an explicit
      cleanup policy; durable audit records preserve hashes and provenance.
- [ ] Offline fake-binary conformance covers all four adapters; opt-in smoke tests cover installed
      real CLIs without becoming the repository gate.
- [ ] No adapter may push, publish or bypass fleet handoff verification.

## Notes

- Depends on C-552.

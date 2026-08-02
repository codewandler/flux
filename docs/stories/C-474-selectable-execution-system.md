---
id: C-474
title: "A selectable execution system — local remains default, a remote substrate can own a turn's effects"
pillar: Core
status: done
priority: 5
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [flux-system, flux-runtime, flux-tools, flux-flow, flux-cli]
note: "the shipped RemoteSystem cannot be installed today: ExecutionEnvironment, ToolContext and WorkspaceContext all store Arc<System>"
---

# A selectable execution system

## Goal

Let an operator select the guarded substrate used by a turn while keeping the native `System` as the
unchanged default. The model cannot select or replace the target.

## Acceptance

- [x] Failing-first: the same registered `read`, `write` and foreground-process operations execute
      against a loopback `RemoteSystem`, leaving a conflicting local marker untouched.
- [x] `flux-system` exposes an object-safe execution-system bundle spanning workspace files, host
      files, environment, process and network ports plus immutable substrate/workspace identity;
      native `System` and `RemoteSystem` implement it.
- [x] `ExecutionEnvironment`, `ToolContext` and `WorkspaceContext` carry the selected bundle instead
      of requiring `Arc<System>`; sub-agents inherit the active target snapshot.
- [x] User configuration, credential storage, provider/model calls, session storage and evidence
      storage remain local control-plane concerns. Workspace-derived project discovery uses the
      selected system.
- [x] Local-only operations are not allowed to fall back silently: they are either expressed by the
      port or refuse as unserved under a remote target.
- [x] No target option produces byte-for-byte current local behavior and the complete gate is green.

## Progress

- Filed after inspecting the production assembly: C-399 landed the remote port implementation, but
  `ToolContext::system()` and `ExecutionEnvironment::new` still require the concrete native type.
- 2026-08-02: `flux-system::port::ExecutionSystem` bundles the complete process, environment,
  host-file, workspace-file and network port families. `ToolContext` and `ExecutionEnvironment` accept an operator-installed override;
  absent an override, effects dynamically follow `WorkspaceContext`, preserving worktree enter/leave.
  The core file/search/edit and foreground-process operations use the selected substrate, and
  `core_coding_ops_use_the_selected_execution_system` proves conflicting local content is untouched.
  `SpawnRequest` carries the selected target into a child context.
- 2026-08-02 complete: the bundle now includes guarded network and opaque managed resources. The CLI
  exposes `--remote` without changing the local default, inherits it into sub-agents, and restricts
  remote sessions to port-aware operations so there is no native fallback path.

## Notes

- Depends on C-435 and C-473 so the bundle represents all guarded IO before a public remote flag exists.
- This is an operator topology choice. It does not weaken the connector rule that a connector's
  runtime kind is declared by its manifest and never chosen by model input.

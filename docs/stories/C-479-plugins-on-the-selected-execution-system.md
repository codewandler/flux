---
id: C-479
title: "Plugins on a selected execution system — keep the plugin local, route its guarded host callbacks deliberately"
pillar: Core
status: backlog
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [flux-plugin, flux-system, flux-cli, plugins]
note: "Docker/Kubernetes plugins are disabled under --remote today; the decision is where the trusted plugin process lives versus where each declared process/file/connection capability lands"
---

# Plugins on a selected execution system

## Goal

Allow a plugin operation to use the operator-selected execution system when every real effect it
requests is representable there, while keeping plugin startup, manifest verification, credentials,
and policy in the local control plane.

## Acceptance

- [ ] Failing-first: under a loopback `RemoteSystem`, a process-capability fixture records its argv
      only on the selected substrate; a conflicting local marker remains untouched.
- [ ] The trusted plugin binary still starts through the native guarded spawn with its cleared
      environment. Moving an operation's effects does not silently move plugin installation,
      verification, OAuth, config, or model/provider credentials.
- [ ] File, process, managed-process, and connection callbacks bind to the selected system only when
      their complete capability contract is remotely representable. Any missing callback marks that
      operation native-only through C-478 metadata; there is no partial local fallback.
- [ ] Unix-socket connections retain physical-path confinement on the executing host. A remote
      Docker call cannot authorize `/var/run/docker.sock` by validating a similarly named local
      path.
- [ ] Operation-bound secrets cross the encrypted link only when the approved remote callback needs
      them; they are never persisted in the delivery ledger or plugin diagnostics.
- [ ] Offline Docker fixture: `docker.info` reaches a fake Docker Unix socket on the selected system,
      never a local socket.
- [ ] Offline Kubernetes fixture: a read operation invokes a fake `kubectl` on the selected system,
      with kubeconfig/location semantics evaluated on that host rather than copied accidentally from
      the control machine.
- [ ] Plugin operations that remain native-only are hidden and refused with an operator-visible
      reason. Local mode behavior and capability narrowing are unchanged.
- [ ] Public Docker/Kubernetes docs list the first remote-capable operation set and remaining gaps.
- [ ] Full root and nested-plugin workspace gates green in both sandbox postures.

## Progress

- Filed 2026-08-02 from C-477. Blocked on C-478's placement vocabulary; the guarded system port and
  HTTPS/WSS remote resources already exist.

## Notes

- Depends on [C-478](C-478-explicit-operation-execution-placement.md).
- A plugin is trusted native code and remains a local control-plane dependency in this design. Only
  its declared, host-mediated effects move.

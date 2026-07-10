---
title: Plugin authoring
description: "Author a capability-scoped plugin with manifest declarations, security boundaries, and IO contracts."
---

# Plugin authoring

Plugins add operations as trusted native subprocesses that speak the flux plugin protocol. The host
launches each process with a cleared environment, projects its operations into the tool catalog, and
executes declared capability requests only when the manifest allows them. Plugins are not OS-sandboxed by default:
direct system calls remain possible, so third-party binaries must be reviewed like any
other native dependency. Opt-in [OS process sandboxing](../security/os-sandbox.md) closes that gap
underneath the manifest-gated capability model.

This public page is the short authoring contract. The full source-level guide is linked below.

## Authoring contract

- The host does privileged IO.
- Plugin capabilities are deny-by-default and manifest-scoped.
- Secrets are requested through declared secret purposes.
- Process and network access use host callbacks, not ambient environment access.
- Tool effects, risk, and idempotency must be declared honestly.
- Native code is trusted; the manifest governs host callbacks, not arbitrary syscalls.

## Full guide

The canonical in-repo authoring guide is
[plugins/AUTHORING.md](https://github.com/codewandler/flux/blob/main/plugins/AUTHORING.md).

Use that guide for lifecycle frames, manifest shape, host callbacks, and SDK examples. For the
security surface a manifest declares — the deny-by-default capability set and the `oauth2` block —
see [Plugin capability sandbox](../security/plugin-sandbox.md).

## Related docs

- [Using plugins](./using-plugins.md) — install, pin, inspect, and call plugins.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — manifest-scoped capabilities.
- [Plugin trust & signing](../security/plugin-trust.md) — supply-chain checks for installed binaries.
- [OS process sandboxing](../security/os-sandbox.md) — opt-in confinement of the raw plugin process.

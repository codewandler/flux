---
id: C-397
title: "A container backend for guarded process spawn"
pillar: Core
status: backlog
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "OWNERSHIP UNDECIDED — the port is unsealed so an out-of-repo consumer can implement this without flux changing, an in-repo backend costs a reviewed codegate allowance, and flux's own CLI has no use for it. Decide before promoting"
---

# A container backend for guarded process spawn

## Goal

Execute a guarded spawn inside a container or pod rather than as a child of this process, so a
deployment that must isolate per tenant can do so at the OS boundary instead of trusting one Rust
process to keep callers apart.

## Acceptance

- [ ] **Decide ownership first.** Does this belong in flux, or in the consumer that needs it? Record
      the decision in the design before writing code.
- [ ] If in flux: a `GuardedProcess` implementation (or `sandbox::Backend` variant) that spawns via a
      container runtime, with the same argv-only, env-cleared, output-capped guarantees.
- [ ] If in flux: a reviewed `flux-codegate` allowance, since the backend gate enumerates every
      in-repo implementation deliberately.

## Progress
- (not started — blocked on the ownership decision above)

## Notes
- Named in [ecosystem.md](../designs/ecosystem.md)'s runtime table. The multi-tenancy rule there is
  the motivation: a locally-executing runtime cannot be safely multi-tenant in one process, so
  isolation has to move to the OS or pod level.

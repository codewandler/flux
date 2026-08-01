---
id: C-399
title: "A remote implementation of the guarded-IO port"
pillar: Core
status: backlog
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "OWNERSHIP UNDECIDED, same as C-397 — port.rs already names 'a remote executor' as an intended substrate, which is an invitation to implementors rather than a commitment that flux ships one"
---

# A remote implementation of the guarded-IO port

## Goal

Serve the guarded-IO port by delegating to another substrate over a wire, so a caller can run
operations somewhere other than its own process while the guarantees stay stated in one place.

## Acceptance

- [ ] **Decide ownership first**, as in C-397.
- [ ] If in flux: a port implementation whose failure modes are distinguishable — a refused operation
      and an unreachable delegate must not collapse into one error, since an operator responds to
      them in opposite ways.
- [ ] If in flux: fail-closed on every optional operation the delegate does not serve.

## Progress
- (not started — blocked on the ownership decision above)

## Notes
- `crates/flux-system/src/port.rs` names *"a remote executor"* among the substrates the port exists
  for, and states the traits are unsealed and the in-repo gate stops at this repository.

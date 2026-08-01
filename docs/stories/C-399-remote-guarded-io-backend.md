---
id: C-399
title: "A remote implementation of the guarded-IO port"
pillar: Core
status: ready
priority: 10
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "OWNERSHIP DECIDED 2026-08-01: flux owns it, flux-exchange reuses it. flux must be able to do this locally as dev without depending on a service — that is the local-first principle, not a convenience"
---

# A remote implementation of the guarded-IO port

## Goal

Serve the guarded-IO port by delegating to another substrate over a wire, so a caller can run
operations somewhere other than its own process while the guarantees stay stated in one place.

## Acceptance

- [ ] a port implementation whose failure modes are distinguishable — a refused operation
      and an unreachable delegate must not collapse into one error, since an operator responds to
      them in opposite ways.
- [ ] fail-closed on every optional operation the delegate does not serve.

## Progress
- (not started)

## Notes
- `crates/flux-system/src/port.rs` names *"a remote executor"* among the substrates the port exists
  for, and states the traits are unsealed and the in-repo gate stops at this repository.

## The ownership decision (2026-08-01)

**flux owns this; flux-exchange reuses it.** The alternative — leaving it to whichever consumer needs
it first — would have put a locally-executing runtime behind a service, and flux must be able to do
this kind of work **on a developer's own machine without depending on a web app or a running
service**. That is `docs/vision.md`'s local-first principle applied to the runtime axis, not a
convenience: a capability that only exists when a platform is reachable is a capability the personal
coding agent does not have.

The consequence is accepted deliberately: an in-repo backend costs a reviewed `flux-codegate`
allowance, because that gate enumerates every in-repo implementation of the guarded-IO port on
purpose. Paying it here is the point — the alternative was an unreviewed implementation living
somewhere the gate cannot see.

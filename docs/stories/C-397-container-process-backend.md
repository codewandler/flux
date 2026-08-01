---
id: C-397
title: "A container backend for guarded process spawn"
pillar: Core
status: ready
priority: 9
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "OWNERSHIP DECIDED 2026-08-01: flux owns it, flux-exchange reuses it. flux must be able to do this locally as dev without depending on a service — that is the local-first principle, not a convenience"
---

# A container backend for guarded process spawn

## Goal

Execute a guarded spawn inside a container or pod rather than as a child of this process, so a
deployment that must isolate per tenant can do so at the OS boundary instead of trusting one Rust
process to keep callers apart.

## Acceptance

- [ ] a `GuardedProcess` implementation (or `sandbox::Backend` variant) that spawns via a
      container runtime, with the same argv-only, env-cleared, output-capped guarantees.
- [ ] a reviewed `flux-codegate` allowance, since the backend gate enumerates every
      in-repo implementation deliberately.

## Progress
- (not started)

## Notes
- Named in [ecosystem.md](../designs/ecosystem.md)'s runtime table. The multi-tenancy rule there is
  the motivation: a locally-executing runtime cannot be safely multi-tenant in one process, so
  isolation has to move to the OS or pod level.

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

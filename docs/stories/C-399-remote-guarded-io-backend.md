---
id: C-399
title: "A remote implementation of the guarded-IO port"
pillar: Core
status: in-progress
priority: 4
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "OWNERSHIP DECIDED 2026-08-01: flux owns it, flux-exchange reuses it. flux must be able to do this locally as dev without depending on a service — that is the local-first principle, not a convenience"
---

# A remote implementation of the guarded-IO port

## Goal

Serve the guarded-IO port by delegating to another substrate over a wire, so a caller can run
operations somewhere other than its own process while the guarantees stay stated in one place.

## Acceptance

- [x] a port implementation whose failure modes are distinguishable — a refused operation
      and an unreachable delegate must not collapse into one error, since an operator responds to
      them in opposite ways.
- [x] fail-closed on every optional operation the delegate does not serve.

## Progress

**Landed** as `crates/flux-system/src/remote.rs` — `RemoteSystem` serves all four port families by
handing each operation to a `Delegate`, plus `Loopback` for the in-process far side. Test:
`crates/flux-system/tests/remote_port_failure_modes.rs` (8 tests, an out-of-crate consumer on
purpose — a unit test inside the crate could pass while the seam stayed private).

**Three** failure modes, not two. The Acceptance names a refusal and an unreachable delegate;
implementing it surfaced a third that neither covers and that collapses just as misleadingly:
*unserved* — the delegate does not implement the operation at all. An operator retries an
unreachable link, fixes a refusal, and must **implement** an unserved operation; folding it into
either of the other two sends them somewhere useless. `FailureMode` therefore has three variants
and `failure_mode(&Error)` recovers which.

**The classification is structural, not textual.** A delegate returns `Answer::Refused` or
`Err(Unreachable)` — different positions in the type. Only a transport can construct `Unreachable`,
and the marker prefixes are written by `remote.rs`, so a delegate whose refusal reason *reads*
"the substrate is unreachable" still classifies as a refusal
(`a_delegates_wording_cannot_forge_the_other_failure_mode`). Without this, delegate-authored text
would be able to send an operator to investigate a healthy network.

**No wire format, deliberately.** `Delegate` is a Rust trait; no serialization, transport or
dependency was added to `flux-system` (its dependency set is still `flux-core` + `tokio` + `url`).
That keeps `docs/designs/remote-agents.md`'s open question — channel API or port delegation — open,
and keeps this story to the failure semantics its Acceptance is about.

**Local-first, verified.** `RemoteSystem::loopback` exercises the whole delegation path with nothing
running, and a `Loopback` never reports an unreachable link because there is no link to break.

**Two knock-on fixes in `port.rs`:** the unserved denial is now the public `port::UNSERVED` constant
(the distinction between "not offered" and "guard refused" lived only in that prefix, and
`remote.rs` needs to match on it), and `run_with_stdin`/`spawn_background` now build their denials
through `deny()` instead of hand-writing the same prefix — two literals that had already drifted out
of the one-spelling rule the constant exists to enforce.

**The reviewed cost was paid**, as the ownership decision said it would be: three entries in
`flux-codegate`'s `no_unreviewed_guarded_port_backend_outside_system` allow-list, with the review
rationale recorded beside them.

### Not done, and why

- **`GuardedEnv::env` cannot carry the distinction.** It returns `Option<String>`, so both failure
  modes fail the credential closed as `None` — right for the caller, useless for the operator.
  `RemoteSystem::env_checked` is the inherent escape hatch that keeps them apart; widening the
  `GuardedEnv` trait was not this story's to do.
- **`GuardedWorkspaceFiles` is still outside the gate's enumeration** (`port.rs` documents this
  gap). So `remote.rs`'s *fourth* impl — `GuardedWorkspaceFiles for RemoteSystem` — needed no
  allowance and would have landed unremarked. Closing that is still the two lines `port.rs` names,
  and is now one impl more worth closing than it was.
- **No network delegation.** There is no guarded-network port trait yet (C-435), so egress is absent
  from `Delegate` rather than approximated in it.

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

---
id: C-277
title: "Two unpinned couplings around child-process spawning: a stale `Confinement::Exempt` doc and a cross-crate readiness string"
pillar: Core
status: ready
priority: 7
epic: security-assurance
design: docs/designs/security-assurance.md
note: "both found during C-243's review round — a doc comment naming a call site that does not exist, and a worker-readiness marker that no test can pin from either side"
---

# Two unpinned couplings around child-process spawning

## Goal

Close two small, related defects found while reviewing C-243, both of the same shape: something states
a relationship that nothing verifies.

## Acceptance

**1. `Confinement::Exempt`'s doc names a call site that does not exist.**

- [ ] The doc comment at `crates/flux-system/src/sandbox.rs:892-897` describes the local-eval child
      flux host as using `Confinement::Exempt` *"because it needs provider network access"*. Verified:
      the variant has **no call site** outside `sandbox.rs`, and
      `flux-eval`'s `model_reachable_eval_runner_has_no_sandbox_exemption` (`runner.rs:515`) asserts the
      opposite. Either the doc is corrected, or the exemption is genuinely wired and the test updated —
      decide which, and record why.
- [ ] Whatever the outcome, a comment cannot be the only thing asserting a call site exists. If the
      variant is retained unused, say so in the doc; if it is removed, say what replaced it.
- [ ] ⚠ This one matters out of proportion to its size: the stale comment was cited as *precedent* in a
      rework instruction during this wave, and an implementor correctly refused to follow it. A doc that
      describes a design that was never built will be followed by someone eventually.

**2. Worker readiness is an unpinnable cross-crate string.**

- [ ] `crates/flux-orchestrate/src/worker.rs` decides a worker is live by matching
      `"listening on http://"` against the line printed by `flux_server` (`lib.rs:471`). The constant is
      private, and `flux-server` is L6 while the consumer is L3 — so **no test can pin the pair from
      either side**. Give it a pinnable form: a shared constant in a crate both can depend on, a real
      handshake (poll `/health` or the agent card), or a test that asserts the two strings agree.
- [ ] State the failure mode that motivates it: a rewording does not fail loudly — `fleet.start`
      degrades to a 60-second timeout and reports a worker that never announced itself, which reads as
      a slow or hung worker rather than a broken contract.
- [ ] Whichever fix is chosen, a rewording of the server's line must make a test fail.

## Progress

- (not started)

## Notes

- Both found by the independent review of **C-243** (`impl/C-243`), which judged neither blocking on
  its own. The readiness coupling was explicitly deferred by the coordinator in favour of a story rather
  than a rework round, since it cannot be pinned from either side without a structural change.
- The two are filed together because they are the same failure class — an unverified claim about how
  child-process spawning works — and both live in the blast radius of the same code.
- A real handshake would also fix a second-order problem: the current marker proves the server *printed*
  a line, not that it is *accepting connections*, which C-243's netns finding showed are different
  things.

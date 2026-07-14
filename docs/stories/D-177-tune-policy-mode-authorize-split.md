---
id: D-177
title: Tune policy mode — authorize-only split
pillar: Agent
status: backlog
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
note: "Phase 3b — deferred; the hardest surgery; depends on D-176"
---

# Tune policy mode — authorize-only split

## Goal
Let `Session::what_if().policy(perms)` re-authorize a recorded run under a different permission
policy against the frozen world, so a stricter policy's DENY surfaces as a real divergence — the
"would the tightened policy have blocked the destructive action?" gate.

## Acceptance
- [ ] `WhatIf::policy(perms)` rebuilds the executor from the new `Permissions` and re-runs the target
      turn with the `Frozen` scope.
- [ ] Failing-first: under a stricter policy, an op the original ran is DENIED — the denial surfaces
      as a divergence/`DiffRow`, not masked by the taped output; under a looser/equal policy the run
      stays hermetic and serves from tape.
- [ ] The authorize decision is a pure function of `(op, subjects, Permissions)` with **no** execution
      side effect, and does **not** open a bypass in the one non-bypassable envelope (adversarial test).

## Progress
- (not started — epic deferred; docs-only for now)

## Notes
- The hard part: `Executor::dispatch_outcome` currently bundles authorize + execute. This story adds
  an authorize-*only* entry so policy mode can decide DENY/ALLOW without executing, then serve the
  frozen tape on ALLOW. Isolate and test heavily — this touches the safety-critical envelope.
- Split out of D-176 deliberately so the rest of Tune ships without blocking on it.

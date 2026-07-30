---
id: C-273
title: "Embedder resource limits — fuel, memory ceiling, wall-clock deadline"
pillar: Core
status: blocked
priority: 6
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "Wasm bounds authority, NOT resources — an unbounded loop or allocation takes the embedder down unless the host caps it; this is not inherited from the sandbox"
---

# Embedder resource limits — fuel, memory ceiling, wall-clock deadline

## Goal

A Wasm module cannot reach anything it was not granted, but it can spin forever and allocate until the
embedder dies. Executing submitted code therefore requires the embedder to bound CPU, memory and
wall-clock time. This is not a property of the sandbox and must be built.

## Acceptance

- [ ] An embedded run is bounded on all three axes: instruction/fuel or epoch interruption, a memory
      ceiling, and a wall-clock deadline.
- [ ] Failing-first tests with **adversarial** modules: an infinite loop, an allocation bomb, and a
      program that blocks on a host import. Each must hit its limit and be terminated with a
      diagnosable error, rather than hanging or taking the embedder with it.
- [ ] Hitting a limit is reported as a distinguishable outcome, not as a generic failure — an operator
      must be able to tell "the program exceeded its budget" from "the program failed".
- [ ] Limits are configurable per run with documented defaults, and the defaults are defensible for
      untrusted input rather than merely generous.
- [ ] Partial effects are accounted for: a run killed mid-flight may already have caused host-side
      effects, and the story states what the caller can assume.

## Progress

- (blocked on C-271)

## Notes

- The three-axis framing matters: fuel alone does not stop an allocation bomb, and a memory cap alone
  does not stop a spin loop.
- Relevant precedent for "bounded, and honest when the bound is hit": C-261's daemon admission and
  completed-usage circuit breakers, and C-260's bounded REST SSE lifecycle.

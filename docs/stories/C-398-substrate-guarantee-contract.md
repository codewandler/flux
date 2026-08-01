---
id: C-398
title: "Say what binding flux-system without flux-runtime means"
pillar: Core
status: ready
priority: 7
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "AGENTS.md says every tool runs through Executor::dispatch — true of FLUX, and a reader who finds an out-of-repo consumer bypassing it will reasonably conclude something is broken. Nothing states which guarantees travel with the substrate alone"
---

# Say what binding flux-system without flux-runtime means

## Goal

A consumer may link `flux-system` and bring its own policy engine — that is the point of a published
substrate with an unsealed port. Nothing in the tree says which guarantees such a consumer gets and
which it does not, so the safety story is currently only legible to someone who has read both crates.

## Acceptance

- [ ] A contract document (crate-level docs on `flux-system`, and a section reachable from
      `docs/concepts.md`) lists, explicitly and separately:
      - guarantees that travel with `flux-system` alone — path confinement, argv-only execution,
        egress resolution and range blocking, sandbox confinement, env clearing, output capping;
      - guarantees that are `flux-runtime`'s and **do not** travel — default-deny authorization,
        approval, redaction of tool output, evidence.
- [ ] It states that a consumer taking only the first set is **supported**, and that assuming the
      second is the failure the document exists to prevent.
- [ ] `AGENTS.md`'s *"Every tool runs through `Executor::dispatch`"* is scoped to flux explicitly, so
      it can no longer be read as a claim about every consumer of the substrate.
- [ ] The existing `port.rs` answer (what it means to *implement* the port) is cross-linked and not
      duplicated — these are two different questions and both should stay answered once.

## Progress
- (not started)

## Notes
- Docs-only; no behavioural change. The gate for this story is review, not a test.
- `docs/concepts.md` already carries the peer framing publicly; this is its crate-level companion.

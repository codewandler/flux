---
id: C-364
title: Teach the direct-I/O gate about ureq and resolve the live un-waived hit
pillar: Core
status: backlog
epic: structural-gate-blind-spots
design: docs/designs/structural-gate-blind-spots.md
note: "PROVEN LIVE MISS — crates/flux-capabilities/src/datasource/embeddings.rs:130 is an un-waived outbound ureq POST inside a scanned model-facing crate, and the gate passes today"
---

# Teach the direct-I/O gate about `ureq` and resolve the live un-waived hit

## Goal

Close the one demonstrated, in-tree miss of the direct-I/O gate — not a hypothetical mutation, an
actual call the invariant claims cannot exist.

## Acceptance

- [ ] `classify_direct_io` (`crates/flux-codegate/src/lib.rs:587-628`) classifies `ureq` outbound
      calls.
- [ ] The resulting hit at `crates/flux-capabilities/src/datasource/embeddings.rs:130` is resolved:
      routed through the guarded egress path, or waived with a reason that states why the existing
      `guard_url` call is sufficient there.
- [ ] Failing-first: the gate reds on the current tree before the call is resolved.
- [ ] Every HTTP-capable dependency of a scanned crate is enumerated once, so the next client crate
      added to a model-facing manifest is not invisible by default — this is the general form and it
      belongs to C-366.

## Progress

- 2026-08-01 — the miss was confirmed by running the gate during validation: it passes with the call
  present.

## Notes

- The call is benign today: it does pass `flux_system::net::guard_url`. The defect is that the
  invariant "every direct I/O in a model-facing crate is absent or reasoned" is not enforced on it.

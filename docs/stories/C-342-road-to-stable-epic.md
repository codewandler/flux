---
id: C-342
title: "The road to stable — what must be true before flux is measured rather than built (epic)"
pillar: Core
status: ready
priority: 2
epic: road-to-stable
design: docs/designs/road-to-stable.md
note: "STABILITY EPIC — ~16 of 110 open stories block a credible stable claim; the other ~94 are capability stability does not depend on. The real blocker is not the bug count but the published API surface: C-337 records a scheduled breaking window and carries zero implementation stories"
---

# The road to stable

## Goal

Name the work that stands between flux and being **measured rather than built** — the switch to
`flux-bench` harness runs driving improvement — so the distinction between "blocks stable" and "is
merely unbuilt" stops being re-derived every time someone asks.

Full argument and evidence: [`docs/designs/road-to-stable.md`](../designs/road-to-stable.md).

The short version, from the 2026-07-31 backlog analysis:

- **~16 of 110 open stories block stability.** The other ~94 are capability it does not depend on.
- **The backlog has flipped to discovery-driven** — 54% of stories C-301…340 originate from a review
  or an implementor report, against ~1% for C-1…200. **Zero of the 20 newest stories are new
  capability.**
- **17 of 85 non-epic open stories are defects a user can hit**, clustered in three places: webhook
  delivery, the grammar and its editor mirrors, and redaction. **Two redaction stories fail open.**
- **The architecture is settled; the published API is not.** C-337 says "preserve, do not redesign"
  about the architecture, while recording a scheduled breaking window for `AgentSpec` — and carries
  no implementation stories.

## Acceptance

Each item is checkable rather than a judgement. The full list with reasoning is in the design doc.

- [ ] No open correctness story **fails open** — C-339. (C-323 ✅ 2026-07-31.)
- [ ] No open priority-1/2 correctness story — C-340.
- [ ] Editor mirrors guarded, non-vacuously — C-340, C-336. (C-334 ✅.)
- [ ] Every channel delivery authenticated and distinguishable from an unverified one — C-291, C-292,
      C-295, D-217, **and all four prioritized rather than left in `backlog`**.
- [ ] An unattended run survives a provider transport failure — C-227, C-228 (epic C-229). This is a
      *precondition* for harness-driven work, since harness runs are unattended by definition.
- [ ] No wiring line without an observing test — C-313, C-314, C-332. (C-328 ✅ shipped the census.)
- [ ] The RUSTSEC advisory ignore is dropped — C-205.
- [ ] Vendor-host egress holds when flux is not the one dialing — C-311.
- [ ] **C-337 is decomposed into stories and its breaking window is scheduled.** Until then "stable"
      can mean "the runtime does the right thing" but not "the API you build against will not change."
- [ ] **C-255's final bullet is ticked** — three fresh independent reviews against the exact resulting
      working tree find no reproducible High-severity containment defect.

## Notes

- ⚠ **This epic does not claim the list is complete.** Two clusters are mid-cascade — C-315 → C-323 →
  C-338/C-339 and C-301 → C-334 → C-340 each found *more* than their parent. That is exactly why the
  last acceptance item is C-255's "three fresh reviews" bullet rather than this story's own checklist:
  a checklist can be completed, an adversarial review can only be *passed*.
- ⚠ **The webhook cluster is unprioritized and unstarted**, and it is the only cluster on an
  internet-facing port. Deciding its priority is the first thing this epic should force.
- ⚠ `areas:` under-reports the defect map — only 62 of 110 open stories carry it, and 7 of the 20
  correctness stories have none, including the entire webhook cluster. Any analysis keyed on `areas:`
  will understate the cluster that matters most.
- Open question recorded rather than answered: does "stable" mean **1.0**? flux uses the minor
  position as the breaking signal pre-1.0. If stable means 1.0, the API-surface work is not optional
  and the `AgentSpec` window must land first.
- Referenced epics, deliberately **not** members (an epic is not nested in another):
  [C-255](C-255-adversarial-review-remediation-epic.md),
  [C-337](C-337-architectural-simplification-epic.md),
  [C-229](C-229-unattended-run-integrity-epic.md).

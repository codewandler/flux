---
id: C-446
title: "Windows has no native sandbox backend — so the OS isolation boundary is not everywhere"
pillar: Core
status: ready
priority: 7
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-system, docs]
note: "the review's sharpest one-line summary of F2: `Flux has a mandatory policy/guarded-IO boundary everywhere, but not a mandatory OS isolation boundary everywhere`. Either build the backend or say plainly what a Windows deployment does not get"
---

# The boundary that stops at the platform edge

## Goal

Close the Windows gap or state it, so nobody deploys flux on Windows believing they have the isolation
the docs describe.

## The finding

> *"Windows has no native backend in the reviewed tree. Thus Flux has a mandatory policy/guarded-IO
> boundary everywhere, but not a mandatory OS isolation boundary everywhere."*

D-134…D-137 shipped bubblewrap (Linux) and Seatbelt (macOS) with *graceful-Windows*. Graceful means it
does not break; it does not mean it confines.

## Acceptance

- [ ] A decision, recorded: build a Windows backend, or declare Windows out of scope for OS isolation.
- [ ] ⚠ **Whichever way, the docs state what a Windows deployment gets and does not get** — the policy
      and guarded-IO envelope still apply; the OS sandbox does not. An operator who reads *"defense in
      depth via an OS sandbox"* and runs on Windows currently has a belief the tree does not support.
- [ ] The review's own deployment note is honoured: *"Poor fit: … Windows workloads requiring
      Flux-provided OS isolation."* If that stays true, it belongs in the docs, not only in a review.
- [ ] If a backend is built, it meets the same single-spawn-choke-point discipline as the other two —
      ⚠ not a second confinement path.
- [ ] Full gate green.

## Notes

- Feeds [C-440](C-440-the-topologies-page.md): "what do I get on this platform" is a topology question
  and that page will want the row.
- ⚠ Windows containment primitives (job objects, AppContainer, restricted tokens) are not equivalents of
  bubblewrap; a partial backend that *looks* like the others but confines less would be worse than none.

## Progress
- Filed 2026-08-02 from the Pi comparison.

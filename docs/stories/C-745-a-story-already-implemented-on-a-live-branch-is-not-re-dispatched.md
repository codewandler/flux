---
id: C-745
title: "A story already implemented on a live branch is not re-dispatched"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
---

# A story already implemented on a live branch is not re-dispatched

## Goal

A branch audit found `C-562` implemented **independently four times** — waves 257, 281, 286 and 308,
carrying 827, 1121, 1129 and 1004 unique lines of `board_fleet_cmd.rs` — and `C-569` twice more on
waves 299 and 302. The fleet re-dispatched stories that were already implemented on a live branch and
discarded the losers: roughly 5,000 lines of model output, produced and thrown away.

C-723 stopped the driver withholding on an unverified `already-built` signal. This is the opposite
error: dispatching work that demonstrably exists, because nothing looks at the branches.

## Acceptance

- [ ] Before dispatching an item, the driver checks whether a live branch already holds an
      implementation of it, and reports what it found either way.
- [ ] The check is evidence-based, not a name match. C-723's lesson applies exactly: a branch whose
      name contains the story id proves nothing, and `wave-745/story/C-575` held a genuine
      implementation while `wave-472` branches held superseded ones.
- [ ] Finding one does not silently withhold. It dispatches with the prior attempt named, or
      withholds with the branch cited — the operator must be able to see which and why.
- [ ] A superseded attempt is distinguishable from a live one, so the check does not resurrect work
      that main has already moved past.
- [ ] Regression test: two waves dispatched for one story produce a report naming the first
      attempt's branch rather than two independent implementations.

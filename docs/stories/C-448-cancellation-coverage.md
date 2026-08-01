---
id: C-448
title: "Audit cancellation — Pi's reaches retries, compaction and bash; whether flux's does is unknown"
pillar: Core
status: ready
priority: 6
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-flow, flux-runtime]
note: "Pi strength, flux unaudited. The review credits Pi with cancellation reaching `the high-level agent and session-owned work, including retries, compaction and bash`, and scores both 9.0 on that axis — so this is about confirming flux's coverage, not assuming a deficit"
---

# Where does cancel actually reach?

## Goal

Establish what cancelling a flux turn actually stops, and close whatever it does not.

## Why

The review credits Pi with cancellation that *"reaches the high-level agent and session-owned work,
including retries, compaction and bash"* — and scores both harnesses **9.0** on the
providers/context/sessions/cancellation axis, crediting flux with *"unusually explicit
history/cancellation invariants."*

⚠ So this is not a stated deficit. It is an **unaudited claim on our side**, and the three places Pi
names are exactly the ones that are easy to miss: a retry loop that keeps retrying, a compaction that
keeps running, and a spawned process that keeps executing after the user pressed cancel.

## Acceptance

- [ ] A map of what cancellation reaches today, per surface: an in-flight model call · a retry/backoff
      loop · compaction · a spawned process · a sub-agent turn · a journey.
- [ ] **Failing-first** for each gap found: a test asserting the work stops, failing at the merge base.
- [ ] ⚠ **A spawned process is the one with a real-world cost.** A cancelled turn whose `bash` keeps
      running is an effect continuing after the operator said stop — closer to a safety issue than an
      ergonomics one.
- [ ] What cancellation deliberately does *not* stop is stated, with the reason. ⚠ Pairs with
      [A-141](A-141-what-pause-means-for-an-effect-in-flight.md), which is the same question for pause:
      an effect already in flight cannot be un-sent, and claiming otherwise is worse than the gap.
- [ ] Full gate green.

## Notes

- ⚠ Do not build a second cancellation mechanism. Find the existing one and extend its reach.
- The run-control epic ([A-140](A-140-pause-a-live-run.md)) is adjacent: pause and cancel share the
  question of what "stop" means at a boundary, and should share the answer.

## Progress
- Filed 2026-08-02 from the Pi comparison.

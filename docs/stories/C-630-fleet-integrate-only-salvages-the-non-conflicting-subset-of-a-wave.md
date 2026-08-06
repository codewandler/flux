---
id: C-630
title: "fleet integrate --only salvages the non-conflicting subset of a wave"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "wave-346 parked whole while two of three stories were independently integrable"
---

# fleet integrate --only salvages the non-conflicting subset of a wave

## Goal

Integration is all-or-nothing per wave: wave-346 parked whole on one conflicting pair while two
of its three stories were independently integrable, and the driver's only remedy was a terminal
park. With conflicts now decided by a real cherry-pick trial, the natural salvage is to integrate
the subset that combines and report exactly what was left out and why.

## Acceptance

- [ ] `fleet integrate --only <item>...` integrates the named subset of a wave's accepted handoffs, runs the single gate on that candidate, and leaves the rest untouched and re-integrable.
- [ ] The wave records which items were deferred and the recorded conflict evidence for each.
- [ ] A wave-346-shaped fixture (A+B combine, C conflicts) lands A+B as a candidate and leaves C accepted-but-uncombined.

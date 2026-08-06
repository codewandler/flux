---
id: C-631
title: "flux fleet drive replaces the bash autopilot"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "the 582-line bash driver parses state three ways without --if-revision and cannot self-edit safely; its loops (planner/retro/scribe) are authored .flux files it invokes host-side, so the port inherits them unchanged"
---

# flux fleet drive replaces the bash autopilot

## Goal

The unattended driver is a 582-line bash script that parses `state.json` three ways with no
`--if-revision` guard, cannot be edited while running (bash re-reads by byte offset), needed a
production failure to find each of its ~17 defects, and holds the only closed control loop in the
system. Its judgment is already outside it — planner/retro/scribe are authored `.flux` loops it
invokes host-side — so a native `flux fleet drive` inherits them unchanged and replaces only tick
mechanics: status fingerprinting, wave-state advancement, handoff reconstruction, snapshot
accumulation, dispatch width.

## Acceptance

- [ ] `flux fleet drive --tick` performs one deterministic tick (report, advance, accumulate, dispatch) and `--loop` runs it on an interval under a single-instance guard.
- [ ] All state reads go through the native store with revision guards; no state.json text parsing.
- [ ] Park/claim semantics implement the roadmap R-10..R-13 contracts (handoff-before-park, reasoned parks, claim release, budget reset on unpark).
- [ ] The bash autopilot is deleted from the roadmap repository once drive reaches parity, and AUTOPILOT.md documents the native command instead.

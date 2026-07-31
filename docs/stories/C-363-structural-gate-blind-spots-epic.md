---
id: C-363
title: "Structural gate blind spots — eleven mutations that pass every source gate today (epic)"
pillar: Core
status: backlog
epic: structural-gate-blind-spots
design: docs/designs/structural-gate-blind-spots.md
note: "EPIC — ASSURE-02/03's filed holes are closed, but mutation-testing found eleven survivors including a LIVE un-waived ureq POST in a scanned crate and a whole second production catalog (flux-app) that no census covers"
---

# Structural gate blind spots

## Goal

Make each source gate reject the representative bad change a competent author would actually write,
rather than the fixture the gate's own author wrote.

## Acceptance

- [ ] C-364 teaches the direct-I/O gate about `ureq` and resolves the live un-waived hit.
- [ ] C-365 closes `const`/`static`/field alias capture and records the repo-wide macro-body blind spot.
- [ ] C-366 derives the model-facing crate set from production registration and caps waivers.
- [ ] C-367 extends the catalog census to the `flux-app` assembly and the infallible register family.
- [ ] C-368 requires every catalog op to publish a risk tier in a risk-bearing table.
- [ ] Every story lands its representative mutation first and proves the gate reds on it.

## Progress

- 2026-08-01 — opened from the mutation table in the design doc.

## Notes

- This is the repo's recorded "guards tested against their own assumptions" pattern, now with a
  measured instance count.
- AGENTS.md calls the pin census "the one that catches wiring no test observes"; it covers exactly
  one setter on two builder roots (C-330 widens it). Worth correcting the framing while here.

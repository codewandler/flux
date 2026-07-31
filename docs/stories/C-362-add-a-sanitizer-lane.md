---
id: C-362
title: Add a sanitizer lane over the unsafe-adjacent seams
pillar: Core
status: backlog
epic: assurance-lane-residuals
design: docs/designs/assurance-lane-residuals.md
note: "no -Zsanitizer flags anywhere in the repo; ASan/TSan/MSan is the other limb of ASURE-01 that no addition has touched"
---

# Add a sanitizer lane over the unsafe-adjacent seams

## Goal

Cover the failure classes Miri's two pure seams cannot reach — concurrency and memory behaviour in
the crates that actually hold locks, channels and FFI-adjacent code.

## Acceptance

- [ ] An ASan (and where feasible TSan) job runs the test suite for the crates with real
      concurrency: the event store, the flow engine's turn gate and spawn supervisor, and the
      server's resource governor.
- [ ] Crates or tests that cannot run under a sanitizer are listed explicitly with the reason,
      machine-checked the way the `FLUX_MIRI_UNSUPPORTED` inventory already is.
- [ ] The lane records a real execution before the story closes.

## Progress

- 2026-08-01 — filed from the ASSURE-01 lane split.

## Notes

- Lower priority than C-359/C-360: those make existing lanes honest, this adds a new one.

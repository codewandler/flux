---
id: C-367
title: Extend the catalog census to the flux-app assembly and the infallible register family
pillar: Core
status: backlog
epic: structural-gate-blind-spots
design: docs/designs/structural-gate-blind-spots.md
note: "LIVE CONSEQUENCE — flux app run assembles a SECOND production catalog; emit/send/ask/spawn are registered there and are checked by no metadata-coherence gate and no risk gate. The seam scan is rooted at flux-cli/src only"
---

# Extend the catalog census to the `flux-app` assembly and the infallible register family

## Goal

Make "the production catalog" mean every catalog the product assembles, and make the registration
census see every way an op can be registered.

## Acceptance

- [ ] `production_catalog()` (`crates/flux-cli/src/catalog_coherence.rs:93-198`) covers the
      `flux-app` assembly (`crates/flux-app/src/app.rs:872-897`), or an equivalent census exists
      there; `emit`, `send`, `ask` and `spawn` are subject to `metadata_violations` and the risk gate.
- [ ] The registration visitor records the infallible family — `register`, `try_extend`, and the
      `register_*` wrappers (`flux_tools::register_builtins`, `flux_web::register_web`,
      `CognitionPack::register`) — not only idents prefixed `try_register`
      (`catalog_coherence.rs:730-732`).
- [ ] `EXCLUDED_REGISTRATION_SOURCES` no longer keys on the rendered token `source`, so naming a
      variable `source` cannot buy silence (`:640-644`, `:684`).
- [ ] The reused-label hole (`:945-947`) is either closed or its residual restated with the new
      scope.
- [ ] Failing-first fixtures for each: an op registered from `flux-app`, an op registered via
      `registry.register`, and a pack registered through a variable named `source`.

## Progress

- 2026-08-01 — mutations 5, 6, 7 from the design doc's table; mutation 7 has a live consequence.

## Notes

- `crates/flux-cli/tests/website_contract.rs:557-574` carries the App/pane/fleet op names as
  hard-coded literals rather than reading a registry — same root cause, fixed by the same widening.

---
id: C-508
title: "Adopt the cross-repository Exchange-only integration program"
pillar: Core
status: done
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "Decision 0001 now governs the program: Exchange executes every official integration; Flux embeds one client and ships no local official fallback"
---

# Adopt the cross-repository Exchange-only integration program

## Goal

Make the cross-repository roadmap the scheduling authority for Flux's official-integration migration
and reconcile the local implementation contracts with its Exchange-only execution decision before
runtime work begins.

## Acceptance

- [x] Root `AGENTS.md` sends cross-repository program work to the sibling `flux-roadmap` schedule
      while preserving repo-local Goal and Acceptance as the implementation contract.
- [x] C-500…C-506 no longer require, permit, or test a local official-integration executor or plugin
      fallback; the rejected C-502 implementation is closed as superseded rather than built.
- [x] C-503 is the Milestone 1 embedded Service Account/effective-catalogue/one-shot HTTP client only;
      subscribe, streamed output, cancellation, and leases remain explicitly outside it for the
      later lifecycle milestone.
- [x] The ecosystem design records the authoritative execution path, ownership boundary, migration
      ratchet, and final zero-plugin Flux release consequence without claiming implementation shipped.
- [x] The engineering changelog records the contract correction and the generated story board agrees
      with frontmatter.

## Progress

- 2026-08-03: Completed the contract reconciliation from flux-roadmap Decision 0001. No runtime,
  release, or public capability changed in this documentation-only story.

## Notes

- Cross-repository source: `../flux-roadmap/decisions/0001-exchange-executes-official-integrations.md`.
- Documentation-only contract correction; no failing-first behavioral test applies.

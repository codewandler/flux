---
id: C-492
title: "Reserve Agent for the runtime across the family vocabulary"
pillar: Core
status: done
design: docs/designs/ecosystem.md
epic: docs-completeness
areas: [docs]
note: "Exchange's legacy bearer principal collides with Flux Agent; define App, Event, qualified providers, Managed Agent and Service Account once"
---

# Reserve Agent for the runtime across the family vocabulary

## Goal

Keep Agent meaning model + loop + bounded capabilities across Flux, and give Exchange's API bearer
principal the distinct Service Account name while completing the App/event/provider vocabulary.

## Acceptance

- [x] `docs/concepts.md` defines App, Event Type and Event Delivery and qualifies Model Provider and
      Identity Provider without inventing a second runtime.
- [x] The ecosystem design calls the non-human bearer principal Service Account and reserves Managed
      Agent for an Exchange-hosted Flux Agent.
- [x] The generated public Concepts mirror is current and `website_in_sync` passes.

## Progress

- 2026-08-03: Raised from the Flux Exchange released-domain audit against Flux 0.52.1 and
  flux-connectors 0.16.0.
- 2026-08-03: Contributor concepts and ecosystem pages now use the shared vocabulary; the public
  mirrors and customer changelog were regenerated. All five `website_in_sync` tests and all 32
  `website_contract` tests pass.

## Notes

- This is vocabulary alignment, not a claim that Exchange already hosts Apps.

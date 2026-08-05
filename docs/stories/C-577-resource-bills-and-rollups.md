---
id: C-577
title: "Expose resource bills and rollups from result through Fleet"
pillar: Core
status: backlog
epic: resource-accounting
design: docs/designs/resource-accounting.md
areas: [flux-cli, flux-events, flux-board, flux-tui]
depends_on: [C-520, C-576]
note: "bounded bills by request/story/epic/worker/wave/repository/Fleet with physical usage, money basis, coverage and Board evidence links"
---

# Make the cost of a result visible

## Goal

Give humans and automation one bounded resource bill for a result or story and truthful rollups to
epics, workers, waves, repositories and Fleet.

## Acceptance

- [ ] A versioned JSON projection queries by root request/result, BoardRef/story, epic at revision,
      worker/session, wave, repository, Fleet, loop phase and time range. Human output is a rendering
      of the same projection.
- [ ] Every bill shows physical measurement totals, exclusive/inclusive/allocated columns, reported
      charges, estimates, unpriced coverage, unsupported dimensions, source/freshness and pricing/
      allocation basis. Unknown never renders as zero or disappears from denominators.
- [ ] A completed Fleet story records one bounded Board evidence link with receipt id/digest,
      coverage and compact direct/inclusive totals. Canonical receipts remain in the ledger; story
      frontmatter/prose is not rewritten as a metrics database.
- [ ] Story-to-epic and story-to-wave rollups use explicit revisions and count every receipt once.
      Five-story/three-repository and shared-overhead fixtures reconcile from leaves to Fleet total.
- [ ] C-518/C-520 model-token/cost views can consume the bill projection while retaining foreign
      harness partial coverage; C-571/C-573 consume the same bounded scope projections for limits and
      adaptation.
- [ ] Large histories use indexed bounded reads, deterministic ordering and omission metadata.
      Billing views load no transcript or repository content merely to render usage.
- [ ] Public docs explain “reported”, “estimated”, “unpriced”, physical usage without a money rate,
      exclusive/inclusive/allocated cost and why a bill is not an invoice.

## Progress

- (not started)

## Notes

- The exact CLI spelling is deliberately left to the public machine-CLI design; the JSON schema and
  accounting semantics land before another TUI view.

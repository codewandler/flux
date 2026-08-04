---
id: C-514
title: "Adopt the Decision 0006 datasource vocabulary"
pillar: Core
status: done
note: "Decision 0006 now governs datasources: one declared read-only definition, two access modes, connector-owned definitions, Exchange-owned tenant bindings, flux-owned vocabulary/registry/declaration — and the board leaves the datasource vocabulary"
---

# Adopt the Decision 0006 datasource vocabulary

## Goal

Reconcile flux's concepts, designs, stories and public docs with flux-roadmap Decision 0006
(*Datasources are declared read surfaces*) before any datasource or board implementation work
dispatches against the old, four-meanings-under-one-name vocabulary.

## Acceptance

- [x] `docs/ecosystem.md` and `docs/concepts.md` state the family definition — a datasource is a
      named, declared, read-only record surface (*operations do; datasources know*) with exactly one
      access mode (indexed or live) — and the ownership split: Datasource Definitions belong to the
      connector package, tenant bindings and the governed read seam to Exchange, and the wire
      vocabulary, the one registry across both modes, and the Flux-Lang declaration surface to flux.
      Both website mirrors are regenerated in the same change.
- [x] The board leaves the datasource vocabulary in those pages: it is documented as a first-class
      write-capable surface direction, not a third datasource mode.
- [x] `docs/designs/async-live-datasource-seam.md` amends its "plugin-protocol live-datasource
      capability" non-goal: the open door's named consumers are flux-connectors (the compiled-in
      catalogue, indexed mode) and flux-exchange (the tenant Datasource read seam, 0006 rules 7–8),
      not the plugin protocol Milestone 5 removes.
- [x] `docs/designs/datasource-discoverability.md` carries a disposition note: its registry
      direction resumes under 0006 rule 2 — one registry enumerating declared datasources across
      indexed and live modes — and is scoped accordingly (D-250).
- [x] `docs/designs/connector-backed-storage-facade.md` carries a supersession banner: superseded in
      part by flux-roadmap Decisions 0001–0005 and 0006; any revival is re-derived under the
      declared-surface pattern.
- [x] `website/docs/agent/datasources.md` and `website/docs/sdk/datasources.md` use the 0006
      vocabulary (declared read-only definition, two access modes, the board split as direction)
      without describing any unshipped feature as shipped; current behavior documentation is intact
      and the board pages still enumerate all eleven generated board operations
      (`crates/flux-cli/tests/website_contract.rs`).
- [x] The follow-up work is filed rather than implied: the first-class-board epic (A-148, L-130,
      re-pointed A-134/A-115/A-118), the one-registry story (D-250), the authority-subject grammar
      story (D-251), the DatasourceDecl kind extension (D-252), the protocol-line counterparty note
      (D-253), and the data-pipelines exploration (D-254 + seed design).
- [x] The engineering changelog records the vocabulary adoption and the generated story board agrees
      with frontmatter.

## Progress

- 2026-08-04: Completed the vocabulary reconciliation from flux-roadmap Decision 0006 in one
  documentation-only change: concepts/ecosystem (+ website mirrors), the three affected designs,
  both website datasource pages, the first-class-board epic with its design and re-pointed stories,
  five follow-up stories, the data-pipelines seed design, the roadmap narrative, the changelog entry
  and the regenerated board. No runtime, release, or public capability changed.
- 2026-08-04: Deferred verification (needs a heavy build, listed rather than run here):
  `cargo test -p codewandler-flux-lang --test website_in_sync`, `cargo test -p flux-cli --test
  website_contract`, and `scripts/build-embedded-docs.sh --check` (full website npm build).

## Notes

- Cross-repository source: `../flux-roadmap/decisions/0006-datasources-are-declared-read-surfaces.md`
  and the flux-roadmap ROADMAP "Datasources and boards" section. Follows the Milestone 0 adoption
  pattern C-508 established for Decision 0001.
- Documentation-only contract correction; no failing-first behavioral test applies. The website
  mirrors were spliced with the same body-extraction rule `website_in_sync.rs` checks.
- Implementation ordering is unchanged for Milestone 1: nothing here touches the first-run path.

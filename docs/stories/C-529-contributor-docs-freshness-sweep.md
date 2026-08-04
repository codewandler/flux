---
id: C-529
title: "Contributor docs freshness sweep: retire the compiler-thesis remnants and rotting status blocks"
pillar: Core
status: done
priority:
design:
epic:
areas: [docs]
note: "architecture.md/CONTRIBUTING.md/roadmap.md still teach the retired plan-compiler thesis; board and roadmap status prose rotted 13-17 releases; several small doc gaps."
---

# Contributor docs freshness sweep: retire the compiler-thesis remnants and rotting status blocks

## Goal

Make the contributor front doors tell the truth again: retire the "model emits a Flux-Lang plan"
thesis everywhere it survives, replace rotting hardcoded release-status prose with evergreen
pointers, and close the small documentation gaps found alongside (missing REPL command, broken
published link, unindexed projections, orphaned pages).

## Acceptance

- [x] `docs/architecture.md`, `CONTRIBUTING.md`, and `docs/roadmap.md`'s Direction section state
      the adaptive-loop thesis; a new failing-first
      `website_contract::contributor_entry_docs_do_not_revive_the_retired_compiler_thesis` test
      pins the exact retired sentences out and the corrected framing in.
- [x] `docs/roadmap.md`'s release-status preamble carries no hardcoded version claim (the
      codegate-pinned status line on line 3 stays untouched); its `../designs/` links are fixed
      and pinned out by the same test.
- [x] The hand-written Status block in `docs/stories/README.md` contains no release-version
      claims that rot; it points at CHANGELOG.md and the roadmap status line instead.
- [x] `/plugin-refresh <name>` appears in the website REPL command table; the README's published
      topologies link includes the `/docs/` segment — both pinned by website_contract.
- [x] `docs/language.md` points at the shipped Glyph and Railflux projections and documents the
      quoted-key field-path form; the agent-loop and a2a contributor/website pairs carry
      reciprocal pointer notes; `docs/designs/README.md` states the directory's convention;
      `docs/README.md` maps `zendesk-triage.md`; `AGENTS.md` points at the docs map.

## Progress

- 2026-08-05: Story opened from the approved improvement plan; implementation begins in the same
  session.
- 2026-08-05: Implemented and closed. The new contract test failed first against the stale docs
  (architecture.md's plan-compiler paragraph) and is green after the sweep; codegate's
  roadmap-status-line pin stayed green, confirming line 3 was untouched. The website build's
  broken-link gate covers the touched website pages; website contract 34/34.

## Notes

- The TUI clipboard documentation correction is deliberately excluded — untracked story C-526
  owns it.
- `docs/ecosystem.md` and `docs/concepts.md` are mirrored and already current; not touched here.
- The new contract test is scoped to the exact stale sentences (not broad retired-claim lists)
  because roadmap epic history may legitimately quote old phrasing.

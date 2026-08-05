---
id: C-578
title: "Agent-native Flux documentation — release-matched answers through a datasource (epic)"
pillar: Core
status: backlog
epic: agent-native-flux-docs
design: docs/designs/agent-native-flux-docs.md
areas: [flux-capabilities, flux-cli, flux-server, flux-skill]
note: "EPIC — Flux questions activate a bounded built-in docs datasource instead of relying on model memory, arbitrary checkout reads or web search"
---

# Agent-native Flux documentation — release-matched answers through a datasource

## Goal

Let every complete Flux agent discover, search and retrieve the documentation belonging to its own
release, including deterministic overviews for topics such as Agent-Loop, Flux-Lang and Board/Fleet.
Use the indexed datasource contract rather than inventing a documentation-only retrieval path.

## Acceptance

- [ ] C-579 publishes a deterministic `flux-docs` indexed datasource from the same release-matched
      documentation source as `flux docs`, with version/digest identity, bounded topic/page/section
      records and a freshness gate.
- [ ] C-580 makes the datasource available through the ordinary read-only datasource operations and
      dispatcher, including deterministic topic overview, search, exact get and relation traversal
      without arbitrary filesystem, process or network access.
- [ ] C-581 surfaces the docs group whenever intent is clearly about Flux itself or working with
      Flux, keeps unrelated turns quiet, and documents the explicit activation/discovery path.
- [ ] Agent-Loop, Flux-Lang and Board/Fleet end-to-end fixtures prove that a clean installed binary,
      with no source checkout and no network, finds the right topic, retrieves cited sections and
      reports the exact Flux release/corpus it used.
- [ ] Generated skills, public docs, changelogs, embedded-doc freshness and the full repository gate
      agree with the shipped operation names, schemas, limits, access kinds and authority subjects.

## Progress

- 2026-08-05 — epic and three delivery stories contracted; implementation has not started.

## Notes

- Depends on the accepted datasource boundary in flux-roadmap Decision 0006.
- This epic does not move Board back under the datasource vocabulary.

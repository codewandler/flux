---
id: C-579
title: "Build the release-matched Flux documentation datasource"
pillar: Core
status: backlog
epic: agent-native-flux-docs
design: docs/designs/agent-native-flux-docs.md
areas: [flux-datasource, flux-server, website]
note: "derive deterministic topic/page/section records from the same committed public-doc source and bind them to the Flux release and corpus digest"
---

# Build the release-matched Flux documentation datasource

## Goal

Turn the documentation already bundled with Flux into one deterministic indexed datasource artifact
whose records can be consumed without a checkout, server or network connection.

## Acceptance

- [ ] A failing-first fixture proves that the distributed binary currently has no registered
      `flux-docs` source even though `public-docs.zip` is present.
- [ ] The documentation build emits versioned `topic`, `page` and `section` records using the public
      datasource vocabulary, stable record identities, typed relations, normalized searchable text,
      the Flux version and a corpus digest.
- [ ] A reviewed topic manifest covers at least Agent-Loop, Flux-Lang, Board/Fleet, tools/operations,
      permissions/approvals, configuration, providers, SDK, plugins/connectors, datasources,
      sessions/durability and troubleshooting, with aliases and ordered starting pages.
- [ ] Duplicate ids, broken page/anchor relations, orphan topics, unbounded records and content not
      present in the committed public-doc source fail the build deterministically.
- [ ] `scripts/build-embedded-docs.sh --check`, website CI and the release path prove the datasource
      artifact and `public-docs.zip` were produced from the same committed source and cannot drift.

## Progress

- Not started.

## Notes

- Indexed mode is required: this is an in-process, release-fixed corpus with search and relations.
- Topic overviews are authored navigation records, not generated model summaries.

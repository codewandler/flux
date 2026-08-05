---
id: C-581
title: "Surface Flux documentation for questions about Flux itself"
pillar: Agent
status: backlog
epic: agent-native-flux-docs
design: docs/designs/agent-native-flux-docs.md
areas: [flux-tools, flux-cli, flux-skill, website]
note: "intent-aware activation for Flux commands, configuration, Agent-Loop, Flux-Lang, Board/Fleet, SDK and safety without polluting unrelated turns"
---

# Surface Flux documentation for questions about Flux itself

## Goal

Ensure an agent is offered the release-matched documentation operations when a request is clearly
about Flux or working with Flux, while keeping unrelated turns' operation vocabulary compact.

## Acceptance

- [ ] A failing-first intent corpus shows clear Flux-self questions currently proceed without the
      `flux-docs` retrieval group and establishes positive, negative and ambiguous cases.
- [ ] Normal intent/tool-group selection activates the docs retrieval vocabulary for Flux commands
      and configuration, Agent-Loop, Flux-Lang, Board/Fleet, operations, SDK, providers, permissions,
      plugins/connectors, datasources and troubleshooting; it uses topic metadata/aliases rather than
      one raw keyword switch.
- [ ] Unrelated uses of the word `flux`, repository files that merely mention Flux and ordinary
      coding requests do not activate the group without semantic evidence; explicit user selection
      or a request to consult Flux documentation always does.
- [ ] The activated group teaches the shortest bounded overview/search/get shapes and identifies
      `flux-docs` as release-matched; it injects no page body before an operation is called.
- [ ] Generated `flux skill` output, agent CLI/TUI help and public docs show how to discover and
      explicitly activate the datasource, and tests drift-guard the rendered operation names and
      schemas against the live registry.
- [ ] End-to-end model fixtures answer one Agent-Loop, one Flux-Lang and one Board/Fleet question
      from cited datasource records, while a newer-than-release question reports corpus identity and
      does not silently fall through to web access.

## Progress

- Not started.

## Notes

- Depends on C-580.
- Surfacing is not authorization: calls still pass through the normal dispatcher and datasource
  access policy.

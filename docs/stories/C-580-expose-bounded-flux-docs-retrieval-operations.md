---
id: C-580
title: "Expose bounded Flux documentation retrieval through datasource operations"
pillar: Core
status: backlog
epic: agent-native-flux-docs
design: docs/designs/agent-native-flux-docs.md
areas: [flux-capabilities, flux-cli, flux-runtime]
note: "register flux-docs in complete agent assemblies and reuse sources/search/get/list/relation/batch_get; topic overview is a projection, not a second backend"
---

# Expose bounded Flux documentation retrieval through datasource operations

## Goal

Make the release-matched `flux-docs` records safely usable by an agent through Flux's existing
indexed datasource contract, with concise topic overview and search journeys.

## Acceptance

- [ ] Failing-first assembly tests prove CLI, TUI, SDK Client and sub-agent paths do not currently
      expose the bundled documentation as the same `flux-docs` datasource.
- [ ] Every complete agent assembly registers `flux-docs` once in the unified datasource registry;
      `sources`, `search`, `get`, `list`, `relation` and `batch_get` return the same typed records and
      identities through `Executor::dispatch`.
- [ ] Search hit count, record text and total returned bytes are bounded; typed truncation carries
      stable continuation/record identities, and adversarial queries cannot select a path, URL,
      source outside `flux-docs` or content outside the embedded corpus.
- [ ] Topic overview resolves canonical ids and declared aliases, returns the exact topic record plus
      ordered related record identities, and delegates to the datasource rather than maintaining a
      separate summary or search implementation.
- [ ] Specs declare read-only datasource access and exact `datasource:flux-docs/<entity>/<id>`
      subjects; tests prove no filesystem, process, network, write or hidden approval bypass.
- [ ] Offline installed-binary fixtures retrieve Agent-Loop, Flux-Lang and Board/Fleet evidence and
      expose the Flux version/corpus digest in every result envelope.

## Progress

- Not started.

## Notes

- Depends on C-579.

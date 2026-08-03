---
id: C-506
title: "Remove plugin support and distribution from Flux"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "after the last adapter cutover, delete plugin host/install/index and every archive/sign/upload path; CI rejects their return"
---

# Remove plugin support and distribution from Flux

## Goal

Finish the migration by removing plugin execution, installation and distribution from Flux, so its
binary and release pipeline contain only the embedded Exchange client and no additional integration
artifact path.

## Acceptance

- [ ] Plugin host/runtime crates, `host-kit`, `pack-index`, installer commands, configuration and
      documentation are deleted rather than renamed into a connector compatibility path.
- [ ] Flux release workflows build, archive, sign and upload no plugin binary, pack, index or other
      official integration artifact; obsolete secrets and release inputs are removed.
- [ ] Connector/runtime artifacts are built and distributed only by the connector/Exchange pipeline,
      including any temporary framed-stdio implementation used behind Exchange.
- [ ] CI scans the Flux workspace, release workflows and public docs and rejects reintroduced plugin
      support, plugin installation, or official integration artifacts.
- [ ] Core language, agent, SDK and built-in-tool releases remain usable with Exchange absent; the
      unavailable capability is official external integration execution.

## Progress

- (not started)

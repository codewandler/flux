---
id: C-505
title: "Retire every official native integration crate after Exchange proof"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "remove each of the 18 vendor adapters when its Exchange replacement passes C-504; no local fallback and no big-bang deletion"
---

# Retire every official native integration crate

## Goal

Remove vendor-specific integration implementations from `plugins/` one at a time as their connector
replacements become conformant through Exchange, leaving no second official catalogue or fallback in
Flux.

## Acceptance

- [ ] The checked inventory matches flux-connectors C-505 exactly: collaboration, infrastructure,
      observability, data/secrets and remaining-adapter waves account for all eighteen crates.
- [ ] Each deletion lands only after a published replacement, parity evidence, stable replacement
      addresses and migration notes; catalogue presence alone is insufficient.
- [ ] Examples, skills, docs and defaults move to Exchange-backed connector operations without
      silently changing granted effects or credential placement; plugin install commands are removed.
- [ ] The official integration adapter count reaches zero and a gate prevents a vendor-specific crate
      from being reintroduced outside flux-connectors.
- [ ] Deleted adapters cannot be reached through a local connector host, retained plugin fallback, or
      Flux-owned runtime artifact; temporary connector runtime artifacts execute only behind Exchange.

## Progress

- (not started)

## Notes

- D-249's Asterisk removal is the completed incremental cutover model. C-506 removes the remaining
  plugin support and release machinery after the final adapter is gone.

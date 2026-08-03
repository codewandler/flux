---
id: C-505
title: "Retire every official native integration crate after connector parity"
pillar: Core
status: backlog
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "remove the 18 vendor adapters only after flux-connectors C-499…C-503 publish and pass C-504 locality conformance; no big-bang deletion"
---

# Retire every official native integration crate

## Goal

Remove vendor-specific integration implementations from `plugins/` as their connector replacements
become conformant, leaving Flux with generic runtime/protocol support rather than a second official
catalogue.

## Acceptance

- [ ] The checked inventory matches flux-connectors C-505 exactly: collaboration, infrastructure,
      observability, data/secrets and remaining-adapter waves account for all eighteen crates.
- [ ] Each deletion lands only after a published replacement, parity evidence, stable replacement
      addresses and migration notes; a connector entry alone is insufficient.
- [ ] Examples, skills, docs, default features and install commands move to connector bundles without
      silently changing granted effects or credential placement.
- [ ] The official integration adapter count reaches zero and a gate prevents a vendor-specific crate
      from being reintroduced outside flux-connectors.
- [ ] The generic stdio plugin protocol remains available for connector runtime artifacts and external
      extensions unless C-506 deliberately replaces it.

## Progress

- (not started)

## Notes

- D-249's Asterisk removal is the completed cutover model. D-214/D-220 and the pending generated
  connector channel work are partial migrations to reuse.

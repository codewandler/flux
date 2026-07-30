---
id: C-283
title: Export the core catalogue as versioned JSON specifications
pillar: Core
status: done
epic: core-catalogue
design: docs/designs/core-catalogue.md
areas: [cli, flux-lang, flux-spec, website]
note: "One deterministic export joins real ToolSpecs to the generated Flux-Lang schema and gives every core record a dereferenceable JSON identity"
---

# Export the core catalogue as versioned JSON specifications

## Goal

Give downstream explorers one offline, deterministic, machine-readable source for Flux's foundational
operations, language nodes, and declared capability availability without building a parallel registry.

## Acceptance

- [x] A failing-first CLI test fixes `flux catalog core --format json`, schema version 1, stable
      ordering, and byte-for-byte deterministic output.
- [x] The export selects exactly `http.request` plus the 28 agreed pure transforms from the real
      registry and preserves each serialized `ToolSpec` without executing it.
- [x] Every Flux-Lang node is derived from the strict AST schema, and the published AST projection
      gives each node kind a stable `#node-<kind>` anchor.
- [x] Catalogue and entry JSON Schemas are generated from the wire types; every record has a unique
      canonical `https://flux.codewandler.org/v1/core/...json` `$id` and validates.
- [x] HTTP is available and linked to `http.request`; DNS, TCP, UDP, and ICMP are planned,
      non-callable capabilities with no fabricated operation names or schemas.
- [x] `return` is a language node, `noop` is absent, and command/docs/changelog/what's-new surfaces
      explain those distinctions.
- [x] The full repository gate passes, including the website/schema drift checks.

## Progress

- Added the deterministic offline CLI export over the real tool registry and Flux-Lang schema, with
  generated wire schemas, 29 operations, 43 nodes, and five capability records.
- The export validates its generated catalogue/entry documents against those schemas; its output is
  byte-identical to the snapshot consumed by flux-connectors C-112.
- Full workspace build/test/clippy/fmt/codegate and the Docusaurus production build pass.

## Notes

- The JSON URLs are schema/specification identities, not runtime invocation names. Existing
  `ToolSpec::name` values remain the callable spelling.

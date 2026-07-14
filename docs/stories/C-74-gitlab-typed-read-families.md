---
id: C-74
title: Bind GitLab project, merge-request, and issue reads to typed handlers
pillar: Core
status: done
epic: typed-plugin-migration
design: plugins/TYPED-MIGRATION.md
note: bounded first GitLab migration unit from the typed-plugin handler matrix
---

# Bind GitLab project, merge-request, and issue reads to typed handlers

## Goal

Make the bounded `project.list/show`, `mr.list/show`, and `issue.list/show` families use GitLab
input and output types as their executable contracts. Preserve the existing raw GitLab array/object
wire shapes and every unknown vendor field while publishing truthful schemas for the stable fields
flux consumes.

## Acceptance

- [x] The six operations register through `PluginBuilder::operation_typed`; their handlers accept
      typed inputs and never repeat the input contract with `serde_json::Value` extraction.
- [x] Input structs derive `Deserialize + Serialize + JsonSchema`, reject unknown fields, retain the
      documented aliases/defaults, and keep address preflight and live execution on one resolver.
- [x] Successful list results remain JSON arrays and show results remain JSON objects. Stable fields
      used by flux are represented in generated output schemas, while open GitLab extensions and
      explicit `null` values survive byte-for-byte JSON-value round trips.
- [x] Hermetic handler tests pin request construction, contribution behavior, raw vendor-field
      preservation, path-aware input failures, and manifest input/output schemas.
- [x] `plugins/TYPED-MIGRATION.md` records this completed migration unit and explains why the next
      open GitLab result families remain on `operation_flexible`.
- [x] The GitLab plugin build, tests, clippy, formatting, and guest dependency boundary are green.

## Notes

- This is the first bounded GitLab output-contract batch after
  [C-68](C-68-typed-plugin-handlers-output-schemas.md). Mutation responses, streams, diffs, and
  other vendor-specific result families remain separate migration units.
- The wire contract is intentionally not changed to a flux-owned `{ items, count }` envelope: GitLab
  callers already consume the vendor's top-level arrays and objects.

## Progress

- Six bounded read families now decode typed inputs once and return transparent map-backed output
  types. The failing-first output-schema/result-shape tests pass with 91 GitLab tests, build,
  clippy `-D warnings`, formatting, the guest dependency boundary, and the manual-schema guard.

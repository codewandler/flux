---
id: C-68
title: Bind plugin schemas to typed handlers and outputs
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: first-party plugin input structs derive schemas but handlers still parse Value through a second contract
---

# Bind plugin schemas to typed handlers and outputs

## Goal

Make a plugin operation's Rust input/output types the executable contract, not schema-only dead code
beside manual `serde_json::Value` extraction.

## Acceptance

- [x] Host-kit provides typed registration (for example `operation_typed<I, O>`) that derives input
      and output schemas, deserializes input exactly once, invokes a typed handler, and serializes the
      typed result.
- [x] Schema validation/runtime deserialization share one error contract, including path-aware field
      errors; a failing-first drift test cannot make the schema accept a shape the handler rejects or
      vice versa.
- [x] An explicit flexible/legacy adapter supports intentional aliases and open payloads without
      making manual parsing the default; preflight and live dispatch use the same normalized input.
- [x] Representative simple and flex-heavy operations migrate first, followed by a documented
      migration matrix for every first-party plugin; migrated operations remove their schema-only
      `#[allow(dead_code)]` structs/manual extractors where no longer needed.
- [x] The migrated representative stable list/get/show result families adopt D-164 output schemas
      through their typed outputs; generated manifest/catalog sync tests prevent drift.
- [x] Shared helpers are extracted only for transport-neutral behavior; vendor-specific semantics
      stay inside each plugin, and every existing contract fixture remains green.

## Progress

- 2026-07-14 — Added typed input/output registration with path-aware decode errors and made flexible
  parsing explicit. The phased representative cutover covers websearch's simple `provider.list` and
  flex-heavy `search` plus Jira attachment list/get; all remaining first-party handlers are
  explicitly `operation_flexible` and are tracked in `plugins/TYPED-MIGRATION.md`.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Builds on [D-36](D-36-schemars-plugin-op-schemas.md) and
  [D-164](D-164-plugin-operation-output-schemas.md); D-36 removed hand-written schema literals but
  deliberately left handlers on manual `Value` extraction.
- Sequence after C-64. Coordinate the guest API with C-69 before mass migration.

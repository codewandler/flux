---
id: D-164
title: Preserve plugin operation output schemas
pillar: Agent
status: done
priority:
epic: plugin-output-schemas
design: docs/designs/plugin-output-schemas.md
note:
---

# Preserve plugin operation output schemas

## Goal
Let plugin authors declare the JSON shape an operation returns and make that declaration available to every
host/catalog consumer.

## Acceptance
- [x] A failing-first manifest serde test round-trips optional `output_schema` without breaking old manifests
      (`operation_output_schema_round_trips_and_defaults_for_legacy_manifests`, `crates/flux-plugin`).
- [x] A failing-first projection test proves `PluginTool::spec().output_schema` matches the manifest
      (`plugin_operation_output_schema_projects_to_tool_spec`, `crates/flux-plugin`).
- [x] Authoring/reference docs describe the field and the workspace gate stays green — the
      `with_output_schema(op, schema)` host-kit combinator (mirroring `grouped`/`risked`) plus
      `PluginBuilder::map_operations` give authors a tested way to set it; AUTHORING.md documents both.

## Progress
- Started 2026-07-12 for ai-agent-platform's live capability reference.
- Done 2026-07-12. Landed unreleased (pending the next MINOR, cut with D-165).

## Notes
- **Authoring surface (added post-review):** the initial cut told authors to "set `OperationSpec.output_schema`"
  with no setter, and left the kubernetes plugin's exhaustive `OperationSpec` literal un-updated (a `plugins/`
  workspace compile break, since the field is not `..Default`-filled there). Both fixed: `with_output_schema` +
  `map_operations` combinators, and the missing `output_schema: None` in `kubernetes`'s `op_spec_typed`.
- **SemVer:** additive only (a serde-defaulted optional field + new host-kit combinators) → would be a patch on
  its own; ships in the D-165 MINOR.

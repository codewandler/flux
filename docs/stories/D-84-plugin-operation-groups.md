---
id: D-84
title: Plugin-authored operation groups
pillar: Agent
status: done
design: docs/designs/secret-management-plugins.md
epic: secret-management-plugins
note: "Plugin manifests carry group definitions + per-op group tags; projected tools preserve them so plugins can organize ops without per-workspace groups.toml."
---

# Plugin-authored operation groups

## Goal
Let plugin manifests declare operation groups that travel with the plugin pack and project into the
model-facing tool catalog.

## Acceptance
- [x] `OperationSpec` carries optional group metadata and old manifests still deserialize.
- [x] `PluginManifest` carries plugin-authored `ToolGroup` definitions.
- [x] `PluginTool::new` maps the op group into `ToolSpec.group`.
- [x] Loaded plugin groups are merged into the agent's runtime group list.
- [x] Failing-first test: a grouped plugin op is projected with its group and remains advertised when
      the plugin declares a force-on group.

## Progress
- Done in this session. `flux-plugin` covers manifest compatibility and operation group projection.

## Notes
- Built-in groups already exist for first-party tools; this story extends that capability to native
  plugin manifests.

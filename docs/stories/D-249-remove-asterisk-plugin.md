---
id: D-249
title: "Remove Asterisk from Flux after moving ARI to connectors"
pillar: Agent
status: in-progress
priority: 1
design: docs/designs/asterisk-ari.md
epic: asterisk-ari
areas: [plugins, flux-plugin]
note: "owner correction — delete AMI and ARI plugin completely; unwind Asterisk-only WebSocket/blob host APIs"
---

# Remove Asterisk from Flux after moving ARI to connectors

## Goal

Remove the entire Asterisk plugin and the generic plugin-host surface introduced only for its
mistaken ARI implementation. Asterisk ARI is an API-spec-generated REST connector owned by
`flux-connectors`; Flux retains no AMI remnant.

## Acceptance

- [x] A failing-first inventory test proves the Asterisk plugin is still a workspace member before
      deletion and absent afterwards.
- [x] All 30 tracked files under `plugins/asterisk/` and every active registration, smoke-test,
      documentation and distribution reference are removed; historical changelog/story evidence is
      preserved and marked superseded where it otherwise states current ownership.
- [x] The plugin WebSocket capability and HTTP-response-to-blob helper introduced solely for
      Asterisk are removed from protocol, host and host-kit; generic connection, guarded HTTP, blob
      storage and bounded response reading remain.
- [x] Protocol/host-kit/plugin-pack versions follow their independent breaking-release contract and
      lockfiles/goldens are regenerated.
- [ ] Root and nested-plugin gates pass, engineering/customer changelogs explain the correction, and
      corrective core and plugin-pack releases are cut and watched green.
- [x] The user-owned dirty `docs/stories/C-163-plugin-commands-and-host-ui.md` in the primary checkout
      is never touched; all work runs from the isolated clone at exact base `1f6146ea`.

## Progress

- 2026-08-02: owner explicitly rejected both ARI and AMI ownership in Flux and directed that the
  entire plugin be deleted. Eventing is deferred to future connector channel design.
- 2026-08-02: `git ls-files plugins/asterisk | wc -l` measured 30 tracked files. Production-caller
  searches found no non-Asterisk consumer of the plugin WebSocket or HTTP-response-to-blob helpers.
- 2026-08-02: failing-first inventory and protocol tests were observed red before deletion. The
  final pack has 18 plugin binaries across 20 nested-workspace packages; Asterisk's 30 files and
  empty directory skeleton are absent.
- 2026-08-02: source-breaking removals move `codewandler-flux-plugin-protocol` and host-kit to
  2.0.0 and the pack to 0.2.0. The wire marker deliberately remains `flux.plugin.v1`: serde ignores
  the retired optional key, and the compatibility test proves old non-Asterisk manifests load.

## Notes

- Do not revert the normal capped HTTP response reader; it has independent safety value.
- The source bytes are re-vendored from Asterisk's first-party repository by `flux-connectors`, not
  moved as Flux-owned API code.

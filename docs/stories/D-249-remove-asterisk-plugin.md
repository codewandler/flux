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

- [ ] A failing-first inventory test proves the Asterisk plugin is still a workspace member before
      deletion and absent afterwards.
- [ ] All 30 tracked files under `plugins/asterisk/` and every active registration, smoke-test,
      documentation and distribution reference are removed; historical changelog/story evidence is
      preserved and marked superseded where it otherwise states current ownership.
- [ ] The plugin WebSocket capability and HTTP-response-to-blob helper introduced solely for
      Asterisk are removed from protocol, host and host-kit; generic connection, guarded HTTP, blob
      storage and bounded response reading remain.
- [ ] Protocol/host-kit/plugin-pack versions follow their independent breaking-release contract and
      lockfiles/goldens are regenerated.
- [ ] Root and nested-plugin gates pass, engineering/customer changelogs explain the correction, and
      corrective core and plugin-pack releases are cut and watched green.
- [ ] The user-owned dirty `docs/stories/C-163-plugin-commands-and-host-ui.md` in the primary checkout
      is never touched; all work runs from this clean clone at `41fc0777`.

## Progress

- 2026-08-02: owner explicitly rejected both ARI and AMI ownership in Flux and directed that the
  entire plugin be deleted. Eventing is deferred to future connector channel design.
- 2026-08-02: `git ls-files plugins/asterisk | wc -l` measured 30 tracked files. Production-caller
  searches found no non-Asterisk consumer of the plugin WebSocket or HTTP-response-to-blob helpers.

## Notes

- Do not revert the normal capped HTTP response reader; it has independent safety value.
- The source bytes are re-vendored from Asterisk's first-party repository by `flux-connectors`, not
  moved as Flux-owned API code.

---
id: C-485
title: "Keep removed Asterisk plugin surfaces removed"
pillar: Core
status: done
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-system, flux-channels]
note: "v0.52.0 already removed ARI and AMI under D-249; generic channel work must not resurrect either plugin surface"
---

# Keep removed Asterisk plugin surfaces removed

## Goal

Reconcile the generated-channel program with D-249's already-published v0.52.0 removal so extracting
a reusable WebSocket guard does not accidentally restore either Asterisk plugin surface.

## Acceptance

- [x] Flux remains free of Asterisk ARI and AMI plugin registration, sources and generated pack
      entries at the v0.52.1 baseline.
- [x] C-483 extracts only the generic guarded session; no connector or Asterisk-specific policy is
      added to `flux-system`.
- [x] The superseded Asterisk design and the master design state the already-shipped removal and do
      not promise a compatibility release that cannot exist after publication.
- [x] A future AMI adapter and stored-recording binary/blob support remain separately designed gaps,
      not reasons to resurrect the removed plugin.
- [x] Root tests, clippy and format are green without a nested Asterisk plugin workspace.

## Progress

- 2026-08-02: baseline corrected from the plan's stale premise after `git show v0.52.0` and the
  v0.52.1 tree confirmed D-249's removal. The stash conflict in `flux-plugin/src/host.rs` was
  resolved to the v0.52.1 version so none of the retired implementation returned.

- 2026-08-03: the v0.53-based root gate, codegate and both sandbox postures pass with only the
  connector-neutral session and driver in the tree; no nested Asterisk workspace or pack returned.

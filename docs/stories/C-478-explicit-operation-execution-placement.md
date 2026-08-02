---
id: C-478
title: "Execution placement is operation metadata — replace remote mode's registry-diff denylist"
pillar: Core
status: ready
priority: 5
epic: remote-agents
design: docs/designs/remote-agents.md
areas: [flux-runtime, flux-cli, flux-plugin]
note: "remote assembly currently disables whole registry diffs after each outer pack; safe, but too blunt to ever make mixed local-control/selected-system catalogs complete"
---

# Execution placement is operation metadata

## Goal

Make each operation's valid execution placement explicit so local mode stays unchanged, remote mode
can surface every operation whose effects use the selected system, and native-only operations remain
fail-closed without hard-coded registration-order diffs.

## Acceptance

- [ ] Failing-first: two operations registered by the same outer pack declare different placement;
      remote assembly surfaces the selected-system operation and refuses the native-only one. The
      current “disable everything added since the registry snapshot” behavior fails this test.
- [ ] One typed placement vocabulary distinguishes at least: local control-plane work,
      selected-execution-system effects, and native-system-only effects. The location of this
      metadata respects crate layering; it is not smuggled into effect or risk fields.
- [ ] The safe default for an unannotated third-party/outer operation is native-only under a remote
      target. Local mode remains byte-for-byte unchanged.
- [ ] Every production built-in and outer pack is classified. A census test fails when a newly
      registered public operation has no deliberate placement decision.
- [ ] Remote refusal is defense in depth at dispatch as well as catalog filtering; a cached plan or
      direct call cannot execute a hidden native-only op locally.
- [ ] `/tools`, CLI diagnostics, or an equivalent operator surface explains why an operation is
      unavailable under the selected target.
- [ ] The blanket `before_*`/registry-diff remote disable blocks in `execution.rs` are removed.
- [ ] Public execution-model docs derive or verify their compatibility categories against the same
      metadata rather than maintaining an unrelated list.
- [ ] Full gate green in both sandbox postures.

## Progress

- Filed 2026-08-02 from C-477's execution-placement audit.

## Notes

- This is compatibility metadata, not authorization. Every surfaced operation still traverses
  authorization → approval → guarded IO.
- [C-479](C-479-plugins-on-the-selected-execution-system.md) consumes this vocabulary for plugin
  operations whose host callbacks can be served remotely.

---
id: C-478
title: "Execution placement is operation metadata — replace remote mode's registry-diff denylist"
pillar: Core
status: done
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

- [x] Failing-first: two operations registered by the same outer pack declare different placement;
      remote assembly surfaces the selected-system operation and refuses the native-only one. The
      current “disable everything added since the registry snapshot” behavior fails this test.
- [x] One typed placement vocabulary distinguishes at least: local control-plane work,
      selected-execution-system effects, and native-system-only effects. The location of this
      metadata respects crate layering; it is not smuggled into effect or risk fields.
- [x] The safe default for an unannotated third-party/outer operation is native-only under a remote
      target. Local mode remains byte-for-byte unchanged.
- [x] Every production built-in and outer pack is classified. A census test fails when a newly
      registered public operation has no deliberate placement decision.
- [x] Remote refusal is defense in depth at dispatch as well as catalog filtering; a cached plan or
      direct call cannot execute a hidden native-only op locally.
- [x] `/tools`, CLI diagnostics, or an equivalent operator surface explains why an operation is
      unavailable under the selected target.
- [x] The blanket `before_*`/registry-diff remote disable blocks in `execution.rs` are removed.
- [x] Public execution-model docs derive or verify their compatibility categories against the same
      metadata rather than maintaining an unrelated list.
- [x] Full gate green in both sandbox postures.

## Progress

- Filed 2026-08-02 from C-477's execution-placement audit.
- 2026-08-05: implementation started on the dispatched post-C-205 wave.
- 2026-08-05: added `OperationPlacement` metadata and fail-closed registry defaults; remote catalog
  filtering and the dispatch gate now decide compatibility per operation, with `/tools` carrying
  the exact refusal reason. Removed the registration-order snapshot/diff blocks and classified the
  complete production catalog behind a census test.
- 2026-08-05: failing-first evidence replaced the placement lookup with the former blanket
  native-only decision: `remote_placement_filters_and_refuses_each_operation_independently` failed
  because the selected-system member of one mixed pack disappeared. Restoring the per-operation
  lookup made that case and the unannotated/local-mode compatibility case pass.
- 2026-08-05: targeted verification passed for runtime (204 tests), tools (216), plugin (184 passed,
  1 ignored), CLI (429), and the remaining directly touched packages (1,032 passed, 5 ignored),
  plus focused all-target clippy with warnings denied and `cargo fmt --all -- --check`. The wave
  coordinator owns the one combined full gate in both sandbox postures before shipping.
- 2026-08-05: the integrated wave passed `scripts/release-full-gate.sh` (workspace build and tests,
  all-target clippy with warnings denied, both-workspace formatting, and 51 codegate tests), the
  complete workspace suite with `FLUX_BWRAP_BIN=/nonexistent/bwrap`, and all five
  `sandbox_backend` integration tests with `FLUX_TEST_SANDBOX_BACKEND=1`.

## Notes

- This is compatibility metadata, not authorization. Every surfaced operation still traverses
  authorization → approval → guarded IO.
- [C-479](C-479-plugins-on-the-selected-execution-system.md) consumes this vocabulary for plugin
  operations whose host callbacks can be served remotely.

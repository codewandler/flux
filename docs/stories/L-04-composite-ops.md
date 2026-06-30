---
id: L-04
title: Define custom ops by composing Flux-Lang
pillar: Language
status: done
priority:
design: docs/designs/composite-ops.md
---

# Define custom ops by composing Flux-Lang

## Goal
Let `.flux` modules define reusable custom operations by chaining existing ops, process execution,
model calls, and context-pack control without bypassing the runtime safety envelope.

## Acceptance
- [x] `op` declarations parse into `Program` and round-trip through serde.
- [x] A flow can call a module-local composite op with typed params and receive its returned value.
- [x] Composite locals are scoped and do not leak into the caller's symbol view.
- [x] Inner calls still dispatch through `Executor::dispatch`, including process ops.
- [x] Static analysis rejects unknown composites, recursive composites, `await` inside composites, and
      declared metadata that understates transitive effects/risk.
- [x] A new argv-only `proc.run` tool executes through `flux_system::System` and is shell-group gated.
- [x] SDK, CLI flow execution, and `flux-app` can load module composites.

## Progress
- Done. Added `op` declarations to native modules, composite-aware catalogs/runtime execution, SDK/CLI/app
  wiring, an argv-only `proc.run` op, docs, and regression tests.

## Notes
- Composite ops are language/runtime sub-flows, not plugins and not `flux_runtime::Tool`s.

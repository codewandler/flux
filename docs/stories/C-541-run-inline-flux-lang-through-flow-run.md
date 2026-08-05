---
id: C-541
title: Run inline Flux-Lang through flow_run
pillar: Core
status: done
priority: 2
epic:
design: docs/designs/composite-ops.md
areas: [flux-tools, flux-lang, docs]
note: "Let an agent execute supplied Flux-Lang without first writing a workspace file, while preserving the normal execution envelope."
---

# Run inline Flux-Lang through flow_run

## Goal

Allow `flow_run` to accept Flux-Lang source directly for ephemeral composed work without turning
inline source into a shortcut around parsing, analysis, authorization, approval, guarded IO, or
session recording.

## Acceptance

- [x] `flow_run` accepts exactly one of a stored name, workspace path, or `inline_program` and
      rejects missing or ambiguous addresses.
- [x] Inline source is parsed and revalidated against the live operation catalogue immediately
      before execution.
- [x] Inline execution uses the same inputs, session, approval, guarded-IO, reentry, and runtime
      boundaries as stored and path-addressed flows.
- [x] Route receipts identify the inline address without claiming a resolved filesystem path.
- [x] Unit tests cover successful inline execution and mutually exclusive addressing.
- [x] Targeted `flux-tools` tests pass on the integrated main tree.

## Progress

- 2026-08-05: Contract recovered during local-main integration after the implementation had landed
  in a bundled local commit. The code and documentation already satisfied the functional acceptance;
  the integrated-tree targeted test remained before closure.
- 2026-08-05: Closed after
  `cargo test -p codewandler-flux-tools flow_run_executes_an_inline_program --lib` passed on the
  reconciled local `main`.

## Notes

- The inline address is an execution input, not persistence. Agents that need a durable artifact
  should still write a checked `.flux` file through the normal repository workflow.
- Design: [composite operations](../designs/composite-ops.md) and
  [Flux flow](../designs/flux-flow.md).

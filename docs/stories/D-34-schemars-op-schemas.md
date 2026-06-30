---
id: D-34
title: schemars-derive every operation schema (kill hand-written JSON Schema)
pillar: Core
status: done
priority:
design:
---

# schemars-derive every operation schema

## Goal

No in-process `ToolSpec` operation may hand-write its JSON Schema: every op's `input_schema`
must be derived from a typed Rust struct via `schemars` (`flux_spec::tool_input_schema::<T>()`),
so the schema and the runtime parsing cannot drift. This is the engineering work the agent in
session `s_249` failed to deliver (separately root-caused: the empty-parallel-branch bug, fixed
in `453379e`).

Unblocked by **L-09** (named-argument calls): `x-param-order` is gone and `required` is a set,
so `schemars`-derived schemas (which don't emit `x-param-order`) bind correctly.

## Acceptance

- [x] `rg "input_schema:\s*(serde_json::)?json!\(\{" crates -g '*.rs'` returns **no** op-declaration
  hits (only test asserts remain) across `flux-tools`, `flux-eval`, `flux-orchestrate`.
- [x] Every migrated op's schema comes from `tool_input_schema::<T>()`; handlers either parse via
  `parse_params` (full SSoT) or stay ad-hoc with a documented `#[allow(dead_code)]` struct
  (schema-only, matching the flux-eval precedent).
- [x] `op.register`'s schema↔handler drift is resolved (one `JsonSchema` definition shared by
  schema + runtime, or documented deferral).
- [x] A **regression guard** test (`crates/flux-tools/tests/no_manual_schema.rs`) fails on a
  deliberately reintroduced `input_schema: json!({...})` and passes on the migrated tree.
- [x] A **drift report** (`DRIFT.md`) records every schema↔handler mismatch found + fixed.
- [x] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.

## Scope

In-process `ToolSpec` ops only (~36 sites across `flux-tools`, `flux-eval`, `flux-orchestrate`).
The **275 plugin `OperationSpec` ops** are deferred — tracked separately (host-kit needs
`read_op_typed`/`write_op_typed` helpers; each plugin crate needs `schemars`).

## Progress

Done. All in-process `ToolSpec` ops derive their schema via `tool_input_schema::<T>()`;
`crates/flux-tools/tests/no_manual_schema.rs` guards it; `DRIFT.md` records the drifts.
The 275 plugin `OperationSpec` ops remain deferred (separate story).

## Notes

- Pattern + precedent established: see `WriteInput`/`WriteTool` (full SSoT) and the flux-eval
  structs (`#[allow(dead_code)]` schema-only).
- Out of scope: `flux-lang/opspec.rs` (the composite-op schema *generator*), provider `json!`
  test/MCP-passthrough sites.
